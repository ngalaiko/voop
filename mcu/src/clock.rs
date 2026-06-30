use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Instant;

/// The raw clock state at one instant: the always-available monotonic uptime, plus the
/// wall-clock estimate when an anchor has been set. Stamped onto every DataPoint so iOS
/// reconstructs the timeline itself.
#[derive(Clone, Copy)]
pub struct ClockReading {
    pub uptime_ms: u32,
    pub unix_millis: Option<u64>,
}

/// Maps a monotonic instant to wall-clock time. The monotonic reference is a full `Instant`
/// (64-bit), so extrapolation stays correct across the u32-millisecond wrap of the wire
/// `uptime_ms` field (~49.7 days).
struct Anchor {
    mono: Instant,
    unix_ms: u64,
}

static ANCHOR: Mutex<CriticalSectionRawMutex, Option<Anchor>> = Mutex::new(None);

/// Re-anchor only when a fresh sync disagrees with the current estimate by more than this.
/// Below the threshold the existing anchor is kept, so consecutive `unix_millis` stay smooth
/// and ms-precise — re-anchoring on every ~1 Hz GPS sentence would reset the sub-second phase
/// and could nudge the wall-clock backward.
const RESYNC_THRESHOLD_MS: u64 = 2000;

/// Record a wall-clock anchor. Call when iOS writes a time sync or when GPS fixes. Idempotent
/// within `RESYNC_THRESHOLD_MS`, so frequent GPS sentences don't disturb a good anchor.
pub async fn set(unix_seconds: u32) {
    let now = Instant::now();
    let new_unix_ms = unix_seconds as u64 * 1000;
    let mut anchor = ANCHOR.lock().await;
    let resync = match anchor.as_ref() {
        None => true,
        Some(a) => {
            let estimate = a.unix_ms + now.saturating_duration_since(a.mono).as_millis();
            estimate.abs_diff(new_unix_ms) > RESYNC_THRESHOLD_MS
        }
    };
    if resync {
        *anchor = Some(Anchor { mono: now, unix_ms: new_unix_ms });
    }
}

/// Current clock reading: always the monotonic uptime, plus a wall-clock estimate once an
/// anchor exists. The anchor is only second-accurate (iOS/GPS sync it in whole seconds); the
/// sub-second part comes from the monotonic delta, so consecutive `unix_millis` stay ms-precise.
pub async fn now() -> ClockReading {
    let now = Instant::now();
    let uptime_ms = now.as_millis() as u32;
    let unix_millis = ANCHOR
        .lock()
        .await
        .as_ref()
        .map(|a| a.unix_ms + now.saturating_duration_since(a.mono).as_millis());
    ClockReading { uptime_ms, unix_millis }
}
