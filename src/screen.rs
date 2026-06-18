use display_interface::DisplayError;
use embassy_nrf::{peripherals, twim, Peri};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use heapless::String;
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use static_cell::StaticCell;

#[derive(Debug)]
pub enum Error {
    InitError(DisplayError),
    PrintError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::InitError(e) => write!(f, "screen init failed: {:?}", e),
            Error::PrintError => write!(f, "screen print failed"),
        }
    }
}

impl core::error::Error for Error {}

pub struct Screen {
    display: Ssd1306<
        I2CInterface<twim::Twim<'static>>,
        DisplaySize128x64,
        ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
    >,
}

impl Screen {
    pub fn print(&mut self, state: crate::gps::State, ble: Result<Option<crate::ble::State>, crate::ble::Error>) -> Result<(), Error> {
        use core::fmt::Write;

        let style = MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(BinaryColor::On)
            .build();

        self.display
            .clear(BinaryColor::Off)
            .map_err(|_| Error::PrintError)?;

        // Line 1: GPS fix status + BLE connection status
        // e.g. "GPS:FIX BLE:CON" or "GPS:--- BLE:---"
        let gps_status = match &state.status {
            Some(crate::gps::Status::Autonomous) => "FIX",
            Some(crate::gps::Status::Differential) => "DIF",
            Some(crate::gps::Status::Invalid) => "INV",
            None => "---",
        };
        let ble_status = match &ble {
            Err(_) => "ERR",
            Ok(None) => "---",
            Ok(Some(_)) => "CON",
        };
        let mut line1: String<32> = String::new();
        write!(line1, "GPS:{} BLE:{}", gps_status, ble_status)
            .map_err(|_| Error::PrintError)?;
        Text::with_baseline(&line1, Point::new(0, 0), style, Baseline::Top)
            .draw(&mut self.display)
            .map_err(|_| Error::PrintError)?;

        // Line 2: lat/lon on same line, 4 decimal places
        // e.g. "37.1234/-122.1234" or "N/A/N/A"
        let mut line2: String<32> = String::new();
        match (state.lat, state.lon) {
            (Some(lat), Some(lon)) => write!(line2, "{:.4}/{:.4}", lat, lon)
                .map_err(|_| Error::PrintError)?,
            (Some(lat), None) => write!(line2, "{:.4}/N/A", lat)
                .map_err(|_| Error::PrintError)?,
            (None, Some(lon)) => write!(line2, "N/A/{:.4}", lon)
                .map_err(|_| Error::PrintError)?,
            (None, None) => write!(line2, "N/A/N/A").map_err(|_| Error::PrintError)?,
        }
        Text::with_baseline(&line2, Point::new(0, 12), style, Baseline::Top)
            .draw(&mut self.display)
            .map_err(|_| Error::PrintError)?;

        self.display.flush().map_err(|_| Error::PrintError)
    }
}

pub fn init(
    i2c: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_04>,
    scl: Peri<'static, peripherals::P0_05>,
) -> Result<Screen, Error> {
    static TX_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    let tx_buf = TX_BUF.init([0u8; 64]);

    let i2c = twim::Twim::new(i2c, crate::Irqs, sda, scl, twim::Config::default(), tx_buf);
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().map_err(Error::InitError)?;
    display.clear(BinaryColor::Off).map_err(Error::InitError)?;
    display.flush().map_err(Error::InitError)?;

    Ok(Screen { display })
}
