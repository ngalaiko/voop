pub mod central;
pub mod peripheral;

use bt_hci::cmd::SyncCmd;
use embassy_nrf::{mode::Blocking, peripherals, rng, Peri};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::vendor::ZephyrReadStaticAddrs;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;
use trouble_host::prelude::*;

pub use central::Error;

pub(crate) type MyController = nrf_sdc::SoftdeviceController<'static>;

pub struct Ble {
    mpsl: &'static MultiprotocolServiceLayer<'static>,
    sdc: MyController,
}

impl Ble {
    /// Request the high-frequency crystal through MPSL, which owns the CLOCK peripheral, and
    /// keep it running for the lifetime of the device — USB needs HFXO continuously.
    ///
    /// Calls the raw `mpsl_clock_hfclk_src_request` directly: the pinned nrf-mpsl revision's
    /// `Hfclk` wrapper still calls the removed `mpsl_clock_hfclk_request` symbol and fails to
    /// link against its own vendored library.
    pub fn request_hfclk_forever(&self) -> Result<(), Error> {
        unsafe extern "C" fn hfclk_started(_src: u32) {}
        let ret = unsafe {
            mpsl::raw::mpsl_clock_hfclk_src_request(
                mpsl::raw::MPSL_CLOCK_HF_SRC_XO,
                Some(hfclk_started),
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(Error::HfclkFailed)
        }
    }

    pub async fn run(self) {
        let Ble { mpsl, sdc: controller } = self;

        let random_addr = match ZephyrReadStaticAddrs::new().exec(&controller).await {
            Ok(r) => Address::new(AddrKind::RANDOM, r.addr.addr),
            Err(e) => {
                log::error!("[BLE] failed to read static addr: {:?}", e);
                return;
            }
        };

        static RESOURCES: StaticCell<HostResources<MyController, DefaultPacketPool, 2, 1>> =
            StaticCell::new();
        let resources = RESOURCES.init(HostResources::new());
        let stack = trouble_host::new(controller, resources)
            .set_random_address(random_addr)
            .build();

        let mut runner = stack.runner();

        embassy_futures::join::join(
            async { mpsl.run().await },
            async {
                // select, not join: peripheral::run and central::run never return, so a
                // join could never complete — a runner failure would leave the host dead
                // (both roles parked on pending operations) with the error log unreachable.
                // Any side ending means BLE is unrecoverable: log it and reset the SoC.
                match embassy_futures::select::select3(
                    runner.run_with_handler(&central::CscEventHandler),
                    peripheral::run(&stack),
                    central::run(&stack),
                )
                .await
                {
                    embassy_futures::select::Either3::First(Err(e)) => {
                        log::error!("[BLE] runner error: {:?}", e);
                    }
                    _ => log::error!("[BLE] task ended unexpectedly"),
                }
                // Let the USB logger flush the line, then reboot.
                embassy_time::Timer::after(embassy_time::Duration::from_millis(100)).await;
                cortex_m::peripheral::SCB::sys_reset();
            },
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn init(
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
) -> Result<Ble, Error> {
    let mpsl_p = mpsl::Peripherals::new(rtc0, timer0, temp, ppi_ch19, ppi_ch30, ppi_ch31);
    // The XIAO nRF52840 carries a 32.768 kHz crystal — use it instead of the internal RC.
    // The RC drifts hundreds of ppm (seconds/hour on the wall-clock anchor between GPS
    // re-syncs) and needs periodic calibration wakeups; the crystal is ~10-50× tighter and
    // cheaper to run. embassy's time driver is switched in main() to match.
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_XTAL as u8,
        rc_ctiv: 0,
        rc_temp_ctiv: 0,
        accuracy_ppm: 50,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };
    static MPSL: StaticCell<MultiprotocolServiceLayer<'static>> = StaticCell::new();
    let mpsl = MPSL.init(
        mpsl::MultiprotocolServiceLayer::new(mpsl_p, crate::Irqs, lfclk_cfg)
            .map_err(|_| Error::MpslInitFailed)?,
    );

    let sdc_p = sdc::Peripherals::new(
        ppi_ch17, ppi_ch18, ppi_ch20, ppi_ch21, ppi_ch22, ppi_ch23, ppi_ch24, ppi_ch25, ppi_ch26,
        ppi_ch27, ppi_ch28, ppi_ch29,
    );

    static RNG_CELL: StaticCell<rng::Rng<'static, Blocking>> = StaticCell::new();
    let rng_ref = RNG_CELL.init(rng::Rng::new_blocking(rng_periph));

    static SDC_MEM: StaticCell<sdc::Mem<8192>> = StaticCell::new();
    let sdc = sdc::Builder::new()
        .map_err(|_| Error::SdcInitFailed)?
        .support_ext_adv()
        .support_peripheral()
        .peripheral_count(1)
        .map_err(|_| Error::SdcInitFailed)?
        .support_ext_scan()
        .support_central()
        .central_count(1)
        .map_err(|_| Error::SdcInitFailed)?
        .scan_buffer_cfg(3)
        .map_err(|_| Error::SdcInitFailed)?
        .buffer_cfg(251, 251, 3, 3)
        .map_err(|_| Error::SdcInitFailed)?
        .build(sdc_p, rng_ref, mpsl, SDC_MEM.init(sdc::Mem::new()))
        .map_err(|_| Error::SdcInitFailed)?;

    Ok(Ble { mpsl, sdc })
}
