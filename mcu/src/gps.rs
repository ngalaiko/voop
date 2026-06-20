use chrono::{Datelike as _, Timelike as _};
use embassy_nrf::uarte::{self, Uarte};
use embassy_nrf::{peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;

#[derive(Clone, Debug)]
pub enum FixQuality {
    Autonomous,
    Differential,
}

#[derive(Clone, Debug)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub fix_quality: FixQuality,
}

#[derive(Clone, Debug)]
pub enum Error {
    SpawnFailed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnFailed => write!(f, "failed to spawn GPS task"),
        }
    }
}

impl core::error::Error for Error {}

// Sent on each valid fix. Fix quality affects precision.
pub static LOCATION: Watch<CriticalSectionRawMutex, Result<Location, Error>, 2> = Watch::new();

// Unix timestamp — sent on every valid RMC sentence with date+time.
pub static TIME: Watch<CriticalSectionRawMutex, Result<u32, Error>, 1> = Watch::new();

// Howard Hinnant's civil-to-days algorithm → Unix epoch.
fn to_unix_epoch(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u32 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u32;
    let m = month as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as u32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i32 - 719468;
    days as u32 * 86400 + hour as u32 * 3600 + minute as u32 * 60 + second as u32
}

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

    let location_tx = LOCATION.sender();
    let time_tx = TIME.sender();

    let mut buf = [0u8; 1024];
    loop {
        match rx.read_until_idle(&mut buf).await {
            Ok(n) => {
                for line in buf[..n].split(|&b| b == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    match nmea::parse_bytes(line) {
                        Ok(nmea::ParseResult::RMC(rmc)) => {
                            let fix_quality = match rmc.status_of_fix {
                                nmea::sentences::rmc::RmcStatusOfFix::Autonomous => {
                                    Some(FixQuality::Autonomous)
                                }
                                nmea::sentences::rmc::RmcStatusOfFix::Differential => {
                                    Some(FixQuality::Differential)
                                }
                                _ => None,
                            };
                            if let (Some(fix_quality), Some(lat), Some(lon)) =
                                (fix_quality, rmc.lat, rmc.lon)
                            {
                                location_tx.send(Ok(Location {
                                    lat,
                                    lon,
                                    fix_quality,
                                }));
                            }
                            if let (Some(date), Some(time)) = (rmc.fix_date, rmc.fix_time) {
                                let epoch = to_unix_epoch(
                                    date.year(),
                                    date.month() as u8,
                                    date.day() as u8,
                                    time.hour() as u8,
                                    time.minute() as u8,
                                    time.second() as u8,
                                );
                                time_tx.send(Ok(epoch));
                            }
                        }
                        Ok(_) | Err(_) => {}
                    }
                }
            }
            Err(e) => log::debug!("[GPS] read error: {:?}", e),
        }
    }
}

pub fn init(
    spawner: embassy_executor::Spawner,
    uarte0: Peri<'static, peripherals::UARTE0>,
    txd: Peri<'static, peripherals::P1_11>,
    rxd: Peri<'static, peripherals::P1_12>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_ch0: Peri<'static, peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, peripherals::PPI_CH1>,
) -> Result<(), Error> {
    spawner
        .spawn(task(uarte0, txd, rxd, timer1, ppi_ch0, ppi_ch1).map_err(|_| Error::SpawnFailed)?);
    Ok(())
}
