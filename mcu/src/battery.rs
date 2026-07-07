use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::saadc::{self, ChannelConfig, Saadc, Time};
use embassy_nrf::{pac, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Ticker, Timer};
use voop_protocol::{BatteryState, BatteryStatus};

/// What the battery task publishes: the derived status plus the raw measurement behind it.
/// The screen shows the millivolts, so the divider scaling and curve can be sanity-checked
/// on-device against a multimeter.
#[derive(Clone, Copy, PartialEq)]
pub struct Reading {
    pub millivolts: u16,
    /// Raw VBUS presence — the same signal Charging is derived from, but available even
    /// when `status` is None. The screen uses it to stay lit on a dev bench.
    pub vbus: bool,
    /// None when nothing is attached to the XIAO's own BAT net (it floats near 0 V, far
    /// below any real Li-Po). That's the norm on the Expansion Board: its JST battery goes
    /// through the board's own ETA6096 charger into the 5 V rail, invisible to this divider
    /// — only a pack on the XIAO's BAT pads (the bare-XIAO build) is measurable here.
    pub status: Option<BatteryStatus>,
}

/// Below this the BAT net is floating, not a battery: a Li-Po that could still power
/// anything reads ≥ 3000 mV; the unconnected net reads tens of mV of decaying charge.
/// ⚠ Only trustworthy while VBUS is absent: with USB in, the unloaded BQ25100 drives the
/// empty net to a ~3.75–4.18 V charge/terminate/bleed sawtooth (observed 2026-07-07),
/// which reads as a plausible battery. Dev setups with no pack on the XIAO's pads should
/// not run this task at all (it's behind the `battery` feature).
const BATTERY_PRESENT_MV: u16 = 2500;

/// Latest MCU battery reading. Sent whenever the measurement moves (the percent itself is
/// hysteresis-dampened; see the loop). 1 receiver: the screen. The BLE peripheral reads
/// snapshots via `try_get()` instead of holding a slot — it only needs the current value at
/// status-notify time, never a wakeup.
pub static MCU_BATTERY: Watch<CriticalSectionRawMutex, Reading, 1> = Watch::new();

// XIAO nRF52840 Plus battery circuitry (Seeed_Studio_XIAO_nRF52840_Plus_SCH_PCB_v1.1) —
// this differs from the non-Plus boards:
//   - VBAT —R— P0.31 (AIN7) —R— P0.14: no MOSFET switch; P0.14 itself is the divider's
//     bottom leg. Output LOW to sink the divider current and read ("output Sink only" per
//     the schematic note — never drive it high into a 4.2 V pack). Held LOW for the whole
//     uptime: with the leg disconnected P0.31 floats to raw VBAT through the top resistor,
//     past the pin's VDD+0.3 V limit. The always-on divider costs ~3 µA.
//   - The BQ25100 charger has NO charge-status output — all six pins are power or current
//     programming. The net Seeed labels "P0.17_~CHG" actually lands on PRE_TERM (the
//     termination-current programming resistor), a leftover name from the non-Plus board
//     where P0.17 really read the charger's status. Do not bias P0.17 (a pull-up there
//     shifts the termination current); charging is inferred from VBUS presence instead.
//
// The schematic doesn't print the divider values; the numbers below are the non-Plus
// board's 1 MΩ / 510 kΩ (×0.338), which match the readings observed on hardware. If the
// percent is ever systematically off, recheck these before touching the curve.
const DIVIDER_TOP_KOHM: u64 = 1000;
const DIVIDER_BOTTOM_KOHM: u64 = 510;

// Resting-voltage curve for a 1S Li-Po, (millivolts, percent), descending. Interpolated
// linearly between entries, clamped outside. While on USB the pack sits at the charger's
// CC/CV voltage (up to 4.2 V), so the percent reads optimistic until unplugged — good
// enough for a status line; a coulomb counter is the only honest fix.
const CURVE: [(u16, u8); 10] = [
    (4200, 100),
    (4060, 90),
    (3980, 80),
    (3920, 70),
    (3870, 60),
    (3820, 50),
    (3790, 40),
    (3750, 30),
    (3680, 20),
    (3300, 0),
];

fn percent_from_mv(mv: u16) -> u8 {
    let Some(&(top_mv, top_pct)) = CURVE.first() else { return 0 };
    if mv >= top_mv {
        return top_pct;
    }
    for pair in CURVE.windows(2) {
        let (hi_mv, hi_pct) = pair[0];
        let (lo_mv, lo_pct) = pair[1];
        if mv >= lo_mv {
            let span_mv = (hi_mv - lo_mv) as u32;
            let span_pct = (hi_pct - lo_pct) as u32;
            return lo_pct + ((mv - lo_mv) as u32 * span_pct / span_mv) as u8;
        }
    }
    0
}

/// Whether USB power is attached, straight from POWER.USBREGSTATUS (a side-effect-free
/// read; the USB driver's HardwareVbusDetect polls the same register). The BQ25100 charges
/// whenever VBUS is present and the pack isn't full, so this is the closest available proxy
/// for "charging" — a full pack on USB still reads Charging, which the status line can live
/// with.
fn vbus_present() -> bool {
    pac::POWER.usbregstatus().read().vbusdetect()
}

