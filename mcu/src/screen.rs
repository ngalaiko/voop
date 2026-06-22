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

use crate::gps::FixQuality;
use crate::store::DataPoint;

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

fn render(display: &mut Display, point: Option<DataPoint>) -> Result<(), InternalError> {
    use core::fmt::Write;

    let style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    display
        .clear(BinaryColor::Off)
        .map_err(|_| InternalError::RenderError)?;

    // Line 1: GPS fix quality + coordinates
    let mut line1: String<32> = String::new();
    match point.and_then(|p| p.lat.zip(p.lon).map(|(lat, lon)| (lat, lon, p.fix_quality))) {
        None => write!(line1, "GPS:---"),
        Some((lat, lon, fq)) => {
            let q = match fq {
                Some(FixQuality::Differential) => "DIF",
                _ => "AUT",
            };
            write!(
                line1,
                "GPS:{} {:.2}/{:.2}",
                q,
                lat as f64 / 1_000_000.0,
                lon as f64 / 1_000_000.0
            )
        }
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line1, Point::new(0, 0), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 2: crank revolutions
    let mut line2: String<16> = String::new();
    match point.and_then(|p| p.crank_revs) {
        None => write!(line2, "CRK: ---"),
        Some(revs) => write!(line2, "CRK: {}", revs),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line2, Point::new(0, 12), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 3: sensor battery
    let mut line3: String<16> = String::new();
    match point.and_then(|p| p.sensor_battery) {
        None => write!(line3, "BAT: ---"),
        Some(pct) => write!(line3, "BAT: {}%", pct),
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

    let Some(mut updated_rx) = crate::store::UPDATED.receiver() else {
        log::error!("[Screen] UPDATED watch: no free receiver slot");
        return;
    };

    if let Err(e) = render(&mut display, None) {
        log::warn!("[Screen] {}", e);
    }

    loop {
        updated_rx.changed().await;
        let point = crate::store::peek_latest().await;
        if let Err(e) = render(&mut display, point) {
            log::warn!("[Screen] {}", e);
        }
    }
}
