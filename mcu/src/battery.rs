use embassy_futures::select::select;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::saadc::{self, ChannelConfig, Saadc, Time};
use embassy_nrf::{peripherals, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Ticker};
use voop_protocol::{BatteryState, BatteryStatus};

/// Latest MCU battery reading. Sent only when percent or charge state changes.
/// 1 receiver: the screen. The BLE peripheral reads snapshots via `try_get()` instead of
/// holding a slot — it only needs the current value at status-notify time, never a wakeup.
pub static MCU_BATTERY: Watch<CriticalSectionRawMutex, BatteryStatus, 1> = Watch::new();

// XIAO nRF52840 battery circuitry, both stock and Plus:
//   - VBAT feeds a 1 MΩ / 510 kΩ divider (×0.338) into P0.31 = AIN7. The divider hangs off a
//     MOSFET gated by P0.14: LOW connects it. Seeed's guidance is to keep P0.14 LOW whenever
//     a battery is attached — with the divider disconnected P0.31 floats toward raw VBAT
//     (up to 4.2 V) through the 1 MΩ leg, over the pin's VDD+0.3 V limit. The always-on
//     divider costs VBAT/1.51 MΩ ≈ 3 µA, noise next to the GPS/BLE/OLED budget.
//   - The BQ25101 charger's open-drain CHG status lands on P0.17 (it also drives the onboard
//     charge LED): LOW while charging, released when done or on USB-only/no-battery.
const DIVIDER_TOP_KOHM: u64 = 1000;
const DIVIDER_BOTTOM_KOHM: u64 = 510;

// Resting-voltage curve for a 1S Li-Po, (millivolts, percent), descending. Interpolated
// linearly between entries, clamped outside. While charging the pack sits at the charger's
// CC/CV voltage (up to 4.2 V), so the percent reads optimistic until the charger is unplugged
// — good enough for a status line; a coulomb counter is the only honest fix.
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

pub struct Battery {
    saadc: Peri<'static, peripherals::SAADC>,
    vbat: Peri<'static, peripherals::P0_31>,
    vbat_en: Peri<'static, peripherals::P0_14>,
    chg: Peri<'static, peripherals::P0_17>,
}

pub fn init(
    saadc: Peri<'static, peripherals::SAADC>,
    vbat: Peri<'static, peripherals::P0_31>,
    vbat_en: Peri<'static, peripherals::P0_14>,
    chg: Peri<'static, peripherals::P0_17>,
) -> Battery {
    Battery { saadc, vbat, vbat_en, chg }
}

impl Battery {
    pub async fn run(self) {
        // Divider on for the whole uptime (see the P0.31 overvoltage note above).
        let _vbat_en = Output::new(self.vbat_en, Level::Low, OutputDrive::Standard);

        // CHG is open-drain; the onboard LED path pulls it up, the internal pull-up just
        // makes the read deterministic if that path ever floats.
        let mut chg = Input::new(self.chg, Pull::Up);

        let mut channel = ChannelConfig::single_ended(self.vbat);
        // The defaults (internal 0.6 V reference, 1/6 gain) give a 3.6 V full scale — the
        // divided VBAT tops out around 1.44 V. But the divider's ~340 kΩ source impedance
        // needs the longest acquisition window; the default 10 µs is only rated to 100 kΩ
        // and undercharges the sample cap, reading low.
        channel.time = Time::_40US;
        let mut adc = Saadc::new(self.saadc, crate::Irqs, saadc::Config::default(), [channel]);
        adc.calibrate().await;

        let sender = MCU_BATTERY.sender();
        let mut last: Option<BatteryStatus> = None;
        let mut ticker = Ticker::every(Duration::from_secs(30));

        loop {
            // Average a few conversions to knock down SAADC noise (~a few LSB ≈ tens of mV
            // after the divider is scaled back up, enough to wobble a percent step).
            let mut sum: i32 = 0;
            for _ in 0..4 {
                let mut buf = [0i16; 1];
                adc.sample(&mut buf).await;
                sum += i32::from(buf[0].max(0));
            }
            let raw = (sum / 4) as u64;

            // 12-bit single-ended: raw/4096 of the 3.6 V full scale, then undo the divider.
            let vbat_mv =
                (raw * 3600 * (DIVIDER_TOP_KOHM + DIVIDER_BOTTOM_KOHM) / DIVIDER_BOTTOM_KOHM / 4096) as u16;

            let status = BatteryStatus {
                percent: percent_from_mv(vbat_mv),
                state: if chg.is_low() { BatteryState::Charging } else { BatteryState::Discharging },
            };

            if last != Some(status) {
                log::info!("[Battery] {} mV, {}%, {:?}", vbat_mv, status.percent, status.state);
                sender.send(status);
                last = Some(status);
            }

            // Charge level drifts slowly; plugging/unplugging USB shouldn't wait out the
            // ticker, so a CHG edge re-samples immediately.
            select(ticker.next(), chg.wait_for_any_edge()).await;
        }
    }
}
