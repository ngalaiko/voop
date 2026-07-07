use chrono::{Datelike as _, Timelike as _};
use embassy_futures::select::{select, Either};
use embassy_nrf::uarte::{self, Uarte};
use embassy_nrf::{peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, with_timeout};

#[derive(Clone, Copy)]
pub struct GpsState {
    pub lat_microdeg: i32,
    pub lon_microdeg: i32,
}

pub static GPS: Watch<CriticalSectionRawMutex, Option<GpsState>, 2> = Watch::new();

// If no NMEA arrives for this long, consider the GPS silent and report loss of fix.
const GPS_SILENCE_TIMEOUT: Duration = Duration::from_secs(5);

// After this many consecutive silent windows while awake, assume the module is stuck
// (most likely: standby didn't release on UART traffic) and hot-restart it.
const SILENT_WINDOWS_BEFORE_RESTART: u32 = 3;

// Air530 power commands (cross-checked against the vendor-datasheet translations in the
// Meshtastic Air530 driver and the LilyGO T-Watch library):
//   - "$PGKC051,0" enters standby: ~0.85 mA vs ~37-43 mA tracking/acquiring. RAM stays
//     powered, so the next fix is a hot start (seconds).
//   - Deeper modes ("$PGKC105,4", ~31 µA) need the WAKE pin, which the Grove cable
//     doesn't carry — UART is all we have. True power-off needs a FET in Grove VCC.
//   - The references disagree on whether UART traffic wakes it from PGKC051 standby
//     (LilyGO only ever uses the WAKE pin), so the wake path sends bytes and, if the
//     module stays silent, the watchdog escalates to a hot restart ("$PGKC030,1,1") —
//     that restores full operation with ephemeris intact. Verify on hardware.
const STANDBY_CMD: &[u8] = b"$PGKC051,0*37\r\n";
const HOT_RESTART_CMD: &[u8] = b"$PGKC030,1,1*2C\r\n";

#[derive(Clone, Copy, Debug, PartialEq)]
enum FixQuality {
    Autonomous,
    Differential,
}

pub struct Gps {
    uarte0: Peri<'static, peripherals::UARTE0>,
    txd: Peri<'static, peripherals::P1_11>,
    rxd: Peri<'static, peripherals::P1_12>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_ch0: Peri<'static, peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, peripherals::PPI_CH1>,
}

pub fn init(
    uarte0: Peri<'static, peripherals::UARTE0>,
    txd: Peri<'static, peripherals::P1_11>,
    rxd: Peri<'static, peripherals::P1_12>,
    timer1: Peri<'static, peripherals::TIMER1>,
    ppi_ch0: Peri<'static, peripherals::PPI_CH0>,
    ppi_ch1: Peri<'static, peripherals::PPI_CH1>,
) -> Gps {
    Gps { uarte0, txd, rxd, timer1, ppi_ch0, ppi_ch1 }
}

impl Gps {
    pub async fn run(self) {
        let mut config = uarte::Config::default();
        config.baudrate = uarte::Baudrate::BAUD9600;
        let uarte = Uarte::new(self.uarte0, self.rxd, self.txd, crate::Irqs, config);
        let (mut tx, mut rx) = uarte.split_with_idle(self.timer1, self.ppi_ch0, self.ppi_ch1);

        let Some(mut gate) = crate::power::GPS_ENABLED.receiver() else {
            log::error!("[GPS] GPS_ENABLED: no free receiver slot");
            return;
        };

        let gps_tx = GPS.sender();
        let mut had_fix = false;
        let mut silent_windows = 0u32;

        let mut buf = [0u8; 1024];
        loop {
            // Gate closed → standby. The module powers up acquiring at boot, so a parked
            // boot goes straight to sleep here too.
            if !gate.get().await {
                gps_tx.send(None);
                had_fix = false;
                if let Err(e) = tx.write(STANDBY_CMD).await {
                    log::warn!("[GPS] standby write error: {:?}", e);
                }
                log::info!("[GPS] standby");
                loop {
                    if gate.get().await {
                        break;
                    }
                    gate.changed().await;
                }
                // Any UART traffic should release PGKC051 standby; if it doesn't, the
                // silence watchdog below hot-restarts the module.
                if let Err(e) = tx.write(b"\r\n").await {
                    log::warn!("[GPS] wake write error: {:?}", e);
                }
                silent_windows = 0;
                log::info!("[GPS] wake");
            }

            let gate_closed = async {
                loop {
                    if !gate.get().await {
                        break;
                    }
                    gate.changed().await;
                }
            };
            match select(
                gate_closed,
                with_timeout(GPS_SILENCE_TIMEOUT, rx.read_until_idle(&mut buf)),
            )
            .await
            {
                // Power policy pulled the plug — loop back around into standby. A read
                // cancelled mid-sentence is fine: the parser resyncs on the next newline.
                Either::First(()) => continue,
                // No NMEA for the whole window — the GPS has gone silent. Report loss of fix
                // so consumers stop stamping the last known position; if it stays silent,
                // assume a wedged module (standby that ignored the wake bytes) and restart.
                Either::Second(Err(_)) => {
                    if had_fix {
                        gps_tx.send(None);
                        had_fix = false;
                    }
                    silent_windows += 1;
                    if silent_windows.is_multiple_of(SILENT_WINDOWS_BEFORE_RESTART) {
                        log::warn!("[GPS] silent {}s, hot-restarting", silent_windows * 5);
                        if let Err(e) = tx.write(HOT_RESTART_CMD).await {
                            log::warn!("[GPS] restart write error: {:?}", e);
                        }
                    }
                }
                Either::Second(Ok(Ok(n))) => {
                    silent_windows = 0;
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
                                match (fix_quality, rmc.lat, rmc.lon) {
                                    (Some(_fix), Some(lat), Some(lon))
                                        if (-90.0..=90.0).contains(&lat)
                                            && (-180.0..=180.0).contains(&lon) =>
                                    {
                                        gps_tx.send(Some(GpsState {
                                            lat_microdeg: (lat * 1_000_000.0) as i32,
                                            lon_microdeg: (lon * 1_000_000.0) as i32,
                                        }));
                                        had_fix = true;
                                    }
                                    // No valid fix — or a garbage/out-of-range coordinate — this
                                    // sentence. Emit one loss event on the transition so consumers
                                    // stop stamping stale coordinates.
                                    _ => {
                                        if had_fix {
                                            gps_tx.send(None);
                                            had_fix = false;
                                        }
                                    }
                                }
                                // Anchor the wall clock only from sentences with a valid fix.
                                // Pre-fix RMC (status V) carries RTC/default date-time — often
                                // years off — and at 1 Hz it would out-shout the one-shot iOS
                                // time sync, poisoning every DataPoint's unix_millis.
                                if fix_quality.is_some() {
                                    if let (Some(date), Some(time)) = (rmc.fix_date, rmc.fix_time)
                                    {
                                        let epoch = to_unix_epoch(
                                            date.year(),
                                            date.month() as u8,
                                            date.day() as u8,
                                            time.hour() as u8,
                                            time.minute() as u8,
                                            time.second() as u8,
                                        );
                                        crate::clock::set(epoch).await;
                                    }
                                }
                            }
                            Ok(_) | Err(_) => {}
                        }
                    }
                }
                Either::Second(Ok(Err(e))) => log::debug!("[GPS] read error: {:?}", e),
            }
        }
    }
}

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
