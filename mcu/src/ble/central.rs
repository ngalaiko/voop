use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::Watch;
use trouble_host::prelude::*;

use super::MyController;

const CSC_SERVICE: Uuid = Uuid::new_short(0x1816);
const CSC_MEASUREMENT: Uuid = Uuid::new_short(0x2A5B);
const BATTERY_SERVICE: Uuid = Uuid::new_short(0x180F);
const BATTERY_LEVEL: Uuid = Uuid::new_short(0x2A19);

#[derive(Clone, Debug)]
pub enum Error {
    SpawnFailed,
    MpslInitFailed,
    SdcInitFailed,
    RunnerCrashed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnFailed => write!(f, "spawn failed"),
            Error::MpslInitFailed => write!(f, "MPSL init failed"),
            Error::SdcInitFailed => write!(f, "SDC init failed"),
            Error::RunnerCrashed => write!(f, "BLE runner crashed"),
        }
    }
}

impl core::error::Error for Error {}

// Cumulative crank revolutions — sent only when the value changes (bike moving).
pub static CRANK_REVS: Watch<CriticalSectionRawMutex, Result<u16, Error>, 1> = Watch::new();

// Battery percentage (0–100) — sent when the sensor reports a change.
pub static BATTERY: Watch<CriticalSectionRawMutex, Result<u8, Error>, 1> = Watch::new();

// Signals the address of the first CSC sensor spotted during scanning.
pub(super) static SENSOR_ADDR: Signal<CriticalSectionRawMutex, Address> = Signal::new();

fn ad_has_uuid16(data: &[u8], target: u16) -> bool {
    let mut i = 0;
    while i < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        let ad_type = data[i + 1];
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

pub(super) struct CscEventHandler;

impl EventHandler for CscEventHandler {
    fn on_ext_adv_reports(&self, reports: LeExtAdvReportsIter<'_>) {
        for report in reports.filter_map(|r| r.ok()) {
            if ad_has_uuid16(report.data, 0x1816) {
                SENSOR_ADDR.signal(Address::new(report.addr_kind, report.addr));
            }
        }
    }
}

pub async fn run(stack: &Stack<'_, MyController, DefaultPacketPool>) {
    loop {
        SENSOR_ADDR.reset();

        log::info!("[BLE central] Scanning for CSC sensor...");
        let central = stack.central();
        let mut scanner = Scanner::new(central);
        let scan_config = ScanConfig {
            filter_accept_list: &[],
            ..Default::default()
        };

        let session = match scanner.scan_ext(&scan_config).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[BLE central] scan error: {:?}", e);
                continue;
            }
        };

        let addr = SENSOR_ADDR.wait().await;
        drop(session);
        embassy_futures::yield_now().await;

        log::info!("[BLE central] Found CSC sensor, connecting...");
        let mut central = scanner.into_inner();
        let connect_config = ConnectConfig {
            connect_params: Default::default(),
            scan_config: ScanConfig {
                filter_accept_list: &[addr],
                ..Default::default()
            },
        };

        let conn = match central.connect_ext(&connect_config).await {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[BLE central] connect error: {:?}", e);
                continue;
            }
        };
        log::info!("[BLE central] Connected to Garmin");

        let client: GattClient<'_, MyController, DefaultPacketPool, 4> =
            match GattClient::new(stack, &conn).await {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[BLE central] GattClient error: {:?}", e);
                    continue;
                }
            };

        let (task_result, _) = embassy_futures::join::join(client.task(), async {
            let services = match client.services_by_uuid(&CSC_SERVICE).await {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("[BLE central] service discovery error: {:?}", e);
                    return;
                }
            };
            let service = match services.first() {
                Some(s) => s.clone(),
                None => {
                    log::info!("[BLE central] No CSC service found");
                    return;
                }
            };
            let characteristic: Characteristic<[u8]> = match client
                .characteristic_by_uuid(&service, &CSC_MEASUREMENT)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[BLE central] characteristic error: {:?}", e);
                    return;
                }
            };
            let mut csc_listener = match client.subscribe(&characteristic, false).await {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("[BLE central] subscribe error: {:?}", e);
                    return;
                }
            };

            let mut battery_listener = 'bat: {
                let Ok(svcs) = client.services_by_uuid(&BATTERY_SERVICE).await else {
                    break 'bat None;
                };
                let Some(svc) = svcs.first().cloned() else {
                    break 'bat None;
                };
                let Ok(bat_char): Result<Characteristic<[u8]>, _> =
                    client.characteristic_by_uuid(&svc, &BATTERY_LEVEL).await
                else {
                    break 'bat None;
                };
                client.subscribe(&bat_char, false).await.ok()
            };

            log::info!("[BLE central] Subscribed to CSC measurement");
            embassy_futures::join::join(
                async {
                    let crank_tx = CRANK_REVS.sender();
                    let mut last_revs: Option<u16> = None;
                    loop {
                        let notif = csc_listener.next().await;
                        let data = notif.as_ref();
                        if data.len() >= 5 && (data[0] & 0x02) != 0 {
                            let revs = u16::from_le_bytes([data[1], data[2]]);
                            if Some(revs) != last_revs {
                                crank_tx.send(Ok(revs));
                                last_revs = Some(revs);
                            }
                            log::debug!("[BLE central] CSC revs={}", revs);
                        }
                    }
                },
                async {
                    let battery_tx = BATTERY.sender();
                    match battery_listener {
                        Some(ref mut listener) => loop {
                            let notif = listener.next().await;
                            let data = notif.as_ref();
                            if let Some(&level) = data.first() {
                                log::info!("[BLE central] Battery: {}%", level);
                                battery_tx.send(Ok(level));
                            }
                        },
                        None => core::future::pending().await,
                    }
                },
            )
            .await;
        })
        .await;

        if let Err(e) = task_result {
            log::warn!("[BLE central] GATT task error: {:?}", e);
        }
        log::info!("[BLE central] Disconnected, restarting scan");
    }
}
