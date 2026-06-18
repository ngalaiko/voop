use bt_hci::cmd::SyncCmd;
use embassy_executor::Spawner;
use embassy_nrf::{mode::Blocking, Peri, peripherals, rng};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use nrf_sdc::{self as sdc, mpsl};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::vendor::ZephyrReadStaticAddrs;
use static_cell::StaticCell;
use trouble_host::prelude::*;

const CSC_SERVICE: Uuid = Uuid::new_short(0x1816);
const CSC_MEASUREMENT: Uuid = Uuid::new_short(0x2A5B);

#[derive(Debug, Clone)]
pub struct State {
    pub cumulative_crank_revs: Option<u16>,
    pub last_crank_event_time: Option<u16>, // 1/1024 s units
}

static STATE: Mutex<CriticalSectionRawMutex, Result<Option<State>, Error>> =
    Mutex::new(Ok(None));

pub async fn read() -> Result<Option<State>, Error> {
    STATE.lock().await.clone()
}

// Signals the address of the first CSC sensor spotted during scanning.
static SENSOR_ADDR: Signal<CriticalSectionRawMutex, Address> = Signal::new();

#[derive(Debug, Clone)]
pub enum Error {
    SpawnError(embassy_executor::SpawnError),
    SdcError(sdc::Error),
    MpslError(mpsl::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnError(e) => write!(f, "spawn error: {:?}", e),
            Error::SdcError(e) => write!(f, "SDC error: {:?}", e),
            Error::MpslError(e) => write!(f, "MPSL error: {:?}", e),
        }
    }
}

impl core::error::Error for Error {}

type MyController = nrf_sdc::SoftdeviceController<'static>;

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

// Parses BLE AD structures to find a specific 16-bit service UUID.
fn ad_has_uuid16(data: &[u8], target: u16) -> bool {
    let mut i = 0;
    while i < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        let ad_type = data[i + 1];
        // 0x02 = Incomplete 16-bit UUID list, 0x03 = Complete 16-bit UUID list
        if ad_type == 0x02 || ad_type == 0x03 {
            let uuid_data = &data[i + 2..i + 1 + len];
            let mut j = 0;
            while j + 1 < uuid_data.len() {
                if u16::from_le_bytes([uuid_data[j], uuid_data[j + 1]]) == target {
                    return true;
                }
                j += 2;
            }
        }
        i += 1 + len;
    }
    false
}

// Called by the RxRunner for each batch of extended advertising reports.
struct CscEventHandler;

impl EventHandler for CscEventHandler {
    fn on_ext_adv_reports(&self, reports: LeExtAdvReportsIter<'_>) {
        for report in reports.filter_map(|r| r.ok()) {
            if ad_has_uuid16(report.data, 0x1816) {
                SENSOR_ADDR.signal(Address::new(report.addr_kind, report.addr));
            }
        }
    }
}

#[embassy_executor::task]
async fn ble_task(controller: MyController) {
    // Read the SDC's factory-assigned random static address so trouble-host uses
    // AddrKind::RANDOM (not PUBLIC, which the nRF52840 doesn't have).
    let random_addr = match ZephyrReadStaticAddrs::new().exec(&controller).await {
        Ok(r) => Address::new(AddrKind::RANDOM, r.addr.addr),
        Err(e) => {
            log::error!("[BLE] failed to read static addr: {:?}", e);
            return;
        }
    };

    static RESOURCES: StaticCell<HostResources<MyController, DefaultPacketPool, 1, 1>> =
        StaticCell::new();
    let resources = RESOURCES.init(HostResources::new());
    let stack = trouble_host::new(controller, resources)
        .set_random_address(random_addr)
        .build();

    let mut runner = stack.runner();
    let (runner_result, _) = embassy_futures::join::join(
        runner.run_with_handler(&CscEventHandler),
        cadence_loop(&stack),
    )
    .await;
    if let Err(e) = runner_result {
        log::error!("[BLE] runner error: {:?}", e);
    }
}

