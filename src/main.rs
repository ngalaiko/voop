#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_time::Timer;
use panic_halt as _;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());
    if let Err(e) = run(
        spawner, p.P0_30, p.USBD, p.TWISPI0, p.P0_04, p.P0_05, p.UARTE0, p.P1_11, p.P1_12,
        p.TIMER0, p.PPI_CH0, p.PPI_CH1,
    )
    .await
    {
        log::error!("{}", e);
        let mut red = Output::new(p.P0_26, Level::High, OutputDrive::Standard);
        loop {
            red.set_high();
            Timer::after_secs(2).await;
            red.set_low();
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
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::UsbError(e) => write!(f, "USB: {:?}", e),
            Error::LoggerError(e) => write!(f, "Logger: {:?}", e),
            Error::ScreenError(e) => write!(f, "Screen: {:?}", e),
            Error::GpsError(e) => write!(f, "GPS: {:?}", e),
        }
    }
}

impl core::error::Error for Error {}

async fn run(
    spawner: Spawner,
    p0_30: embassy_nrf::peripherals::P0_30,
    usbd: embassy_nrf::peripherals::USBD,
    twispi0: embassy_nrf::peripherals::TWISPI0,
    p0_04: embassy_nrf::peripherals::P0_04,
    p0_05: embassy_nrf::peripherals::P0_05,
    uarte0: embassy_nrf::peripherals::UARTE0,
    p1_11: embassy_nrf::peripherals::P1_11,
    p1_12: embassy_nrf::peripherals::P1_12,
    timer0: embassy_nrf::peripherals::TIMER0,
    ppi_ch0: embassy_nrf::peripherals::PPI_CH0,
    ppi_ch1: embassy_nrf::peripherals::PPI_CH1,
) -> Result<(), Error> {
    let usb = usb::init(spawner, usbd).map_err(Error::UsbError)?;
    logger::init(spawner, usb).map_err(Error::LoggerError)?;
    log::info!("Starting");

    let mut screen = screen::init(twispi0, p0_04, p0_05).map_err(Error::ScreenError)?;

    let gps = gps::new(spawner, uarte0, p1_11, p1_12, timer0, ppi_ch0, ppi_ch1)
        .map_err(Error::GpsError)?;

    let mut green = Output::new(p0_30, Level::Low, OutputDrive::Standard);
    loop {
        Timer::after_secs(1).await;
        green.set_high();
        Timer::after_secs(1).await;
        green.set_low();

        let state = gps.read().await.map_err(Error::GpsError)?;
        screen.print(state).map_err(Error::ScreenError)?;
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
        SpawnError(embassy_executor::SpawnError),
        ReadError(uarte::Error),
        ParseError,
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Error::SpawnError(e) => write!(f, "failed to spawn GPS task: {}", e),
                Error::ReadError(e) => write!(f, "failed to read from GPS: {:?}", e),
                Error::ParseError => write!(f, "failed to parse GPS data"),
            }
        }
    }

    impl core::error::Error for Error {}

    #[derive(Debug, Clone)]
    pub enum Status {
        Autonomous,
        Differential,
        Invalid,
    }

    #[derive(Debug, Clone)]
    pub struct State {
        pub lat: Option<f64>,
        pub lon: Option<f64>,
        pub speed_knots: Option<f32>,
        pub status: Option<Status>,
    }

    static STATE: Mutex<CriticalSectionRawMutex, Result<State, Error>> = Mutex::new(Ok(State {
        lat: None,
        lon: None,
        speed_knots: None,
        status: None,
    }));

    #[embassy_executor::task]
    async fn task(
        uarte0: peripherals::UARTE0,
        txd: peripherals::P1_11,
        rxd: peripherals::P1_12,
        timer0: peripherals::TIMER0,
        ppi_ch0: peripherals::PPI_CH0,
        ppi_ch1: peripherals::PPI_CH1,
    ) {
        let mut config = uarte::Config::default();
        config.baudrate = uarte::Baudrate::BAUD9600;
        let uarte = Uarte::new(uarte0, Irqs, rxd, txd, config);
        let (_tx, mut rx) = uarte.split_with_idle(timer0, ppi_ch0, ppi_ch1);

        let mut buf = [0u8; 1024];
        loop {
            log::debug!("Waiting for GPS data...");
            match rx.read_until_idle(&mut buf).await {
                Ok(bytes_read) => {
                    for line in buf[..bytes_read].split(|&b| b == b'\n') {
                        if line.is_empty() {
                            continue;
                        }
                        let reading = nmea::parse_bytes(&line).map_err(|_| Error::ParseError);
                        log::debug!(
                            "Raw GPS data: {}",
                            core::str::from_utf8(line).unwrap_or("<invalid UTF-8>")
                        );
                        let mut state = STATE.lock().await;
                        match reading {
                            Ok(nmea::ParseResult::RMC(rmc)) => {
                                log::debug!("Parsed RMC: {:?}", rmc);
                                *state = Ok(State {
                                    lat: rmc.lat,
                                    lon: rmc.lon,
                                    speed_knots: rmc.speed_over_ground,
                                    status: match rmc.status_of_fix {
                                        nmea::sentences::rmc::RmcStatusOfFix::Invalid => {
                                            Some(Status::Invalid)
                                        }
                                        nmea::sentences::rmc::RmcStatusOfFix::Autonomous => {
                                            Some(Status::Autonomous)
                                        }
                                        nmea::sentences::rmc::RmcStatusOfFix::Differential => {
                                            Some(Status::Differential)
                                        }
                                    },
                                });
                            }
                            Ok(other) => {
                                log::debug!("Parsed other NMEA sentence: {:?}", other);
                                // Ignore other sentences for now
                            }
                            Err(e) => {
                                log::error!("Failed to parse NMEA sentence: {:?}", e);
                                *state = Err(e);
                            }
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

    pub struct Gps;

    impl Gps {
        pub async fn read(&self) -> Result<State, Error> {
            let state = STATE.lock().await;
            state.clone()
        }
    }

    pub fn new(
        spawner: embassy_executor::Spawner,
        uarte0: peripherals::UARTE0,
        txd: peripherals::P1_11,
        rxd: peripherals::P1_12,
        timer0: peripherals::TIMER0,
        ppi_ch0: peripherals::PPI_CH0,
        ppi_ch1: peripherals::PPI_CH1,
    ) -> Result<Gps, Error> {
        spawner
            .spawn(task(uarte0, txd, rxd, timer0, ppi_ch0, ppi_ch1))
            .map_err(Error::SpawnError)?;
        Ok(Gps)
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
            display.clear(BinaryColor::Off).map_err(Error::InitError)?;
            display.flush().map_err(Error::InitError)?;

            Ok(Self { display })
        }

        pub fn print(&mut self, state: super::gps::State) -> Result<(), Error> {
            use core::fmt::Write;

            let text_style = MonoTextStyleBuilder::new()
                .font(&FONT_6X10)
                .text_color(BinaryColor::On)
                .build();

            self.display
                .clear(BinaryColor::Off)
                .map_err(|_| Error::PrintError)?;

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

            let mut line4: String<32> = String::new();
            match state.status {
                Some(super::gps::Status::Autonomous) => {
                    write!(line4, "Status: Auto").map_err(|_| Error::PrintError)?
                }
                Some(super::gps::Status::Differential) => {
                    write!(line4, "Status: Diff").map_err(|_| Error::PrintError)?
                }
                Some(super::gps::Status::Invalid) => {
                    write!(line4, "Status: Invalid").map_err(|_| Error::PrintError)?
                }
                None => write!(line4, "Status: N/A").map_err(|_| Error::PrintError)?,
            }
            Text::with_baseline(&line4, Point::new(0, 30), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|_| Error::PrintError)?;

            self.display.flush().map_err(|_| Error::PrintError)?;
            Ok(())
        }
    }

    pub fn init(
        i2c: peripherals::TWISPI0,
        sda: peripherals::P0_04,
        scl: peripherals::P0_05,
    ) -> Result<Screen, Error> {
        Screen::new(i2c, sda, scl)
    }
}

mod usb {
    use embassy_executor::{SpawnError, Spawner};
    use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
    use embassy_nrf::usb::Driver;
    use embassy_nrf::{bind_interrupts, pac, peripherals, usb};
    use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
    use embassy_usb::driver::EndpointError;
    use embassy_usb::UsbDevice;
    use static_cell::StaticCell;

    bind_interrupts!(struct Irqs {
        USBD => usb::InterruptHandler<peripherals::USBD>;
        CLOCK_POWER => usb::vbus_detect::InterruptHandler;
    });

    pub struct Usb {
        class: CdcAcmClass<'static, Driver<'static, peripherals::USBD, HardwareVbusDetect>>,
    }

    impl Usb {
        pub async fn wait_connection(&mut self) {
            self.class.wait_connection().await;
        }

        pub async fn write_packet(&mut self, data: &[u8]) -> Result<(), EndpointError> {
            self.class.write_packet(data).await
        }
    }

    #[derive(Debug)]
    pub enum Error {
        SpawnError(SpawnError),
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Error::SpawnError(e) => write!(f, "failed to spawn USB task: {}", e),
            }
        }
    }

    impl core::error::Error for Error {}

    pub fn init(spawner: Spawner, usbd: peripherals::USBD) -> Result<Usb, Error> {
        pac::CLOCK.tasks_hfclkstart().write_value(1);
        while pac::CLOCK.events_hfclkstarted().read() != 1 {}

        // USB driver — hands the hardware peripheral to embassy-usb
        let driver = Driver::new(usbd, Irqs, HardwareVbusDetect::new(Irqs));

        // Buffers the USB stack needs — must be 'static
        static STATE: StaticCell<State> = StaticCell::new();
        static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static MSOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

        let state = STATE.init(State::new());
        let config_desc = CONFIG_DESCRIPTOR.init([0u8; 256]);
        let bos_desc = BOS_DESCRIPTOR.init([0u8; 256]);
        let msos_desc = MSOS_DESCRIPTOR.init([0u8; 256]);
        let control_buf = CONTROL_BUF.init([0u8; 64]);

        // Describe the USB device
        let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
        config.manufacturer = Some("Nikita");
        config.product = Some("Bike Computer");
        config.serial_number = Some("1");
        config.max_power = 100;

        // Build the USB device + attach CDC ACM class
        let mut builder = embassy_usb::Builder::new(
            driver,
            config,
            config_desc,
            bos_desc,
            msos_desc,
            control_buf,
        );
        let class = CdcAcmClass::new(&mut builder, state, 64);
        let usb = builder.build();

        #[embassy_executor::task]
        async fn usb_task(
            mut device: UsbDevice<'static, Driver<'static, peripherals::USBD, HardwareVbusDetect>>,
        ) {
            device.run().await;
        }

        spawner.spawn(usb_task(usb)).map_err(Error::SpawnError)?;
        Ok(Usb { class })
    }
}

mod logger {
    use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

    pub struct Logger;

    impl log::Log for Logger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            use core::fmt::Write;
            let mut msg: heapless::String<128> = heapless::String::new();
            let _ = write!(msg, "[{}] {}\r\n", record.level(), record.args());
            let _ = LOG_CHANNEL.try_send(msg);
        }

        fn flush(&self) {}
    }

    static LOG_CHANNEL: Channel<CriticalSectionRawMutex, heapless::String<128>, 32> =
        Channel::new();

    #[embassy_executor::task]
    async fn task(mut usb: super::usb::Usb) {
        loop {
            usb.wait_connection().await;
            loop {
                let msg = LOG_CHANNEL.receive().await;
                for chunk in msg.as_bytes().chunks(64) {
                    if usb.write_packet(chunk).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    static LOGGER: Logger = Logger;

    #[derive(Debug)]
    pub enum Error {
        SpawnError(embassy_executor::SpawnError),
        SetLoggerError,
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Error::SpawnError(e) => write!(f, "failed to spawn logger task: {}", e),
                Error::SetLoggerError => write!(f, "failed to set logger"),
            }
        }
    }

    impl core::error::Error for Error {}

    pub fn init(spawner: embassy_executor::Spawner, usb: super::usb::Usb) -> Result<(), Error> {
        spawner.spawn(task(usb)).map_err(Error::SpawnError)?;
        log::set_logger(&LOGGER).map_err(|_| Error::SetLoggerError)?;
        log::set_max_level(log::LevelFilter::Debug);
        Ok(())
    }
}
