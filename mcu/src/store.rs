use embassy_futures::select::{select3, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Instant;
use heapless::Deque;

use crate::gps::Location;

pub struct GpsAnchor {
    pub instant: Instant,
    pub epoch: u32,
}

pub struct DataPoint {
    pub instant: Instant,
    pub location: Option<Location>,
    pub cumulative_crank_revs: u16,
}

const CAPACITY: usize = 2048;

struct Inner {
    anchor: Option<GpsAnchor>,
    points: Deque<DataPoint, CAPACITY>,
}

static INNER: Mutex<CriticalSectionRawMutex, Inner> = Mutex::new(Inner {
    anchor: None,
    points: Deque::new(),
});

pub async fn set_anchor(instant: Instant, epoch: u32) {
    let mut inner = INNER.lock().await;
    if inner.anchor.is_none() {
        inner.anchor = Some(GpsAnchor { instant, epoch });
    }
}

pub async fn anchor() -> Option<GpsAnchor> {
    let inner = INNER.lock().await;
    inner.anchor.as_ref().map(|a| GpsAnchor {
        instant: a.instant,
        epoch: a.epoch,
    })
}

pub async fn push(point: DataPoint) {
    let mut inner = INNER.lock().await;
    if inner.points.is_full() {
        inner.points.pop_front();
    }
    inner.points.push_back(point).ok();
}

pub async fn pop() -> Option<DataPoint> {
    INNER.lock().await.points.pop_front()
}

pub async fn len() -> usize {
    INNER.lock().await.points.len()
}

#[derive(Debug)]
pub enum Error {
    SpawnFailed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnFailed => write!(f, "failed to spawn store task"),
        }
    }
}

impl core::error::Error for Error {}

pub fn init(spawner: embassy_executor::Spawner) -> Result<(), Error> {
    spawner.spawn(task().map_err(|_| Error::SpawnFailed)?);
    Ok(())
}

#[embassy_executor::task]
async fn task() {
    let Some(mut location_rx) = crate::gps::LOCATION.receiver() else {
        log::error!("[Store] LOCATION watch: no free receiver slot");
        return;
    };
    let Some(mut time_rx) = crate::gps::TIME.receiver() else {
        log::error!("[Store] TIME watch: no free receiver slot");
        return;
    };
    let Some(mut crank_rx) = crate::ble::CRANK_REVS.receiver() else {
        log::error!("[Store] CRANK_REVS watch: no free receiver slot");
        return;
    };

    let mut current_location: Option<Location> = None;

    loop {
        match select3(location_rx.changed(), time_rx.changed(), crank_rx.changed()).await {
            Either3::First(Ok(loc)) => {
                current_location = Some(loc);
            }
            Either3::First(Err(e)) => {
                log::warn!("[Store] GPS location error: {}", e);
                current_location = None;
            }
            Either3::Second(Ok(epoch)) => {
                set_anchor(Instant::now(), epoch).await;
            }
            Either3::Second(Err(e)) => {
                log::warn!("[Store] GPS time error: {}", e);
            }
            Either3::Third(Ok(revs)) => {
                push(DataPoint {
                    instant: Instant::now(),
                    location: current_location.clone(),
                    cumulative_crank_revs: revs,
                })
                .await;
            }
            Either3::Third(Err(e)) => {
                log::warn!("[Store] BLE crank error: {}", e);
            }
        }
    }
}
