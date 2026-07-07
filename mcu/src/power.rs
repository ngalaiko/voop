use embassy_futures::select::{select3, Either, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Timer};

// Ride-state power policy: one place decides what deserves power, driven by the activity
// signals the device has. Consumers just obey their gate; hold times and interactions all
// live here.
//
// Normal mode (IMU healthy) — motion is the master signal:
//   - Sensor scan: the Garmin sleeps when still and only advertises once the crank turns,
//     so scanning a parked bike duty-cycles the radio around the clock to find nobody.
//     Motion is the exact moment the sensor becomes findable.
//   - GPS: armed by the first motion so the Air530's fix is ready by the time the crank
//     turns (tens of seconds to acquire), sustained by cadence or motion, released after
//     GPS_HOLD of neither.
//
// Degraded mode (imu::HEALTHY == false) — the Garmin itself becomes the motion detector,
// since it too only wakes on crank movement:
//   - Sensor scan: duty-cycled (DEGRADED_SCAN_BURST out of every DEGRADED_SCAN_PERIOD)
//     while disconnected; a parked bike's sensor is silent, so bursts find nobody cheaply,
//     and a ride is picked up within one period of the first pedal stroke.
//   - GPS: on while the sensor is connected (connected ⇒ the crank moved recently),
//     held for GPS_HOLD past the last cadence event or disconnect.
//
// iOS advertising is deliberately NOT gated in either mode: parked-at-home next to the
// phone is when the ride buffer syncs, and advertising costs tens of µA.

/// Whether the BLE central should be scanning for the cadence sensor.
/// 1 receiver: ble::central.
pub static SCAN_ENABLED: Watch<CriticalSectionRawMutex, bool, 1> = Watch::new();

/// Whether the GPS should be tracking (false = UART standby).
/// 1 receiver: gps.
pub static GPS_ENABLED: Watch<CriticalSectionRawMutex, bool, 1> = Watch::new();

// Scan keeps going this long past the last motion so a stop at a light doesn't abort an
// in-progress discovery; re-arming is cheap (the next jolt), so it stays short.
const SCAN_HOLD: Duration = Duration::from_secs(30);
// GPS survives this long with no activity: long enough to bridge a café stop without
// re-acquisition, short enough that a parked bike stops paying ~40 mA within minutes.
const GPS_HOLD: Duration = Duration::from_secs(5 * 60);
// Degraded-mode discovery duty cycle. The scan itself runs a 50/200 ms radio window, so
// this averages out to ~4 % radio duty while parked — versus 25 % for always-on scanning.
const DEGRADED_SCAN_BURST: Duration = Duration::from_secs(10);
const DEGRADED_SCAN_PERIOD: Duration = Duration::from_secs(60);

