#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    let mut red = Output::new(p.P0_26, Level::High, OutputDrive::Standard);

    let Ok(mut screen) = screen::new(p.TWISPI0, p.P0_04, p.P0_05) else {
        red.set_high();
        return;
    };

    let Ok(gps) = gps::new(
        spawner, p.UARTE0, p.P0_11, p.P0_12, p.TIMER0, p.PPI_CH0, p.PPI_CH1,
    ) else {
        red.set_high();
        return;
    };

    loop {
        let Ok(state) = gps.read().await else {
            red.set_high();
            continue;
        };

        screen
            .print(screen::State {
                lat: state.lat,
                lon: state.lon,
                speed_knots: state.speed_knots,
            })
            .unwrap();

        Timer::after_secs(1).await;
    }
}

mod gps {
    use embassy_nrf::uarte::{self, Uarte};
    use embassy_nrf::{bind_interrupts, peripherals};

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::mutex::Mutex;

    bind_interrupts!(struct Irqs {
        UARTE0 => uarte::InterruptHandler<peripherals::UARTE0>;
    });

    #[derive(Debug, Clone)]
    pub enum Error {
        ReadError(uarte::Error),
        ParseError,
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Error::ReadError(e) => write!(f, "failed to read from GPS: {:?}", e),
                Error::ParseError => write!(f, "failed to parse GPS data"),
            }
        }
    }

    impl core::error::Error for Error {}

    #[derive(Debug, Clone)]
    pub struct State {
        pub lat: Option<f64>,
        pub lon: Option<f64>,
        pub speed_knots: Option<f32>,
    }

    static STATE: Mutex<CriticalSectionRawMutex, Result<State, Error>> = Mutex::new(Ok(State {
        lat: None,
        lon: None,
        speed_knots: None,
    }));

    #[embassy_executor::task]
    async fn task(
        uarte0: peripherals::UARTE0,
        rxd: peripherals::P0_11,
        txd: peripherals::P0_12,
        timer0: peripherals::TIMER0,
        ppi_ch0: peripherals::PPI_CH0,
        ppi_ch1: peripherals::PPI_CH1,
    ) {
        let uarte = Uarte::new(uarte0, Irqs, rxd, txd, uarte::Config::default());
        let (_tx, mut rx) = uarte.split_with_idle(timer0, ppi_ch0, ppi_ch1);

        let mut buf = [0u8; 82];
        loop {
            match rx.read_until_idle(&mut buf).await {
                Ok(bytes_read) => {
                    let reading =
                        nmea::parse_bytes(&buf[..bytes_read]).map_err(|_| Error::ParseError);
                    let mut state = STATE.lock().await;
                    match reading {
                        Ok(nmea::ParseResult::RMC(rmc)) => {
                            *state = Ok(State {
                                lat: rmc.lat,
                                lon: rmc.lon,
                                speed_knots: rmc.speed_over_ground,
                            });
                        }
                        Ok(_) => {
                            // Ignore other sentences for now
                        }
                        Err(e) => {
                            *state = Err(e);
                        }
                    }
                }
                Err(e) => {
                    let mut state = STATE.lock().await;
                    *state = Err(Error::ReadError(e));
                }
            };
        }
    }

    pub struct Gps {}

    impl Gps {
        pub async fn read(&self) -> Result<State, Error> {
            let state = STATE.lock().await;
            state.clone()
        }
    }

    pub fn new(
        spawner: embassy_executor::Spawner,
        uarte0: peripherals::UARTE0,
        rxd: peripherals::P0_11,
        txd: peripherals::P0_12,
        timer0: peripherals::TIMER0,
        ppi_ch0: peripherals::PPI_CH0,
        ppi_ch1: peripherals::PPI_CH1,
    ) -> Result<Gps, embassy_executor::SpawnError> {
        spawner.spawn(task(uarte0, rxd, txd, timer0, ppi_ch0, ppi_ch1))?;
        Ok(Gps {})
    }
}

mod screen {
    use display_interface::DisplayError;
    use embassy_nrf::{bind_interrupts, peripherals, twim};
    use embedded_graphics::{
        mono_font::{ascii::FONT_6X10, MonoTextStyleBuilder},
        pixelcolor::BinaryColor,
        prelude::*,
        text::{Baseline, Text},
    };
    use heapless::String;
    use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

    bind_interrupts!(struct Irqs {
        TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
    });

    #[derive(Debug)]
    pub enum Error {
        InitError(DisplayError),
        PrintError,
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Error::InitError(e) => write!(f, "failed to initialize screen: {:?}", e),
                Error::PrintError => write!(f, "failed to print to screen"),
            }
        }
    }

    impl core::error::Error for Error {}

    pub struct Screen {
        display: Ssd1306<
            ssd1306::prelude::I2CInterface<twim::Twim<'static, peripherals::TWISPI0>>,
            DisplaySize128x64,
            ssd1306::mode::BufferedGraphicsMode<DisplaySize128x64>,
        >,
    }

    pub struct State {
        pub lat: Option<f64>,
        pub lon: Option<f64>,
        pub speed_knots: Option<f32>,
    }

    impl Screen {
        fn new(
            i2c: peripherals::TWISPI0,
            sda: peripherals::P0_04,
            scl: peripherals::P0_05,
        ) -> Result<Self, Error> {
            let i2c = twim::Twim::new(i2c, Irqs, sda, scl, twim::Config::default());
            let interface = I2CDisplayInterface::new(i2c);
            let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
                .into_buffered_graphics_mode();
            display.init().map_err(Error::InitError)?;

            Ok(Self { display })
        }

        pub fn print(&mut self, state: State) -> Result<(), Error> {
            use core::fmt::Write;

            let text_style = MonoTextStyleBuilder::new()
                .font(&FONT_6X10)
                .text_color(BinaryColor::On)
                .build();

            let mut line1: String<32> = String::new();
            match state.lat {
                Some(lat) => write!(line1, "Lat: {:.5}", lat).map_err(|_| Error::PrintError)?,
                None => write!(line1, "Lat: N/A").map_err(|_| Error::PrintError)?,
            }
            Text::with_baseline(&line1, Point::zero(), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|_| Error::PrintError)?;

            let mut line2: String<32> = String::new();
            match state.lon {
                Some(lon) => write!(line2, "Lon: {:.5}", lon).map_err(|_| Error::PrintError)?,
                None => write!(line2, "Lon: N/A").map_err(|_| Error::PrintError)?,
            }
            Text::with_baseline(&line2, Point::new(0, 10), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|_| Error::PrintError)?;

            let mut line3: String<32> = String::new();
            match state.speed_knots {
                Some(speed) => {
                    write!(line3, "Speed: {:.2} kn", speed).map_err(|_| Error::PrintError)?
                }
                None => write!(line3, "Speed: N/A").map_err(|_| Error::PrintError)?,
            }
            Text::with_baseline(&line3, Point::new(0, 20), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|_| Error::PrintError)?;

            self.display.flush().map_err(|_| Error::PrintError)?;
            Ok(())
        }
    }

    pub fn new(
        i2c: peripherals::TWISPI0,
        sda: peripherals::P0_04,
        scl: peripherals::P0_05,
    ) -> Result<Screen, Error> {
        Screen::new(i2c, sda, scl)
    }
}
