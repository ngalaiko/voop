use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::{peripherals, twim, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{with_timeout, Duration, Timer};
use static_cell::StaticCell;

/// Whether the device is currently moving, per the onboard accelerometer. Coarse on/off only.
/// 2 receivers: the screen + the power policy.
pub static MOVING: Watch<CriticalSectionRawMutex, bool, 2> = Watch::new();

/// Whether the IMU is answering at all. Sent false after repeated bring-up failures so the
/// power policy can fall back to sensor-driven gating instead of waiting for motion events
/// that will never come — a real failure mode: one Sense Plus unit in the field has its
/// internal I2C bus clamped low by a defective sensor (both lines held against pull-ups,
/// either power polarity; diagnosed 2026-07-07). 1 receiver: the power policy.
pub static HEALTHY: Watch<CriticalSectionRawMutex, bool, 1> = Watch::new();

// Declare the IMU unavailable after this many failed bring-ups (~30 s). Retries continue —
// a later success flips HEALTHY back and normal motion gating resumes.
const ATTEMPTS_BEFORE_UNHEALTHY: u32 = 6;

// LSM6DS3TR-C, onboard the XIAO nRF52840 Sense. It hangs off the *internal* sensor I2C bus
// (SDA=P0.07, SCL=P0.27) — distinct from the Expansion Board OLED's external bus — at address
// 0x6A, powered through a load switch on P1.08 (drive high to turn it on), with its INT1 line
// on P0.11.
//
// This is battery-preserving by design: instead of the MCU polling the accelerometer, the chip
// runs its own wake-up (activity) engine in low-power mode (~tens of µA) and only pulls INT1
// when acceleration crosses a threshold. The MCU task then just `await`s that GPIO edge — it
// idles (and the executor sleeps the core) whenever the bike is parked, waking on real motion.
// The same INT1 edge is what a future deep-sleep build wires to wake the whole SoC.
const ADDR: u8 = 0x6A;
const REG_WHO_AM_I: u8 = 0x0F;
const WHO_AM_I_LSM6DS3TR_C: u8 = 0x6A;
const REG_CTRL1_XL: u8 = 0x10;
const REG_CTRL3_C: u8 = 0x12;
const REG_CTRL6_C: u8 = 0x15;
const REG_WAKE_UP_SRC: u8 = 0x1B;
const REG_WAKE_UP_DUR: u8 = 0x5C;
const REG_WK_THS: u8 = 0x5B;
const REG_TAP_CFG: u8 = 0x58;
const REG_MD1_CFG: u8 = 0x5E;

// CTRL1_XL: ODR_XL=0b0011 (52 Hz) | FS_XL=0b00 (±2 g) → 0x30. 52 Hz is ample latency for
// waking on motion, and cheap.
const CTRL1_XL_52HZ_2G: u8 = 0x30;
// CTRL3_C: BDU=1 (bit 6) + IF_INC=1 (bit 2, default) → 0x44. (No output reads here, but keep
// the sane defaults.)
const CTRL3_C_BDU_IFINC: u8 = 0x44;
// CTRL6_C: XL_HM_MODE=1 (bit 4) disables high-performance mode → the accelerometer runs in
// low-power mode at this ODR (~tens of µA instead of ~0.5 mA). This is the battery lever.
const CTRL6_C_XL_LOW_POWER: u8 = 0x10;
// TAP_CFG: INTERRUPTS_ENABLE=1 (bit 7) turns on the basic-interrupt block (wake-up et al.) and
// its slope filter, which removes the constant 1 g so only *changes* in acceleration count.
// LIR=1 (bit 0) latches the interrupt: INT1 stays high until WAKE_UP_SRC is read. Non-latched
// pulses could land between awaits (missed re-arm) and stay *continuously* high under sustained
// acceleration (no edges at all) — latched + clear-on-read gives one clean level per event.
const TAP_CFG_ENABLE_INTS: u8 = 0x81;
// MD1_CFG: INT1_WU=1 (bit 5) routes the wake-up event to the INT1 pin.
const MD1_CFG_INT1_WU: u8 = 0x20;
// WAKE_UP_DUR = 0 → fire on the first sample over threshold (no debounce).
const WAKE_UP_DUR_IMMEDIATE: u8 = 0x00;

// Wake-up threshold, WK_THS[5:0]. LSB = FS_XL / 2^6 = 2000 mg / 64 = 31.25 mg at ±2 g.
// 8 LSB ≈ 250 mg: comfortably above parked noise / wind / a nudge, well below the accelerations
// of a bike being handled or ridden ("movements are quite big"). Raise it if it wakes when
// parked; lower it if it misses a gentle roll-away. INT1 is push-pull active-high by default,
// so watch it go high on real motion.
const WK_THS: u8 = 0x08;

// Once motion is seen, hold "moving" this long past the last jolt so the flag doesn't chatter
// between the wake pulses of individual jolts, or over a brief stop at a light.
const MOTION_HOLD: Duration = Duration::from_secs(3);

// Every I2C op is timeout-guarded: an unpowered or absent chip drags the pulled-up bus low
// through its pads, and a low line makes TWIM wait forever *without* raising an error event
// — the un-guarded version of this task parked silently on its first transaction (observed
// on the Sense Plus, 2026-07-07) and motion detection just never came up.
const I2C_OP_TIMEOUT: Duration = Duration::from_millis(250);

pub struct Imu {
    twim: Peri<'static, peripherals::TWISPI1>,
    sda: Peri<'static, peripherals::P0_07>,
    scl: Peri<'static, peripherals::P0_27>,
    pwr: Peri<'static, peripherals::P1_08>,
    int1: Peri<'static, peripherals::P0_11>,
}

pub fn init(
    twim: Peri<'static, peripherals::TWISPI1>,
    sda: Peri<'static, peripherals::P0_07>,
    scl: Peri<'static, peripherals::P0_27>,
    pwr: Peri<'static, peripherals::P1_08>,
    int1: Peri<'static, peripherals::P0_11>,
) -> Imu {
    Imu { twim, sda, scl, pwr, int1 }
}

impl Imu {
    pub async fn run(self) {
        // The IMU (and the PDM mic) sit behind a load switch on P1.08 — power it and let the
        // chip boot before the first transaction. Held for the whole task so it stays powered.
        let mut power = Output::new(self.pwr, Level::High, OutputDrive::Standard);
        Timer::after(Duration::from_millis(50)).await;

        let mut twim_p = self.twim;
        let mut sda = self.sda;
        let mut scl = self.scl;
        let mut tx_buf = [0u8; 16];

        // Bring-up with retry: a transient NACK at boot must not kill motion detection for
        // the whole uptime — the IMU is the wake source for everything else. Each attempt
        // (not just boot, so any log capture window sees it):
        //   1. Probe SDA/SCL as plain inputs — with pull-ups an idle healthy bus reads high
        //      on both; a line that stays low is held by something outside this firmware,
        //      and TWIM waits on a held line forever.
        //   2. If SDA is held while SCL is free (the classic stuck-slave state: a read
        //      abandoned mid-byte leaves the slave driving a zero until it sees clocks),
        //      run the standard 9-pulse bus clear. Push-pull on SCL is safe precisely
        //      because nobody was holding it.
        //   3. Try the LSM bring-up over a freshly created TWIM (rebuilt per attempt so
        //      the pins are free for the probe); on failure, sweep the bus so whatever is
        //      out there identifies itself (newer Sense boards ship a BMI270 at 0x68
        //      instead of the LSM6DS3TR-C at 0x6A), then flip the power-gate polarity in
        //      case this board revision inverted it (a P-FET gate powers on LOW, and a
        //      dark chip's pads hold the bus low).
        let level = |high| if high { "high" } else { "LOW (held)" };
        let mut attempt = 0u32;
        loop {
            attempt += 1;

            let (sda_high, scl_high) = {
                let sda_probe = Input::new(sda.reborrow(), Pull::Up);
                let scl_probe = Input::new(scl.reborrow(), Pull::Up);
                Timer::after(Duration::from_millis(2)).await;
                (sda_probe.is_high(), scl_probe.is_high())
            };
            log::info!("[IMU] bus idle: SDA {}, SCL {}", level(sda_high), level(scl_high));

            if !sda_high && scl_high {
                {
                    let mut scl_out =
                        Output::new(scl.reborrow(), Level::High, OutputDrive::Standard);
                    for _ in 0..9 {
                        scl_out.set_low();
                        Timer::after_micros(100).await;
                        scl_out.set_high();
                        Timer::after_micros(100).await;
                    }
                }
                let cleared = {
                    let sda_probe = Input::new(sda.reborrow(), Pull::Up);
                    Timer::after(Duration::from_millis(1)).await;
                    sda_probe.is_high()
                };
                log::info!("[IMU] bus clear: SDA now {}", level(cleared));
            }

            let ok = {
                let mut config = twim::Config::default();
                // The board should carry external pull-ups on the internal bus; enable the
                // internal ones too as cheap insurance (they parallel, fine at 100 kHz).
                config.sda_pullup = true;
                config.scl_pullup = true;
                let mut i2c = twim::Twim::new(
                    twim_p.reborrow(),
                    crate::Irqs,
                    sda.reborrow(),
                    scl.reborrow(),
                    config,
                    &mut tx_buf,
                );
                let ok = bring_up(&mut i2c).await;
                if !ok {
                    scan_bus(&mut i2c).await;
                }
                ok
            };
            if ok {
                break;
            }
            if attempt == ATTEMPTS_BEFORE_UNHEALTHY {
                log::error!("[IMU] unavailable after {} attempts, power policy degrades", attempt);
                HEALTHY.sender().send(false);
            }

            let (name, pwr_level) =
                if attempt % 2 == 0 { ("high", Level::High) } else { ("low", Level::Low) };
            power.set_level(pwr_level);
            log::error!(
                "[IMU] bring-up failed (attempt {}), retrying with 6D_PWR {}",
                attempt,
                name
            );
            Timer::after(Duration::from_secs(5)).await;
        }
        HEALTHY.sender().send(true);
        log::info!("[IMU] LSM6DS3TR-C armed for wake-on-motion");

        // The chip is configured; reopen the bus one last time for the runtime loop (the
        // per-attempt handle had to die with each attempt to free the pins for probing).
        static TX_BUF: StaticCell<[u8; 16]> = StaticCell::new();
        let mut config = twim::Config::default();
        config.sda_pullup = true;
        config.scl_pullup = true;
        let mut i2c = twim::Twim::new(
            twim_p,
            crate::Irqs,
            sda,
            scl,
            config,
            TX_BUF.init([0u8; 16]),
        );

        // I2C is done — the chip drives everything from here. The pin is push-pull active-high
        // (LSM6DS3TR-C default), so no pull is needed.
        let mut int1 = Input::new(self.int1, Pull::None);

        let moving_tx = MOVING.sender();
        moving_tx.send(false);

        loop {
            // Parked: the MCU idles here until the IMU flags motion. No polling.
            // wait_for_high is level-sensed, so a latched event raised before this point is
            // seen immediately — nothing to miss.
            int1.wait_for_high().await;
            moving_tx.send(true);
            log::info!("[IMU] motion");

            // Moving: clear the latched event, then wait for the next one. Sustained or
            // repeated motion re-raises INT1 right away and re-arms the hold window; only a
            // full quiet window ends the "moving" state.
            loop {
                let mut src = [0u8; 1];
                let _ = with_timeout(
                    I2C_OP_TIMEOUT,
                    i2c.write_read(ADDR, &[REG_WAKE_UP_SRC], &mut src),
                )
                .await;
                match select(int1.wait_for_high(), Timer::after(MOTION_HOLD)).await {
                    Either::First(()) => continue,
                    Either::Second(()) => break,
                }
            }
            moving_tx.send(false);
            log::info!("[IMU] still");
        }
    }
}

/// One bring-up attempt: verify WHO_AM_I, arm the wake-up engine, let the accelerometer
/// settle, and clear any spurious power-on wake event (the engine can fire right after
/// CTRL1_XL enable — ST AN5130 recommends discarding the settling period).
async fn bring_up(i2c: &mut twim::Twim<'_>) -> bool {
    // Confirm we're talking to the chip we expect before trusting anything.
    let mut who = [0u8; 1];
    match with_timeout(I2C_OP_TIMEOUT, i2c.write_read(ADDR, &[REG_WHO_AM_I], &mut who)).await {
        Ok(Ok(())) if who[0] == WHO_AM_I_LSM6DS3TR_C => {}
        Ok(Ok(())) => {
            log::error!("[IMU] unexpected WHO_AM_I: {:#04x}", who[0]);
            return false;
        }
        Ok(Err(e)) => {
            log::error!("[IMU] WHO_AM_I read failed: {:?}", e);
            return false;
        }
        Err(_) => {
            log::error!("[IMU] WHO_AM_I timed out — bus stuck (chip unpowered or absent?)");
            return false;
        }
    }

    // Bring up the accelerometer in low-power mode and arm the wake-up interrupt on INT1.
    let setup: [[u8; 2]; 7] = [
        [REG_CTRL3_C, CTRL3_C_BDU_IFINC],
        [REG_CTRL6_C, CTRL6_C_XL_LOW_POWER],
        [REG_CTRL1_XL, CTRL1_XL_52HZ_2G],
        [REG_WK_THS, WK_THS],
        [REG_WAKE_UP_DUR, WAKE_UP_DUR_IMMEDIATE],
        [REG_TAP_CFG, TAP_CFG_ENABLE_INTS],
        [REG_MD1_CFG, MD1_CFG_INT1_WU],
    ];
    for reg in &setup {
        match with_timeout(I2C_OP_TIMEOUT, i2c.write(ADDR, reg)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                log::error!("[IMU] register {:#04x} write failed: {:?}", reg[0], e);
                return false;
            }
            Err(_) => {
                log::error!("[IMU] register {:#04x} write timed out", reg[0]);
                return false;
            }
        }
    }

    // ~5 samples at 52 Hz for the slope filter to settle, then discard the phantom event.
    Timer::after(Duration::from_millis(100)).await;
    let mut src = [0u8; 1];
    let _ =
        with_timeout(I2C_OP_TIMEOUT, i2c.write_read(ADDR, &[REG_WAKE_UP_SRC], &mut src)).await;
    true
}

/// Sweep the bus and log every ACK with the value of register 0x00 — enough for the
/// hardware to identify itself: a BMI270 (what newer Sense revisions ship instead of the
/// LSM6DS3TR-C) ACKs at 0x68 and reads 0x24 (its CHIP_ID) at register 0.
async fn scan_bus(i2c: &mut twim::Twim<'_>) {
    let mut found = false;
    for addr in 0x08..=0x77u8 {
        let mut reg0 = [0u8; 1];
        match with_timeout(I2C_OP_TIMEOUT, i2c.write_read(addr, &[0x00], &mut reg0)).await {
            Ok(Ok(())) => {
                found = true;
                log::info!("[IMU] scan: {:#04x} ACK, reg0={:#04x}", addr, reg0[0]);
            }
            Ok(Err(_)) => {}
            Err(_) => {
                log::error!("[IMU] scan: bus stuck at {:#04x}, aborting sweep", addr);
                return;
            }
        }
    }
    if !found {
        log::info!("[IMU] scan: no devices ACKed");
    }
}
