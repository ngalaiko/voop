#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

pub const SERVICE_UUID: &str = "bece0001-ede4-4b59-8c60-1ee44d963a05";
/// Notify: streams DataPoints during a ride.
pub const STREAM_CHAR_UUID: &str = "bece0002-ede4-4b59-8c60-1ee44d963a05";
/// Read: current MCU and sensor status snapshot.
pub const STATUS_CHAR_UUID: &str = "bece0003-ede4-4b59-8c60-1ee44d963a05";
/// Write: iOS sends current unix time to the MCU.
pub const TIME_SYNC_CHAR_UUID: &str = "bece0004-ede4-4b59-8c60-1ee44d963a05";

/// Wire-format version, byte 0 of every packed `DataPoint`. Bump on any incompatible change to
/// the packed layout; `DataPoint::unpack` rejects a buffer whose version byte doesn't match, so
/// a firmware and an app built from different protocol revisions fail closed (no data) instead
/// of silently misinterpreting bytes.
pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BatteryState {
    Charging = 0,
    Discharging = 1,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BatteryStatus {
    pub percent: u8,
    pub state: BatteryState,
}

/// Snapshot read from the STATUS_CHAR_UUID characteristic.
///
/// Wire format (5 bytes, fixed):
///   [version u8][mcu_percent u8][mcu_state u8][flags u8][sensor_battery u8]
///   flags: bit 0 = sensor_connected, bit 1 = sensor_battery_present
/// Byte 0 is PROTOCOL_VERSION — the same fail-closed rule as DataPoint, so a status layout
/// change can't silently misparse (or blank out) on an app built from another revision.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeviceStatus {
    pub mcu_battery: BatteryStatus,
    pub sensor_connected: bool,
    /// Sensor battery percent. None if sensor is not connected or level unknown.
    pub sensor_battery: Option<u8>,
}

impl DeviceStatus {
    pub fn pack(&self) -> [u8; 5] {
        let mut flags: u8 = 0;
        if self.sensor_connected { flags |= 0x01; }
        if self.sensor_battery.is_some() { flags |= 0x02; }
        [
            PROTOCOL_VERSION,
            self.mcu_battery.percent,
            self.mcu_battery.state as u8,
            flags,
            self.sensor_battery.unwrap_or(0xFF),
        ]
    }

    pub fn unpack(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 5 || bytes[0] != PROTOCOL_VERSION { return None; }
        let state = match bytes[2] {
            0 => BatteryState::Charging,
            1 => BatteryState::Discharging,
            _ => return None,
        };
        let flags = bytes[3];
        Some(DeviceStatus {
            mcu_battery: BatteryStatus { percent: bytes[1], state },
            sensor_connected: flags & 0x01 != 0,
            sensor_battery: if flags & 0x02 != 0 { Some(bytes[4]) } else { None },
        })
    }
}

/// A single telemetry sample streamed over STREAM_CHAR_UUID — a raw event, no on-device
/// interpretation. The consumer (iOS) reconstructs the absolute timeline from these.
///
/// Wire format (little-endian, fixed 25 bytes):
///   [version u8][uptime_ms u32][unix_millis u64][lat i32][lon i32][crank_revs u16][crank_event_time u16]
/// Byte 0 is PROTOCOL_VERSION (unpack rejects a mismatch). Sentinels for the optionals:
/// unix_millis == 0 ⇒ None; lat/lon == i32::MIN ⇒ None.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DataPoint {
    /// Raw MCU monotonic clock at capture (ms since boot). Always present — the spine the
    /// consumer uses for ordering, relative timing, reboot detection, and backfill. Resets
    /// on reboot; wraps as a u32 (~49.7 days).
    pub uptime_ms: u32,
    /// The device's wall-clock estimate when it has an anchor (GPS or iOS time sync),
    /// extrapolated from the same monotonic clock; None before the first-ever sync. Carried
    /// per point so a buffered/replayed batch is self-describing across reconnects and reboots.
    pub unix_millis: Option<u64>,
    pub lat_microdeg: Option<i32>,
    pub lon_microdeg: Option<i32>,
    /// CSC cumulative crank revolutions (raw). Always present — a point *is* a crank event.
    pub crank_revs: u16,
    /// CSC "Last Crank Event Time": the sensor's own timestamp of the last crank revolution,
    /// 1/1024 s, wraps every 64 s. Lets the consumer derive cadence from Δrevs ÷ Δevent_time.
    pub crank_event_time: u16,
}

