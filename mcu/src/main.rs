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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.debug = embassy_nrf::config::Debug::NotConfigured;
    let p = embassy_nrf::init(config);

    let usb = usb::init(spawner, p.USBD).expect("Failed to initialize USB");
    logger::init(spawner, usb).expect("Failed to initialize logger");
    log::info!("Starting");

    gps::init(
        spawner, p.UARTE0, p.P1_11, p.P1_12, p.TIMER1, p.PPI_CH0, p.PPI_CH1,
    )
    .expect("Failed to initialize GPS");

    ble::init(
        spawner, p.TIMER0, p.RTC0, p.TEMP, p.PPI_CH17, p.PPI_CH18, p.PPI_CH19, p.PPI_CH20,
        p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25, p.PPI_CH26, p.PPI_CH27,
        p.PPI_CH28, p.PPI_CH29, p.PPI_CH30, p.PPI_CH31, p.RNG,
    )
    .expect("Failed to initialize BLE");

    screen::init(spawner, p.TWISPI0, p.P0_04, p.P0_05).expect("Failed to initialize screen");
    store::init(spawner).expect("Failed to initialize store");

    let _green = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
    loop {
        core::future::pending::<()>().await;
    }
}
