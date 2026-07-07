use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_sync::watch::Watch;

use embassy_time::{with_timeout, Duration, Timer};
use trouble_host::prelude::*;

use super::MyController;

const CSC_SERVICE: Uuid = Uuid::new_short(0x1816);
const CSC_MEASUREMENT: Uuid = Uuid::new_short(0x2A5B);
const BATTERY_SERVICE: Uuid = Uuid::new_short(0x180F);
const BATTERY_LEVEL: Uuid = Uuid::new_short(0x2A19);

#[derive(Clone, Debug)]
pub enum Error {
    SpawnFailed,
    MpslInitFailed,
    SdcInitFailed,
    RunnerCrashed,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnFailed => write!(f, "spawn failed"),
            Error::MpslInitFailed => write!(f, "MPSL init failed"),
            Error::SdcInitFailed => write!(f, "SDC init failed"),
            Error::RunnerCrashed => write!(f, "BLE runner crashed"),
        }
    }
}

impl core::error::Error for Error {}

/// One CSC crank reading: the cumulative revolution count plus the sensor's "Last Crank
/// Event Time" (1/1024 s, u16, wraps every 64 s). Bundled so a single `changed()` delivers
/// both atomically — the event time is what lets iOS derive jitter-free cadence.
#[derive(Clone, Copy, PartialEq)]
pub struct CrankSample {
    pub revs: u16,
    pub event_time: u16,
}

// Crank readings — sent only when the rev count changes (bike moving).
// 2 receivers: peripheral DataPoint loop + screen.
pub static CRANK_REVS: Watch<CriticalSectionRawMutex, CrankSample, 2> = Watch::new();

// Whether the cadence sensor is connected.
// 2 receivers: peripheral DataPoint loop + screen.
pub static SENSOR_CONNECTED: Watch<CriticalSectionRawMutex, bool, 2> = Watch::new();

// Last known sensor battery %.
// 2 receivers: peripheral DataPoint loop + screen.
pub static SENSOR_BATTERY: Watch<CriticalSectionRawMutex, u8, 2> = Watch::new();

// Signals the address of the first CSC sensor spotted during scanning.
pub(super) static SENSOR_ADDR: Signal<CriticalSectionRawMutex, Address> = Signal::new();

fn ad_has_uuid16(data: &[u8], target: u16) -> bool {
    let mut i = 0;
    while i < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        let ad_type = data[i + 1];
        if ad_type == 0x02 || ad_type == 0x03 {
            let uuid_data = &data[i + 2..i + 1 + len];
            let mut j = 0;
            while j + 1 < uuid_data.len() {
                if u16::from_le_bytes([uuid_data[j], uuid_data[j + 1]]) == target {
                    return true;
                }
                j += 2;
            }
        }
        i += 1 + len;
    }
    false
}

pub(super) struct CscEventHandler;

impl EventHandler for CscEventHandler {
    fn on_ext_adv_reports(&self, reports: LeExtAdvReportsIter<'_>) {
        for report in reports.filter_map(|r| r.ok()) {
            if ad_has_uuid16(report.data, 0x1816) {
                SENSOR_ADDR.signal(Address::new(report.addr_kind, report.addr));
            }
        }
    }
}