const COORD_NONE: i32 = i32::MIN;

impl DataPoint {
    pub fn pack(&self) -> [u8; 25] {
        let mut b = [0u8; 25];
        b[0] = PROTOCOL_VERSION;
        b[1..5].copy_from_slice(&self.uptime_ms.to_le_bytes());
        b[5..13].copy_from_slice(&self.unix_millis.unwrap_or(0).to_le_bytes());
        b[13..17].copy_from_slice(&self.lat_microdeg.unwrap_or(COORD_NONE).to_le_bytes());
        b[17..21].copy_from_slice(&self.lon_microdeg.unwrap_or(COORD_NONE).to_le_bytes());
        b[21..23].copy_from_slice(&self.crank_revs.to_le_bytes());
        b[23..25].copy_from_slice(&self.crank_event_time.to_le_bytes());
        b
    }

    pub fn unpack(bytes: &[u8]) -> Option<Self> {
        let b: [u8; 25] = bytes.get(..25)?.try_into().ok()?;
        if b[0] != PROTOCOL_VERSION {
            return None;
        }
        let unix = u64::from_le_bytes(b[5..13].try_into().unwrap());
        let lat = i32::from_le_bytes(b[13..17].try_into().unwrap());
        let lon = i32::from_le_bytes(b[17..21].try_into().unwrap());
        Some(DataPoint {
            uptime_ms: u32::from_le_bytes(b[1..5].try_into().unwrap()),
            unix_millis: (unix != 0).then_some(unix),
            lat_microdeg: (lat != COORD_NONE).then_some(lat),
            lon_microdeg: (lon != COORD_NONE).then_some(lon),
            crank_revs: u16::from_le_bytes(b[21..23].try_into().unwrap()),
            crank_event_time: u16::from_le_bytes(b[23..25].try_into().unwrap()),
        })
    }
}

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn protocol_version() -> u8 { PROTOCOL_VERSION }

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn service_uuid() -> String { SERVICE_UUID.to_string() }

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn stream_char_uuid() -> String { STREAM_CHAR_UUID.to_string() }

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn status_char_uuid() -> String { STATUS_CHAR_UUID.to_string() }

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn time_sync_char_uuid() -> String { TIME_SYNC_CHAR_UUID.to_string() }

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn unpack_data_point(bytes: Vec<u8>) -> Option<DataPoint> {
    DataPoint::unpack(&bytes)
}