pub struct Battery {
    saadc: Peri<'static, peripherals::SAADC>,
    vbat: Peri<'static, peripherals::P0_31>,
    read_bat: Peri<'static, peripherals::P0_14>,
}

pub fn init(
    saadc: Peri<'static, peripherals::SAADC>,
    vbat: Peri<'static, peripherals::P0_31>,
    read_bat: Peri<'static, peripherals::P0_14>,
) -> Battery {
    Battery { saadc, vbat, read_bat }
}

/// One VBAT measurement in millivolts: 4 conversions averaged (SAADC noise spans a few
/// LSB ≈ tens of mV once the divider is scaled back up), then undo the divider against the
/// 3.6 V full scale (12-bit, internal 0.6 V reference, 1/6 gain).
async fn sample_mv(adc: &mut Saadc<'_, 1>) -> u16 {
    let mut sum: i32 = 0;
    for _ in 0..4 {
        let mut buf = [0i16; 1];
        adc.sample(&mut buf).await;
        sum += i32::from(buf[0].max(0));
    }
    let raw = (sum / 4) as u64;
    ((raw * 3600 * (DIVIDER_TOP_KOHM + DIVIDER_BOTTOM_KOHM)) / (DIVIDER_BOTTOM_KOHM * 4096)) as u16
}

/// Median of three tick readings — a single-tick outlier can never move it. Field logs
/// showed lone ~400 mV excursions walking straight into the percent: charger/load
/// transients outlast `sample_mv`'s ~200 µs averaging window, so all four conversions
/// agree on the lie and only cross-tick filtering catches it.
fn median3(a: u16, b: u16, c: u16) -> u16 {
    a.min(b).max(a.max(b).min(c))
}

impl Battery {
    pub async fn run(self) {
        // Divider sink on for the whole uptime (see the P0.31 overvoltage note above).
        let _read_bat = Output::new(self.read_bat, Level::Low, OutputDrive::Standard);

        let mut channel = ChannelConfig::single_ended(self.vbat);
        // The divider's ~340 kΩ source impedance needs a long acquisition window; the
        // default 10 µs is only rated to 100 kΩ and undercharges the sample cap.
        channel.time = Time::_40US;
        let mut adc = Saadc::new(self.saadc, crate::Irqs, saadc::Config::default(), [channel]);
        adc.calibrate().await;

        // The first conversions after offset calibration read low garbage (they showed up
        // as a hard 0% at boot); publish nothing until two consecutive readings agree.
        let mut vbat_mv = sample_mv(&mut adc).await;
        for _ in 0..10 {
            Timer::after(Duration::from_millis(50)).await;
            let mv = sample_mv(&mut adc).await;
            let stable = mv.abs_diff(vbat_mv) <= 20;
            vbat_mv = mv;
            if stable {
                break;
            }
        }

        // Ring of the last 3 tick readings, seeded with the settled boot value; percent
        // and battery presence derive from its median (see median3).
        let mut recent = [vbat_mv; 3];
        let mut idx = 0;

        let sender = MCU_BATTERY.sender();
        let mut last: Option<Reading> = None;
        // 2 s cadence does double duty: VBUS plug/unplug shows up promptly, and the percent
        // re-derives from the post-plug/unplug voltage jump (charger CV vs. resting) without
        // waiting out a long tick. A tick is one register read + ~160 µs of ADC — noise next
        // to the screen's always-on 1 s ticker.
        let mut ticker = Ticker::every(Duration::from_secs(2));

        loop {
            let vbus = vbus_present();
            let state = if vbus { BatteryState::Charging } else { BatteryState::Discharging };

            // Two damping layers, both tuned against field logs. The 3-tick median absorbs
            // lone transient ticks; but adjacent readings still straddle percent boundaries
            // (steps sit only ~3 mV apart in the flat middle of the curve), so the published
            // percent additionally holds until the candidate moves by MORE than 2 points —
            // an observed 31↔33 flap straddles exactly 2 — or the charge state flips. The
            // raw millivolts are published undamped — they're the calibration readout.
            let median = median3(recent[0], recent[1], recent[2]);
            let status = (median >= BATTERY_PRESENT_MV).then(|| {
                let candidate = percent_from_mv(median);
                let percent = match last.and_then(|l| l.status) {
                    Some(l) if l.state == state && l.percent.abs_diff(candidate) <= 2 => {
                        l.percent
                    }
                    _ => candidate,
                };
                BatteryStatus { percent, state }
            });

            let reading = Reading { millivolts: vbat_mv, vbus, status };
            if last.map(|l| l.status) != Some(reading.status) {
                match status {
                    Some(s) => {
                        log::info!("[Battery] {} mV, {}%, {:?}", vbat_mv, s.percent, s.state)
                    }
                    None => log::info!("[Battery] {} mV, no battery on BAT net", vbat_mv),
                }
            }
            if last != Some(reading) {
                sender.send(reading);
                last = Some(reading);
            }

            ticker.next().await;
            vbat_mv = sample_mv(&mut adc).await;
            recent[idx] = vbat_mv;
            idx = (idx + 1) % 3;
        }
    }
}
