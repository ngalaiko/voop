#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::Peri;
use embassy_time::Timer;
use panic_halt as _;

mod irqs;
pub use irqs::Irqs;

pub mod ble;
pub mod gps;
pub mod logger;
pub mod screen;
pub mod usb;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.debug = embassy_nrf::config::Debug::NotConfigured;
    let p = embassy_nrf::init(config);

    if let Err(e) = run(
        spawner, p.P0_30, p.USBD, p.TWISPI0, p.P0_04, p.P0_05, p.UARTE0, p.P1_11, p.P1_12,
        p.TIMER1, p.PPI_CH0, p.PPI_CH1, p.TIMER0, p.RTC0, p.TEMP, p.PPI_CH17, p.PPI_CH18,
        p.PPI_CH19, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25,
        p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29, p.PPI_CH30, p.PPI_CH31, p.RNG,
    )
    .await
    {
        log::error!("{}", e);
        let mut red = Output::new(p.P0_26, Level::High, OutputDrive::Standard);
        loop {
            red.set_low();
            Timer::after_secs(2).await;
            red.set_high();
            Timer::after_secs(2).await;
        }
    }
}

#[derive(Debug)]
enum Error {
    UsbError(usb::Error),
    LoggerError(logger::Error),
    ScreenError(screen::Error),
    GpsError(gps::Error),
    BleError(ble::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UsbError(e) => write!(f, "USB: {}", e),
            Error::LoggerError(e) => write!(f, "Logger: {}", e),
            Error::ScreenError(e) => write!(f, "Screen: {}", e),
            Error::GpsError(e) => write!(f, "GPS: {}", e),
            Error::BleError(e) => write!(f, "BLE: {}", e),
        }
    }
}

impl core::error::Error for Error {}

async fn run(
    spawner: Spawner,
    p0_30: Peri<'static, embassy_nrf::peripherals::P0_30>,
    usbd: Peri<'static, embassy_nrf::peripherals::USBD>,
    twispi0: Peri<'static, embassy_nrf::peripherals::TWISPI0>,
    p0_04: Peri<'static, embassy_nrf::peripherals::P0_04>,
    p0_05: Peri<'static, embassy_nrf::peripherals::P0_05>,
    uarte0: Peri<'static, embassy_nrf::peripherals::UARTE0>,
    p1_11: Peri<'static, embassy_nrf::peripherals::P1_11>,
    p1_12: Peri<'static, embassy_nrf::peripherals::P1_12>,
    timer1: Peri<'static, embassy_nrf::peripherals::TIMER1>,
    ppi_ch0: Peri<'static, embassy_nrf::peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, embassy_nrf::peripherals::PPI_CH1>,
    timer0: Peri<'static, embassy_nrf::peripherals::TIMER0>,
    rtc0: Peri<'static, embassy_nrf::peripherals::RTC0>,
    temp: Peri<'static, embassy_nrf::peripherals::TEMP>,
    ppi_ch17: Peri<'static, embassy_nrf::peripherals::PPI_CH17>,
    ppi_ch18: Peri<'static, embassy_nrf::peripherals::PPI_CH18>,
    ppi_ch19: Peri<'static, embassy_nrf::peripherals::PPI_CH19>,
    ppi_ch20: Peri<'static, embassy_nrf::peripherals::PPI_CH20>,
    ppi_ch21: Peri<'static, embassy_nrf::peripherals::PPI_CH21>,
    ppi_ch22: Peri<'static, embassy_nrf::peripherals::PPI_CH22>,
    ppi_ch23: Peri<'static, embassy_nrf::peripherals::PPI_CH23>,
    ppi_ch24: Peri<'static, embassy_nrf::peripherals::PPI_CH24>,
    ppi_ch25: Peri<'static, embassy_nrf::peripherals::PPI_CH25>,
    ppi_ch26: Peri<'static, embassy_nrf::peripherals::PPI_CH26>,
    ppi_ch27: Peri<'static, embassy_nrf::peripherals::PPI_CH27>,
    ppi_ch28: Peri<'static, embassy_nrf::peripherals::PPI_CH28>,
    ppi_ch29: Peri<'static, embassy_nrf::peripherals::PPI_CH29>,
    ppi_ch30: Peri<'static, embassy_nrf::peripherals::PPI_CH30>,
    ppi_ch31: Peri<'static, embassy_nrf::peripherals::PPI_CH31>,
    rng: Peri<'static, embassy_nrf::peripherals::RNG>,
) -> Result<(), Error> {
    let usb = usb::init(spawner, usbd).map_err(Error::UsbError)?;
    logger::init(spawner, usb).map_err(Error::LoggerError)?;
    log::info!("Starting");

    let mut screen = screen::init(twispi0, p0_04, p0_05).map_err(Error::ScreenError)?;

    let gps = gps::new(spawner, uarte0, p1_11, p1_12, timer1, ppi_ch0, ppi_ch1)
        .map_err(Error::GpsError)?;

    ble::init(
        spawner, timer0, rtc0, temp, ppi_ch17, ppi_ch18, ppi_ch19, ppi_ch20, ppi_ch21, ppi_ch22,
        ppi_ch23, ppi_ch24, ppi_ch25, ppi_ch26, ppi_ch27, ppi_ch28, ppi_ch29, ppi_ch30, ppi_ch31,
        rng,
    )
    .map_err(Error::BleError)?;

    let mut green = Output::new(p0_30, Level::High, OutputDrive::Standard);
    green.set_low();
    loop {
        Timer::after_secs(2).await;

        let gps_state = gps.read().await.map_err(Error::GpsError)?;
        let ble_state = ble::read().await;
        screen
            .print(gps_state, ble_state)
            .map_err(Error::ScreenError)?;
    }
}
