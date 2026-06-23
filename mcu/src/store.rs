use embassy_futures::select::{select4, Either4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::Watch;
use embassy_time::Instant;
use heapless::Deque;

use crate::gps::FixQuality;

#[derive(Clone, Copy, Debug)]
pub struct DataPoint {
    pub monotonic_ms: u32,
    pub crank_revs: Option<u16>,
    pub lat: Option<i32>,           // microdegrees (divide by 1_000_000 for degrees)
    pub lon: Option<i32>,           // microdegrees
    pub fix_quality: Option<FixQuality>,
    pub gps_unix_time: Option<u32>, // seconds since epoch; set only when GPS provides it
    pub sensor_battery: Option<u8>, // Garmin battery %
}

impl DataPoint {
    /// Pack into wire format for BLE notification (max 24 bytes).
    ///
    /// Layout: [monotonic_ms u64 LE][flags u8][crank_revs u16 LE?][lat i32 LE?][lon i32 LE?][gps_unix_time u32 LE?][sensor_battery u8?]
    /// flags: 0x01=crank, 0x02=lat+lon, 0x04=gps_time, 0x08=battery, 0x10=differential_fix
    pub fn pack(&self) -> heapless::Vec<u8, 24> {
        let mut buf: heapless::Vec<u8, 24> = heapless::Vec::new();
        let _ = buf.extend_from_slice(&self.monotonic_ms.to_le_bytes());

        let mut flags: u8 = 0;
        if self.crank_revs.is_some() {
            flags |= 0x01;
        }
        if self.lat.is_some() {
            flags |= 0x02;
        }
        if self.gps_unix_time.is_some() {
            flags |= 0x04;
        }
        if self.sensor_battery.is_some() {
            flags |= 0x08;
        }
        if matches!(self.fix_quality, Some(FixQuality::Differential)) {
            flags |= 0x10;
        }
        let _ = buf.push(flags);

        if let Some(revs) = self.crank_revs {
            let _ = buf.extend_from_slice(&revs.to_le_bytes());
        }
        if let Some(lat) = self.lat {
            let _ = buf.extend_from_slice(&lat.to_le_bytes());
            let _ = buf.extend_from_slice(&self.lon.unwrap_or(0).to_le_bytes());
        }
        if let Some(t) = self.gps_unix_time {
            let _ = buf.extend_from_slice(&t.to_le_bytes());
        }
        if let Some(bat) = self.sensor_battery {
            let _ = buf.push(bat);
        }
        buf
    }
}

const CAPACITY: usize = 4096;

struct Store {
    buf: Deque<DataPoint, CAPACITY>,
}

impl Store {
    const fn new() -> Self {
        Self { buf: Deque::new() }
    }

    fn push(&mut self, point: DataPoint) {
        if self.buf.is_full() {
            self.buf.pop_front();
        }
        let _ = self.buf.push_back(point);
    }

    fn peek_latest(&self) -> Option<DataPoint> {
        self.buf.back().copied()
    }

    fn pop_newest(&mut self) -> Option<DataPoint> {
        self.buf.pop_back()
    }
}

static STORE: Mutex<CriticalSectionRawMutex, Store> = Mutex::new(Store::new());

/// Fires () whenever a new DataPoint is pushed. 2 receivers: screen + peripheral.
pub static UPDATED: Watch<CriticalSectionRawMutex, (), 2> = Watch::new();

pub async fn peek_latest() -> Option<DataPoint> {
    STORE.lock().await.peek_latest()
}

pub async fn pop_newest() -> Option<DataPoint> {
    STORE.lock().await.pop_newest()
}

pub async fn run() {
    let Some(mut crank_rx) = crate::ble::CRANK_REVS.receiver() else {
        log::error!("[Store] CRANK_REVS: no free receiver slot");
        return;
    };
    let Some(mut battery_rx) = crate::ble::BATTERY.receiver() else {
        log::error!("[Store] BATTERY: no free receiver slot");
        return;
    };
    let Some(mut location_rx) = crate::gps::LOCATION.receiver() else {
        log::error!("[Store] LOCATION: no free receiver slot");
        return;
    };
    let Some(mut time_rx) = crate::gps::TIME.receiver() else {
        log::error!("[Store] TIME: no free receiver slot");
        return;
    };

    let mut current_lat: Option<i32> = None;
    let mut current_lon: Option<i32> = None;
    let mut current_fix_quality: Option<FixQuality> = None;
    let mut current_gps_time: Option<u32> = None;
    let mut current_battery: Option<u8> = None;

    loop {
        match select4(
            crank_rx.changed(),
            battery_rx.changed(),
            location_rx.changed(),
            time_rx.changed(),
        )
        .await
        {
            Either4::First(Ok(revs)) => {
                let point = DataPoint {
                    monotonic_ms: Instant::now().as_millis() as u32,
                    crank_revs: Some(revs),
                    lat: current_lat,
                    lon: current_lon,
                    fix_quality: current_fix_quality,
                    gps_unix_time: current_gps_time,
                    sensor_battery: current_battery,
                };
                STORE.lock().await.push(point);
                UPDATED.sender().send(());
            }
            Either4::First(Err(_)) => {}
            Either4::Second(Ok(bat)) => {
                current_battery = Some(bat);
            }
            Either4::Second(Err(_)) => {}
            Either4::Third(Ok(loc)) => {
                current_lat = Some((loc.lat * 1_000_000.0) as i32);
                current_lon = Some((loc.lon * 1_000_000.0) as i32);
                current_fix_quality = Some(loc.fix_quality);
            }
            Either4::Third(Err(_)) => {}
            Either4::Fourth(Ok(t)) => {
                current_gps_time = Some(t);
            }
            Either4::Fourth(Err(_)) => {}
        }
    }
}
