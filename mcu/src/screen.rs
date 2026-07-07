use display_interface::DisplayError;
use embassy_futures::select::{select, select3, select4, Either, Either3, Either4};
use embassy_nrf::{peripherals, twim, Peri};
use embassy_time::{Duration, Ticker, Timer};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Baseline, Text},
};
use heapless::String;
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use static_cell::StaticCell;

use voop_protocol::{BatteryState, BatteryStatus};

use crate::gps::GpsState;

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

type Display = Ssd1306<
    I2CInterface<twim::Twim<'static>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
>;

struct ScreenState {
    gps: Option<GpsState>,
    crank_revs: Option<u16>,
    mcu_battery: Option<BatteryStatus>,
    sensor_connected: bool,
    sensor_battery: Option<u8>,
    ios_connected: bool,
    moving: Option<bool>,
    time: Option<(u8, u8)>,
}

fn render(display: &mut Display, state: &ScreenState) -> Result<(), InternalError> {
    use core::fmt::Write;

    let style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    display.clear(BinaryColor::Off).map_err(|_| InternalError::RenderError)?;

    // Line 0: GPS coordinates
    let mut line: String<32> = String::new();
    match state.gps {
        None => write!(line, "GPS: ---"),
        Some(g) => write!(
            line,
            "GPS:{:.2}/{:.2}",
            g.lat_microdeg as f64 / 1_000_000.0,
            g.lon_microdeg as f64 / 1_000_000.0
        ),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 0), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 1: Crank revolutions
    let mut line: String<16> = String::new();
    match state.crank_revs {
        None => write!(line, "CRK: ---"),
        Some(r) => write!(line, "CRK: {}", r),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 11), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 2: MCU battery
    let mut line: String<16> = String::new();
    match state.mcu_battery {
        None => write!(line, "MCU: ---"),
        Some(b) => write!(
            line,
            "MCU: {}%{}",
            b.percent,
            if b.state == BatteryState::Charging { " CHG" } else { "" }
        ),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 22), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 3: Sensor connection + battery
    let mut line: String<16> = String::new();
    match (state.sensor_connected, state.sensor_battery) {
        (false, _) => write!(line, "SNS: ---"),
        (true, None) => write!(line, "SNS: OK"),
        (true, Some(b)) => write!(line, "SNS: {}%", b),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 33), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 4: Connection + motion status
    let mut line: String<24> = String::new();
    write!(
        line,
        "iOS:{} CAD:{} MOV:{}",
        if state.ios_connected { "Y" } else { "N" },
        if state.sensor_connected { "Y" } else { "N" },
        match state.moving {
            None => "?",
            Some(true) => "Y",
            Some(false) => "N",
        }
    )
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 44), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    // Line 5: Current time (UTC)
    let mut line: String<8> = String::new();
    match state.time {
        None => write!(line, "--:--"),
        Some((h, m)) => write!(line, "{:02}:{:02}", h, m),
    }
    .map_err(|_| InternalError::RenderError)?;
    Text::with_baseline(&line, Point::new(0, 54), style, Baseline::Top)
        .draw(display)
        .map_err(|_| InternalError::RenderError)?;

    display.flush().map_err(|_| InternalError::RenderError)
}

pub struct Screen {
    i2c: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_04>,
    scl: Peri<'static, peripherals::P0_05>,
}

pub fn init(
    i2c: Peri<'static, peripherals::TWISPI0>,
    sda: Peri<'static, peripherals::P0_04>,
    scl: Peri<'static, peripherals::P0_05>,
) -> Screen {
    Screen { i2c, sda, scl }
}