pub async fn run() {
    let Some(mut moving_rx) = crate::imu::MOVING.receiver() else {
        log::error!("[Power] MOVING: no free receiver slot");
        return;
    };
    let Some(mut crank_rx) = crate::ble::central::CRANK_REVS.receiver() else {
        log::error!("[Power] CRANK_REVS: no free receiver slot");
        return;
    };
    let Some(mut healthy_rx) = crate::imu::HEALTHY.receiver() else {
        log::error!("[Power] HEALTHY: no free receiver slot");
        return;
    };
    let Some(mut sensor_rx) = crate::ble::central::SENSOR_CONNECTED.receiver() else {
        log::error!("[Power] SENSOR_CONNECTED: no free receiver slot");
        return;
    };

    let scan_tx = SCAN_ENABLED.sender();
    let gps_tx = GPS_ENABLED.sender();

    let mut moving = false;
    // Optimistic until the IMU says otherwise (~30 s of failed bring-ups); a later
    // recovery flips it back and normal gating resumes.
    let mut imu_healthy = true;
    let mut sensor_connected = false;
    // Hold anchors. While `moving` is true the gates are open regardless, so only the
    // falling edge needs remembering; same for the sensor-disconnect edge.
    let mut last_still: Option<Instant> = None;
    let mut last_crank: Option<Instant> = None;
    let mut last_disconnect: Option<Instant> = None;
    // Phase anchor for the degraded duty cycle.
    let cycle_start = Instant::now();
    let mut scan_on = false;
    let mut gps_on = false;
    scan_tx.send(false);
    gps_tx.send(false);

    loop {
        let now = Instant::now();
        let expiry = |t: Option<Instant>, hold: Duration| t.map(|t| t + hold);
        let active = |e: Option<Instant>| e.is_some_and(|e| now < e);

        // Everything the wake timer might need to re-evaluate for; collected as we go.
        let mut deadline: Option<Instant> = None;
        let mut wake_at = |e: Option<Instant>| {
            if let Some(e) = e {
                if e > now {
                    deadline = Some(deadline.map_or(e, |d| d.min(e)));
                }
            }
        };

        let (want_scan, want_gps) = if imu_healthy {
            let scan_expiry = expiry(last_still, SCAN_HOLD);
            // Option's Ord (None < Some) picks the later of the two anchors' expiries.
            let gps_expiry = expiry(last_still, GPS_HOLD).max(expiry(last_crank, GPS_HOLD));
            let want_scan = moving || active(scan_expiry);
            let want_gps = moving || active(gps_expiry);
            if !moving {
                if want_scan {
                    wake_at(scan_expiry);
                }
                if want_gps {
                    wake_at(gps_expiry);
                }
            }
            (want_scan, want_gps)
        } else {
            // Degraded: duty-cycle discovery while disconnected; while connected the
            // central isn't scanning anyway, so the gate rests false.
            let period = DEGRADED_SCAN_PERIOD.as_ticks();
            let in_cycle = now.duration_since(cycle_start).as_ticks() % period;
            let bursting = in_cycle < DEGRADED_SCAN_BURST.as_ticks();
            let want_scan = !sensor_connected && bursting;
            if !sensor_connected {
                let to_boundary = if bursting {
                    DEGRADED_SCAN_BURST.as_ticks() - in_cycle
                } else {
                    period - in_cycle
                };
                wake_at(Some(now + Duration::from_ticks(to_boundary)));
            }

            let gps_expiry =
                expiry(last_crank, GPS_HOLD).max(expiry(last_disconnect, GPS_HOLD));
            let want_gps = sensor_connected || active(gps_expiry);
            if !sensor_connected && want_gps {
                wake_at(gps_expiry);
            }
            (want_scan, want_gps)
        };

        if want_scan != scan_on {
            scan_on = want_scan;
            // The duty cycle would make this line spam every burst; only transitions of
            // the *other* signals are interesting enough to log at info.
            log::debug!("[Power] scan {}", if scan_on { "on" } else { "off" });
            scan_tx.send(scan_on);
        }
        if want_gps != gps_on {
            gps_on = want_gps;
            log::info!("[Power] gps {}", if gps_on { "on" } else { "off" });
            gps_tx.send(gps_on);
        }

        let timer = async {
            match deadline {
                Some(d) => Timer::at(d).await,
                None => core::future::pending().await,
            }
        };
        match select3(
            embassy_futures::select::select(moving_rx.changed(), crank_rx.changed()),
            embassy_futures::select::select(healthy_rx.changed(), sensor_rx.changed()),
            timer,
        )
        .await
        {
            Either3::First(Either::First(m)) => {
                if moving && !m {
                    last_still = Some(Instant::now());
                }
                moving = m;
            }
            Either3::First(Either::Second(_)) => last_crank = Some(Instant::now()),
            Either3::Second(Either::First(h)) => {
                log::info!("[Power] IMU {}", if h { "healthy" } else { "unavailable" });
                imu_healthy = h;
            }
            Either3::Second(Either::Second(c)) => {
                if sensor_connected && !c {
                    last_disconnect = Some(Instant::now());
                }
                sensor_connected = c;
            }
            Either3::Third(()) => {}
        }
    }
}
