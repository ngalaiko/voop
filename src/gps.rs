use embassy_nrf::uarte::{self, Uarte};
use embassy_nrf::{Peri, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

#[derive(Debug, Clone)]
pub enum Error {
    SpawnError(embassy_executor::SpawnError),
    ReadError(uarte::Error),
    ParseError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnError(e) => write!(f, "failed to spawn GPS task: {:?}", e),
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
    uarte0: Peri<'static, peripherals::UARTE0>,
    txd: Peri<'static, peripherals::P1_11>,
    rxd: Peri<'static, peripherals::P1_12>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_ch0: Peri<'static, peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, peripherals::PPI_CH1>,
) {
    let mut config = uarte::Config::default();
    config.baudrate = uarte::Baudrate::BAUD9600;
    let uarte = Uarte::new(uarte0, rxd, txd, crate::Irqs, config);
    let (_tx, mut rx) = uarte.split_with_idle(timer1, ppi_ch0, ppi_ch1);

    let mut buf = [0u8; 1024];
    loop {
        log::debug!("Waiting for GPS data...");
        match rx.read_until_idle(&mut buf).await {
            Ok(bytes_read) => {
                for line in buf[..bytes_read].split(|&b| b == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    let reading = nmea::parse_bytes(line).map_err(|_| Error::ParseError);
                    log::debug!(
                        "Raw GPS: {}",
                        core::str::from_utf8(line).unwrap_or("<invalid UTF-8>")
                    );
                    let mut state = STATE.lock().await;
                    match reading {
                        Ok(nmea::ParseResult::RMC(rmc)) => {
                            log::debug!("RMC: {:?}", rmc);
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
                            log::debug!("NMEA: {:?}", other);
                        }
                        Err(e) => {
                            log::error!("NMEA parse error: {:?}", e);
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
        STATE.lock().await.clone()
    }
}

pub fn new(
    spawner: embassy_executor::Spawner,
    uarte0: Peri<'static, peripherals::UARTE0>,
    txd: Peri<'static, peripherals::P1_11>,
    rxd: Peri<'static, peripherals::P1_12>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_ch0: Peri<'static, peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, peripherals::PPI_CH1>,
) -> Result<Gps, Error> {
    spawner
        .spawn(task(uarte0, txd, rxd, timer1, ppi_ch0, ppi_ch1).map_err(Error::SpawnError)?);
    Ok(Gps)
}