#[cfg(feature = "uniffi")]
#[uniffi::export]
fn unpack_device_status(bytes: Vec<u8>) -> Option<DeviceStatus> {
    DeviceStatus::unpack(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_eq_point(a: DataPoint, b: DataPoint) {
        assert_eq!(a.uptime_ms, b.uptime_ms);
        assert_eq!(a.unix_millis, b.unix_millis);
        assert_eq!(a.lat_microdeg, b.lat_microdeg);
        assert_eq!(a.lon_microdeg, b.lon_microdeg);
        assert_eq!(a.crank_revs, b.crank_revs);
        assert_eq!(a.crank_event_time, b.crank_event_time);
    }

    #[test]
    fn roundtrips_anchored_point_with_coords() {
        let dp = DataPoint {
            uptime_ms: 1_805_000,
            unix_millis: Some(1_782_714_859_321),
            lat_microdeg: Some(57_705_670),
            lon_microdeg: Some(11_940_034),
            crank_revs: 42,
            crank_event_time: 50_123,
        };
        assert_eq!(dp.pack().len(), 25);
        assert_eq!(dp.pack()[0], PROTOCOL_VERSION);
        assert_eq_point(dp, DataPoint::unpack(&dp.pack()).expect("unpack"));
    }

    #[test]
    fn roundtrips_pre_sync_point_without_unix_or_coords() {
        // Before any time sync and before a GPS fix: only the raw uptime + crank fields.
        let dp = DataPoint {
            uptime_ms: 32_832,
            unix_millis: None,
            lat_microdeg: None,
            lon_microdeg: None,
            crank_revs: 7,
            crank_event_time: 9_001,
        };
        let packed = dp.pack();
        assert_eq!(u64::from_le_bytes(packed[5..13].try_into().unwrap()), 0); // unix sentinel
        assert_eq!(i32::from_le_bytes(packed[13..17].try_into().unwrap()), i32::MIN); // lat sentinel
        assert_eq_point(dp, DataPoint::unpack(&packed).expect("unpack"));
    }

    #[test]
    fn unpack_rejects_short_buffers() {
        assert!(DataPoint::unpack(&[0u8; 24]).is_none());
    }

    #[test]
    fn unpack_rejects_mismatched_version() {
        let dp = DataPoint {
            uptime_ms: 1,
            unix_millis: None,
            lat_microdeg: None,
            lon_microdeg: None,
            crank_revs: 0,
            crank_event_time: 0,
        };
        let mut packed = dp.pack();
        packed[0] = PROTOCOL_VERSION.wrapping_add(1); // a different protocol revision
        assert!(DataPoint::unpack(&packed).is_none());
    }

    // The golden tests pin the exact wire bytes. If one fails, the layout changed: bump
    // PROTOCOL_VERSION and update the bytes here — never ship a layout change without a bump.
    #[test]
    fn data_point_golden_bytes() {
        let dp = DataPoint {
            uptime_ms: 0x0102_0304,
            unix_millis: Some(0x1112_1314_1516_1718),
            lat_microdeg: Some(0x2122_2324),
            lon_microdeg: Some(-2),
            crank_revs: 0x3132,
            crank_event_time: 0x4142,
        };
        #[rustfmt::skip]
        let expected: [u8; 25] = [
            1,                                              // version
            0x04, 0x03, 0x02, 0x01,                         // uptime_ms LE
            0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, // unix_millis LE
            0x24, 0x23, 0x22, 0x21,                         // lat LE
            0xFE, 0xFF, 0xFF, 0xFF,                         // lon = -2 LE
            0x32, 0x31,                                     // crank_revs LE
            0x42, 0x41,                                     // crank_event_time LE
        ];
        assert_eq!(dp.pack(), expected);
    }

    #[test]
    fn device_status_golden_bytes() {
        let ds = DeviceStatus {
            mcu_battery: BatteryStatus { percent: 85, state: BatteryState::Discharging },
            sensor_connected: true,
            sensor_battery: Some(72),
        };
        assert_eq!(ds.pack(), [1, 85, 1, 0x03, 72]);
    }

    #[test]
    fn device_status_roundtrips() {
        let ds = DeviceStatus {
            mcu_battery: BatteryStatus { percent: 42, state: BatteryState::Charging },
            sensor_connected: false,
            sensor_battery: None,
        };
        let back = DeviceStatus::unpack(&ds.pack()).expect("unpack");
        assert_eq!(back.mcu_battery, ds.mcu_battery);
        assert_eq!(back.sensor_connected, ds.sensor_connected);
        assert_eq!(back.sensor_battery, ds.sensor_battery);
    }

    #[test]
    fn device_status_rejects_mismatched_version_and_short_buffers() {
        let ds = DeviceStatus {
            mcu_battery: BatteryStatus { percent: 1, state: BatteryState::Discharging },
            sensor_connected: true,
            sensor_battery: Some(9),
        };
        let mut packed = ds.pack();
        packed[0] = PROTOCOL_VERSION.wrapping_add(1);
        assert!(DeviceStatus::unpack(&packed).is_none());
        assert!(DeviceStatus::unpack(&ds.pack()[..4]).is_none());
    }
}