async fn cadence_loop(stack: &Stack<'_, MyController, DefaultPacketPool>) {
    loop {
        SENSOR_ADDR.reset();

        // Scan for any device advertising the CSC service UUID (0x1816).
        // scan_ext with empty filter_accept_list scans all devices.
        log::info!("[BLE] Scanning for CSC sensor...");
        let central = stack.central();
        let mut scanner = Scanner::new(central);
        let scan_config = ScanConfig {
            filter_accept_list: &[],
            ..Default::default()
        };

        let session = match scanner.scan_ext(&scan_config).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[BLE] scan error: {:?}", e);
                continue;
            }
        };

        // The RxRunner will call CscEventHandler::on_ext_adv_reports as packets arrive.
        // Block here until a CSC device is signalled.
        let addr = SENSOR_ADDR.wait().await;
        drop(session); // Stops the scan (ScanSession RAII guard cancels on drop).
        // Yield so the runner can send LeSetExtScanEnable(false) to the controller
        // before we issue LeExtCreateConn — the SDC rejects connect while scan is active.
        embassy_futures::yield_now().await;

        log::info!("[BLE] Found CSC sensor, connecting...");
        let mut central = scanner.into_inner();
        let connect_config = ConnectConfig {
            connect_params: Default::default(),
            scan_config: ScanConfig {
                filter_accept_list: &[addr],
                ..Default::default()
            },
        };

        // connect_ext keeps us in "extended" HCI mode, consistent with scan_ext above.
        let conn = match central.connect_ext(&connect_config).await {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[BLE] connect error: {:?}", e);
                continue;
            }
        };
        log::info!("[BLE] Connected");

        let client: GattClient<'_, MyController, DefaultPacketPool, 4> =
            match GattClient::new(stack, &conn).await {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[BLE] GattClient error: {:?}", e);
                    continue;
                }
            };

        let (task_result, _) = embassy_futures::join::join(client.task(), async {
            let services = match client.services_by_uuid(&CSC_SERVICE).await {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("[BLE] service discovery error: {:?}", e);
                    return;
                }
            };
            let service = match services.first() {
                Some(s) => s.clone(),
                None => {
                    log::info!("[BLE] No CSC service found on this device");
                    return;
                }
            };

            let characteristic: Characteristic<[u8]> =
                match client.characteristic_by_uuid(&service, &CSC_MEASUREMENT).await {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!("[BLE] characteristic error: {:?}", e);
                        return;
                    }
                };

            let mut listener = match client.subscribe(&characteristic, false).await {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("[BLE] subscribe error: {:?}", e);
                    return;
                }
            };

            log::info!("[BLE] Subscribed to CSC measurement");
            loop {
                let notif = listener.next().await;
                let data = notif.as_ref();
                // CSC Measurement flags byte: bit 1 = crank revolution data present.
                // When set: bytes 1-2 = cumulative crank revolutions (u16 LE),
                //           bytes 3-4 = last crank event time (u16 LE, 1/1024 s units).
                if data.len() >= 5 && (data[0] & 0x02) != 0 {
                    let revs = u16::from_le_bytes([data[1], data[2]]);
                    let event_time = u16::from_le_bytes([data[3], data[4]]);
                    *STATE.lock().await = Ok(Some(State {
                        cumulative_crank_revs: Some(revs),
                        last_crank_event_time: Some(event_time),
                    }));
                    log::debug!("[BLE] CSC revs={} event_time={}", revs, event_time);
                }
            }
        })
        .await;

        if let Err(e) = task_result {
            log::warn!("[BLE] GATT task error: {:?}", e);
        }

        *STATE.lock().await = Ok(None);
        log::info!("[BLE] Disconnected, restarting scan");
    }
}

#[allow(clippy::too_many_arguments)]
pub fn init(
    spawner: Spawner,
    timer0: Peri<'static, peripherals::TIMER0>,
    rtc0: Peri<'static, peripherals::RTC0>,
    temp: Peri<'static, peripherals::TEMP>,
    ppi_ch17: Peri<'static, peripherals::PPI_CH17>,
    ppi_ch18: Peri<'static, peripherals::PPI_CH18>,
    ppi_ch19: Peri<'static, peripherals::PPI_CH19>,
    ppi_ch20: Peri<'static, peripherals::PPI_CH20>,
    ppi_ch21: Peri<'static, peripherals::PPI_CH21>,
    ppi_ch22: Peri<'static, peripherals::PPI_CH22>,
    ppi_ch23: Peri<'static, peripherals::PPI_CH23>,
    ppi_ch24: Peri<'static, peripherals::PPI_CH24>,
    ppi_ch25: Peri<'static, peripherals::PPI_CH25>,
    ppi_ch26: Peri<'static, peripherals::PPI_CH26>,
    ppi_ch27: Peri<'static, peripherals::PPI_CH27>,
    ppi_ch28: Peri<'static, peripherals::PPI_CH28>,
    ppi_ch29: Peri<'static, peripherals::PPI_CH29>,
    ppi_ch30: Peri<'static, peripherals::PPI_CH30>,
    ppi_ch31: Peri<'static, peripherals::PPI_CH31>,
    rng_periph: Peri<'static, peripherals::RNG>,
) -> Result<(), Error> {
    let mpsl_p = mpsl::Peripherals::new(rtc0, timer0, temp, ppi_ch19, ppi_ch30, ppi_ch31);
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };
    static MPSL: StaticCell<MultiprotocolServiceLayer<'static>> = StaticCell::new();
    let mpsl = MPSL.init(
        mpsl::MultiprotocolServiceLayer::new(mpsl_p, crate::Irqs, lfclk_cfg)
            .map_err(Error::MpslError)?,
    );
    spawner.spawn(mpsl_task(mpsl).map_err(Error::SpawnError)?);

    let sdc_p = sdc::Peripherals::new(
        ppi_ch17, ppi_ch18, ppi_ch20, ppi_ch21, ppi_ch22, ppi_ch23,
        ppi_ch24, ppi_ch25, ppi_ch26, ppi_ch27, ppi_ch28, ppi_ch29,
    );

    static RNG_CELL: StaticCell<rng::Rng<'static, Blocking>> = StaticCell::new();
    let rng_ref = RNG_CELL.init(rng::Rng::new_blocking(rng_periph));

    static SDC_MEM: StaticCell<sdc::Mem<4096>> = StaticCell::new();
    let sdc = sdc::Builder::new()
        .map_err(Error::SdcError)?
        .support_ext_scan()
        .support_central()
        .central_count(1)
        .map_err(Error::SdcError)?
        .build(sdc_p, rng_ref, mpsl, SDC_MEM.init(sdc::Mem::new()))
        .map_err(Error::SdcError)?;

    spawner.spawn(ble_task(sdc).map_err(Error::SpawnError)?);
    Ok(())
}