pub async fn run(stack: &Stack<'_, MyController, DefaultPacketPool>) {
    // Back off before every reconnect attempt (after the first) so a sensor that connects but
    // fails service discovery — or any error path that `continue`s — doesn't spin in a tight
    // scan→connect→fail loop. The first iteration scans immediately.
    let mut backoff = false;
    loop {
        if backoff {
            Timer::after(Duration::from_secs(1)).await;
        }
        backoff = true;

        SENSOR_CONNECTED.sender().send(false);
        SENSOR_ADDR.reset();

        log::info!("[BLE central] Scanning for CSC sensor...");
        let central = stack.central();
        let mut scanner = Scanner::new(central);
        let scan_config = ScanConfig {
            filter_accept_list: &[],
            interval: Duration::from_millis(200),
            window: Duration::from_millis(50),
            ..Default::default()
        };

        let session = match scanner.scan_ext(&scan_config).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[BLE central] scan error: {:?}", e);
                continue;
            }
        };

        let addr = SENSOR_ADDR.wait().await;
        drop(session);
        embassy_futures::yield_now().await;

        log::info!("[BLE central] Found CSC sensor, connecting...");
        let mut central = scanner.into_inner();
        let connect_config = ConnectConfig {
            connect_params: Default::default(),
            scan_config: ScanConfig {
                filter_accept_list: &[addr],
                ..Default::default()
            },
        };

        // Bounded: connect_ext awaits a connection-complete event indefinitely, so a sensor
        // that vanished between the scan report and here (out of range, battery pulled)
        // would park this task forever and cadence would never recover.
        let conn = match with_timeout(Duration::from_secs(10), central.connect_ext(&connect_config))
            .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                log::warn!("[BLE central] connect error: {:?}", e);
                continue;
            }
            Err(_) => {
                log::warn!("[BLE central] connect timeout, rescanning");
                continue;
            }
        };
        log::info!("[BLE central] Connected to Garmin");
        SENSOR_CONNECTED.sender().send(true);

        let client: GattClient<'_, MyController, DefaultPacketPool, 4> =
            match GattClient::new(stack, &conn).await {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[BLE central] GattClient error: {:?}", e);
                    continue;
                }
            };

        match select(client.task(), async {
            let services = match client.services_by_uuid(&CSC_SERVICE).await {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("[BLE central] service discovery error: {:?}", e);
                    return;
                }
            };
            let service = match services.first() {
                Some(s) => s.clone(),
                None => {
                    log::info!("[BLE central] No CSC service found");
                    return;
                }
            };
            let characteristic: Characteristic<[u8]> = match client
                .characteristic_by_uuid(&service, &CSC_MEASUREMENT)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[BLE central] characteristic error: {:?}", e);
                    return;
                }
            };
            let mut csc_listener = match client.subscribe(&characteristic, false).await {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("[BLE central] subscribe error: {:?}", e);
                    return;
                }
            };

            let mut battery_listener = 'bat: {
                let Ok(svcs) = client.services_by_uuid(&BATTERY_SERVICE).await else {
                    break 'bat None;
                };
                let Some(svc) = svcs.first().cloned() else {
                    break 'bat None;
                };
                let Ok(bat_char): Result<Characteristic<[u8]>, _> =
                    client.characteristic_by_uuid(&svc, &BATTERY_LEVEL).await
                else {
                    break 'bat None;
                };
                let listener = match client.subscribe(&bat_char, false).await {
                    Ok(l) => l,
                    Err(_) => break 'bat None,
                };
                // Read current battery level immediately — Garmin doesn't push proactively.
                let mut buf = [0u8; 1];
                if let Ok(n) = client.read_characteristic(&bat_char, &mut buf).await {
                    if n > 0 {
                        SENSOR_BATTERY.sender().send(buf[0]);
                    }
                }
                Some(listener)
            };

            log::info!("[BLE central] Subscribed to CSC measurement");
            embassy_futures::join::join(
                async {
                    let crank_tx = CRANK_REVS.sender();
                    let mut last_revs: Option<u16> = None;
                    loop {
                        let notif = csc_listener.next().await;
                        let data = notif.as_ref();
                        // CSC Measurement: [flags u8], then wheel data when flags bit 0 is set
                        // (u32 revs + u16 event time — combined speed+cadence sensors send it),
                        // then crank data when flags bit 1 is set ([revs u16][event_time u16]).
                        // The wheel block must be skipped, or its bytes get read as crank revs.
                        let flags = data.first().copied().unwrap_or(0);
                        let off = if flags & 0x01 != 0 { 7 } else { 1 };
                        if (flags & 0x02) != 0 && data.len() >= off + 4 {
                            let revs = u16::from_le_bytes([data[off], data[off + 1]]);
                            let event_time = u16::from_le_bytes([data[off + 2], data[off + 3]]);
                            if Some(revs) != last_revs {
                                crank_tx.send(CrankSample { revs, event_time });
                                last_revs = Some(revs);
                            }
                            log::debug!("[BLE central] CSC revs={} evt={}", revs, event_time);
                        }
                    }
                },
                async {
                    match battery_listener {
                        Some(ref mut listener) => loop {
                            let notif = listener.next().await;
                            let data = notif.as_ref();
                            if let Some(&level) = data.first() {
                                log::info!("[BLE central] Battery: {}%", level);
                                SENSOR_BATTERY.sender().send(level);
                            }
                        },
                        None => core::future::pending().await,
                    }
                },
            )
            .await;
        })
        .await
        {
            Either::First(Err(e)) => log::warn!("[BLE central] GATT task error: {:?}", e),
            _ => {}
        }
        log::info!("[BLE central] Disconnected, restarting scan");
    }
}
