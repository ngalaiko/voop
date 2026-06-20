use display_interface::DisplayError;
use embassy_futures::select::{select3, Either3};
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

use crate::gps::{FixQuality, Location};

#[derive(Debug)]
enum InternalError {
    InitError(DisplayError),
    RenderError,
}

impl core::fmt::Display for InternalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InternalError::InitError(e) => write!(f, "init failed: {:?}", e),
            InternalError::RenderError => write!(f, "render failed"),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    SpawnFailed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnFailed => write!(f, "failed to spawn screen task"),
        }
    }
}

impl core::error::Error for Error {}

type Display = Ssd1306<
    I2CInterface<twim::Twim<'static>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
>;

fn render(
    display: &mut Display,
    location: Option<&Result<Location, crate::gps::Error>>,
    crank_revs: Option<&Result<u16, crate::ble::Error>>,
    battery: Option<&Result<u8, crate::ble::Error>>,
) -> Result<(), InternalError> {
    use core::fmt::Write;

    let style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    display
        .clear(BinaryColor::Off)
        .map_err(|_| InternalError::RenderError)?;

    // Line 1: GPS — three states: missing / value (with fix quality) / error
    let mut line1: String<32> = String::new();
    match location {
        None => write!(line1, "GPS:---"),
        Some(Ok(loc)) => {
            let q = match loc.fix_quality {
                FixQuality::Autonomous => "AUT",
                FixQuality::Differential => "DIF",
            };
            write!(line1, "GPS:{} {:.2}/{:.2}", q, loc.lat, loc.lon)
        }
        Some(Err(_)) => write!(line1, "GPS:ERR"),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line1, Point::new(0, 0), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 2: crank revolutions — three states: missing / value / error
    let mut line2: String<16> = String::new();
    match crank_revs {
        None => write!(line2, "CRK: ---"),
        Some(Ok(revs)) => write!(line2, "CRK: {}", revs),
        Some(Err(_)) => write!(line2, "CRK: ERR"),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line2, Point::new(0, 12), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 3: battery — three states: missing / value / error
    let mut line3: String<16> = String::new();
    match battery {
        None => write!(line3, "BAT: ---"),
        Some(Ok(pct)) => write!(line3, "BAT: {}%", pct),
        Some(Err(_)) => write!(line3, "BAT: ERR"),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line3, Point::new(0, 24), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    display.flush().map_err(|_| InternalError::RenderError)
}

pub fn init(
    spawner: embassy_executor::Spawner,
    i2c: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_04>,
    scl: Peri<'static, peripherals::P0_05>,
) -> Result<(), Error> {
    spawner.spawn(task(i2c, sda, scl).map_err(|_| Error::SpawnFailed)?);
    Ok(())
}

#[embassy_executor::task]
async fn task(
    i2c: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_04>,
    scl: Peri<'static, peripherals::P0_05>,
) {
    static TX_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    let tx_buf = TX_BUF.init([0u8; 64]);
    let i2c = twim::Twim::new(i2c, crate::Irqs, sda, scl, twim::Config::default(), tx_buf);
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    if let Err(e) = display.init().map_err(InternalError::InitError) {
        log::error!("[Screen] {}", e);
        return;
    }
    display.clear(BinaryColor::Off).ok();
    display.flush().ok();

    let Some(mut location_rx) = crate::gps::LOCATION.receiver() else {
        log::error!("[Screen] LOCATION watch: no free receiver slot");
        return;
    };
    let Some(mut crank_rx) = crate::ble::CRANK_REVS.receiver() else {
        log::error!("[Screen] CRANK_REVS watch: no free receiver slot");
        return;
    };
    let Some(mut battery_rx) = crate::ble::BATTERY.receiver() else {
        log::error!("[Screen] BATTERY watch: no free receiver slot");
        return;
    };

    let mut location: Option<Result<Location, crate::gps::Error>> = None;
    let mut crank_revs: Option<Result<u16, crate::ble::Error>> = None;
    let mut battery: Option<Result<u8, crate::ble::Error>> = None;

    if let Err(e) = render(&mut display, None, None, None) {
        log::warn!("[Screen] {}", e);
    }

    loop {
        match select3(
            location_rx.changed(),
            crank_rx.changed(),
            battery_rx.changed(),
        )
        .await
        {
            Either3::First(result) => location = Some(result),
            Either3::Second(result) => crank_revs = Some(result),
            Either3::Third(result) => battery = Some(result),
        }
        if let Err(e) = render(
            &mut display,
            location.as_ref(),
            crank_revs.as_ref(),
            battery.as_ref(),
        ) {
            log::warn!("[Screen] {}", e);
        }
    }
}