impl Screen {
    pub async fn run(self) {
        static TX_BUF: StaticCell<[u8; 64]> = StaticCell::new();
        let tx_buf = TX_BUF.init([0u8; 64]);
        let i2c = twim::Twim::new(
            self.i2c,
            crate::Irqs,
            self.sda,
            self.scl,
            twim::Config::default(),
            tx_buf,
        );
        let interface = I2CDisplayInterface::new(i2c);
        let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
            .into_buffered_graphics_mode();

        // Retry init: a transient I2C hiccup at boot (or a display that powers up slowly)
        // shouldn't leave the screen dead for the whole uptime.
        let mut attempt = 0u32;
        while let Err(e) = display.init().map_err(InternalError::InitError) {
            attempt += 1;
            log::error!("[Screen] {} (attempt {}), retrying", e, attempt);
            Timer::after(Duration::from_secs(5)).await;
        }
        display.clear(BinaryColor::Off).ok();
        display.flush().ok();

        let Some(mut gps_rx) = crate::gps::GPS.receiver() else {
            log::error!("[Screen] GPS: no free receiver slot");
            return;
        };
        let Some(mut crank_rx) = crate::ble::central::CRANK_REVS.receiver() else {
            log::error!("[Screen] CRANK_REVS: no free receiver slot");
            return;
        };
        let Some(mut sensor_conn_rx) = crate::ble::central::SENSOR_CONNECTED.receiver() else {
            log::error!("[Screen] SENSOR_CONNECTED: no free receiver slot");
            return;
        };
        let Some(mut sensor_bat_rx) = crate::ble::central::SENSOR_BATTERY.receiver() else {
            log::error!("[Screen] SENSOR_BATTERY: no free receiver slot");
            return;
        };
        let Some(mut ios_rx) = crate::ble::peripheral::IOS_CONNECTED.receiver() else {
            log::error!("[Screen] IOS_CONNECTED: no free receiver slot");
            return;
        };
        let Some(mut moving_rx) = crate::imu::MOVING.receiver() else {
            log::error!("[Screen] MOVING: no free receiver slot");
            return;
        };
        let Some(mut mcu_bat_rx) = crate::battery::MCU_BATTERY.receiver() else {
            log::error!("[Screen] MCU_BATTERY: no free receiver slot");
            return;
        };

        let mut state = ScreenState {
            gps: None,
            crank_revs: None,
            mcu_battery: None,
            sensor_connected: false,
            sensor_battery: None,
            ios_connected: false,
            moving: None,
            time: None,
        };

        if let Some(millis) = crate::clock::now().await.unix_millis {
            let seconds = millis / 1000;
            state.time = Some(((seconds / 3600 % 24) as u8, (seconds / 60 % 60) as u8));
        }

        if let Err(e) = render(&mut display, &state) {
            log::warn!("[Screen] {}", e);
        }

        let mut ticker = Ticker::every(Duration::from_secs(1));

        loop {
            match select4(
                select(gps_rx.changed(), crank_rx.changed()),
                select(sensor_conn_rx.changed(), sensor_bat_rx.changed()),
                select3(ios_rx.changed(), moving_rx.changed(), mcu_bat_rx.changed()),
                ticker.next(),
            )
            .await
            {
                Either4::First(Either::First(gps)) => state.gps = gps,
                Either4::First(Either::Second(sample)) => state.crank_revs = Some(sample.revs),
                Either4::Second(Either::First(connected)) => state.sensor_connected = connected,
                Either4::Second(Either::Second(bat)) => state.sensor_battery = Some(bat),
                Either4::Third(Either3::First(connected)) => state.ios_connected = connected,
                Either4::Third(Either3::Second(moving)) => state.moving = Some(moving),
                Either4::Third(Either3::Third(bat)) => state.mcu_battery = Some(bat),
                Either4::Fourth(()) => {
                    if let Some(millis) = crate::clock::now().await.unix_millis {
                        let seconds = millis / 1000;
                        state.time =
                            Some(((seconds / 3600 % 24) as u8, (seconds / 60 % 60) as u8));
                    }
                }
            }

            if let Err(e) = render(&mut display, &state) {
                log::warn!("[Screen] {}", e);
            }
        }
    }
}
