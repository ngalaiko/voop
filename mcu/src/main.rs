#![no_std]
#![no_main]

use embassy_executor::Spawner;
use panic_halt as _;

mod irqs;
pub use irqs::Irqs;

pub mod battery;
pub mod ble;
pub mod clock;
pub mod gps;
pub mod imu;
pub mod logger;
pub mod screen;
pub mod usb;

#[embassy_executor::task]
async fn usb_task(runner: usb::UsbRunner) {
    runner.run().await;
}

#[embassy_executor::task]
async fn logger_task(runner: logger::LoggerRunner) {
    runner.run().await;
}

#[embassy_executor::task]
async fn gps_task(gps: gps::Gps) {
    gps.run().await;
}

#[embassy_executor::task]
async fn ble_task(ble: ble::Ble) {
    ble.run().await;
}

#[embassy_executor::task]
async fn screen_task(screen: screen::Screen) {
    screen.run().await;
}

#[embassy_executor::task]
async fn imu_task(imu: imu::Imu) {
    imu.run().await;
}

#[embassy_executor::task]
async fn battery_task(battery: battery::Battery) {
    battery.run().await;
}

#[embassy_executor::task]
async fn watchdog_task(mut handle: embassy_nrf::wdt::WatchdogHandle) {
    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_secs(1));
    loop {
        handle.pet();
        ticker.next().await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.debug = embassy_nrf::config::Debug::NotConfigured;
    // Onboard 32.768 kHz crystal (matches the MPSL LFCLK config in ble.rs).
    config.lfclk_source = embassy_nrf::config::LfclkSource::ExternalXtal;
    let p = embassy_nrf::init(config);

    // Hardware watchdog, 8 s window, pet once a second from its own task. Without it a
    // panic (panic-halt parks the whole thread-mode executor) or a wedged executor leaves
    // an unattended logger dead but still draining the battery until a power cycle.
    let mut wdt_config = embassy_nrf::wdt::Config::default();
    wdt_config.timeout_ticks = 32768 * 8;
    let Ok((_wdt, [wdt_handle])) = embassy_nrf::wdt::Watchdog::try_new(p.WDT, wdt_config) else {
        panic!("wdt: already running with incompatible config");
    };

    let (usb_runner, usb) = usb::init(p.USBD);
    let logger = logger::init(usb).expect("logger: failed to initialize");
    log::info!("Starting");

    let gps = gps::init(p.UARTE0, p.P1_11, p.P1_12, p.TIMER1, p.PPI_CH0, p.PPI_CH1);
    let ble = ble::init(
        p.TIMER0, p.RTC0, p.TEMP, p.PPI_CH17, p.PPI_CH18, p.PPI_CH19, p.PPI_CH20, p.PPI_CH21,
        p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28,
        p.PPI_CH29, p.PPI_CH30, p.PPI_CH31, p.RNG,
    )
    .expect("ble: failed to initialize");
    // USB needs HFXO running continuously; MPSL owns the CLOCK peripheral, so the clock must
    // be requested through it (a raw register write would get stopped when the radio idles).
    // Requested before the USB task spawns and never released.
    ble.request_hfclk_forever().expect("hfclk: failed to request");
    let screen = screen::init(p.TWISPI0, p.P0_04, p.P0_05);
    let imu = imu::init(p.TWISPI1, p.P0_07, p.P0_27, p.P1_08, p.P0_11);
    let battery = battery::init(p.SAADC, p.P0_31, p.P0_14, p.P0_17);

    spawner.spawn(watchdog_task(wdt_handle).expect("wdt: failed to spawn"));
    spawner.spawn(usb_task(usb_runner).expect("usb: failed to spawn"));
    spawner.spawn(logger_task(logger).expect("logger: failed to spawn"));
    spawner.spawn(gps_task(gps).expect("gps: failed to spawn"));
    spawner.spawn(ble_task(ble).expect("ble: failed to spawn"));
    spawner.spawn(screen_task(screen).expect("screen: failed to spawn"));
    spawner.spawn(imu_task(imu).expect("imu: failed to spawn"));
    spawner.spawn(battery_task(battery).expect("battery: failed to spawn"));
}
