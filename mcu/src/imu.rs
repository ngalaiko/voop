use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::{peripherals, twim, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;

/// Whether the device is currently moving, per the onboard accelerometer. Coarse on/off only.
/// 2 receivers: the screen now; the BLE central later — the long-term plan is to let motion
/// wake up cadence-sensor discovery instead of scanning around the clock.
pub static MOVING: Watch<CriticalSectionRawMutex, bool, 2> = Watch::new();

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
        let _power = Output::new(self.pwr, Level::High, OutputDrive::Standard);
        Timer::after(Duration::from_millis(50)).await;

        static TX_BUF: StaticCell<[u8; 16]> = StaticCell::new();
        let tx_buf = TX_BUF.init([0u8; 16]);
        let mut config = twim::Config::default();
        // The board should carry external pull-ups on the internal bus; enable the internal
        // ones too as cheap insurance (they only parallel, still fine at 100 kHz).
        config.sda_pullup = true;
        config.scl_pullup = true;
        let mut i2c =
            twim::Twim::new(self.twim, crate::Irqs, self.sda, self.scl, config, tx_buf);

        // Bring-up with retry: a transient NACK at boot must not kill motion detection for
        // the whole uptime — the IMU is the planned wake source for everything else.
        let mut attempt = 0u32;
        while !bring_up(&mut i2c).await {
            attempt += 1;
            log::error!("[IMU] bring-up failed (attempt {}), retrying", attempt);
            Timer::after(Duration::from_secs(5)).await;
        }
        log::info!("[IMU] LSM6DS3TR-C armed for wake-on-motion");

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
                let _ = i2c.write_read(ADDR, &[REG_WAKE_UP_SRC], &mut src).await;
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
async fn bring_up(i2c: &mut twim::Twim<'static>) -> bool {
    // Confirm we're talking to the chip we expect before trusting anything.
    let mut who = [0u8; 1];
    match i2c.write_read(ADDR, &[REG_WHO_AM_I], &mut who).await {
        Ok(()) if who[0] == WHO_AM_I_LSM6DS3TR_C => {}
        Ok(()) => {
            log::error!("[IMU] unexpected WHO_AM_I: {:#04x}", who[0]);
            return false;
        }
        Err(e) => {
            log::error!("[IMU] WHO_AM_I read failed: {:?}", e);
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
        if let Err(e) = i2c.write(ADDR, reg).await {
            log::error!("[IMU] register {:#04x} write failed: {:?}", reg[0], e);
            return false;
        }
    }

    // ~5 samples at 52 Hz for the slope filter to settle, then discard the phantom event.
    Timer::after(Duration::from_millis(100)).await;
    let mut src = [0u8; 1];
    let _ = i2c.write_read(ADDR, &[REG_WAKE_UP_SRC], &mut src).await;
    true
}
