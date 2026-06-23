#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use panic_halt as _;

mod irqs;
pub use irqs::Irqs;

pub mod ble;
pub mod gps;
pub mod logger;
pub mod screen;
pub mod store;
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
async fn store_task() {
    store::run().await;
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.debug = embassy_nrf::config::Debug::NotConfigured;
    let p = embassy_nrf::init(config);

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
    let screen = screen::init(p.TWISPI0, p.P0_04, p.P0_05);

    spawner.spawn(usb_task(usb_runner).expect("usb: failed to spawn"));
    spawner.spawn(logger_task(logger).expect("logger: failed to spawn"));
    spawner.spawn(gps_task(gps).expect("gps: failed to spawn"));
    spawner.spawn(ble_task(ble).expect("ble: failed to spawn"));
    spawner.spawn(screen_task(screen).expect("screen: failed to spawn"));
    spawner.spawn(store_task().expect("store: failed to spawn"));
}
