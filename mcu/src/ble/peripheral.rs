use static_cell::StaticCell;
use trouble_host::prelude::*;

// Custom 128-bit service UUID: bece0001-ede4-4b59-8c60-1ee44d963a05
// Custom data characteristic:  bece0002-ede4-4b59-8c60-1ee44d963a05
#[gatt_server]
pub struct BikeServer {
    bike: BikeService,
}

#[gatt_service(uuid = "bece0001-ede4-4b59-8c60-1ee44d963a05")]
struct BikeService {
    // Packed DataPoint wire format, max 24 bytes. See store::DataPoint::pack().
    #[characteristic(uuid = "bece0002-ede4-4b59-8c60-1ee44d963a05", notify)]
    data: [u8; 24],
}

// Extended ADV data: flags + 128-bit service UUID + complete local name.
// Type 0x07 = Complete list of 128-bit UUIDs (LE byte order).
// UUID bece0001-ede4-4b59-8c60-1ee44d963a05 in LE: 05 3a 96 4d e4 1e 60 8c 4b 59 e4 ed 01 00 ce be
const ADV_DATA: &[u8] = &[
    0x02, 0x01, 0x06, // Flags: LE General Discoverable, BR/EDR Not Supported
    0x11, 0x07, // length=17, type=Complete 128-bit UUIDs
    0x05, 0x3A, 0x96, 0x4D, 0xE4, 0x1E, 0x60, 0x8C, 0x4B, 0x59, 0xE4, 0xED, 0x01, 0x00, 0xCE,
    0xBE, 0x0D, 0x09, b'B', b'i', b'k', b'e', b'C', b'o', b'm', b'p', b'u', b't', b'e', b'r',
];

pub async fn run(stack: &Stack<'_, super::MyController, DefaultPacketPool>) {
    static SERVER: StaticCell<BikeServer<'static>> = StaticCell::new();
    let server = SERVER.init(
        BikeServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
            name: "BikeComputer",
            appearance: &appearance::cycling::SPEED_AND_CADENCE_SENSOR,
        }))
        .expect("BikeServer init failed"),
    );

    loop {
        log::info!("[BLE peripheral] Advertising...");

        let sets = [AdvertisementSet {
            params: AdvertisementParameters::default(),
            data: Advertisement::ExtConnectableNonscannableUndirected { adv_data: ADV_DATA },
            address: None,
        }];
        let mut handles = AdvertisementSet::handles(&sets);

        let mut peripheral = stack.peripheral();
        let advertiser = match peripheral.advertise_ext(&sets, &mut handles).await {
            Ok(a) => a,
            Err(e) => {
                log::warn!("[BLE peripheral] advertise error: {:?}", e);
                continue;
            }
        };

        let conn = match advertiser.accept().await {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[BLE peripheral] accept error: {:?}", e);
                continue;
            }
        };

        log::info!("[BLE peripheral] iOS connected");
        let gatt_conn = match conn.with_attribute_server(&server.server) {
            Ok(gc) => gc,
            Err(e) => {
                log::warn!("[BLE peripheral] GATT setup error: {:?}", e);
                continue;
            }
        };

        let handle = server.bike.data.handle;

        embassy_futures::join::join(
            // Drain GATT events so iOS can do service discovery and subscribe.
            async {
                loop {
                    match gatt_conn.next().await {
                        GattConnectionEvent::Disconnected { .. } => break,
                        GattConnectionEvent::Gatt { event } => {
                            if let Err(e) = event.accept() {
                                log::warn!("[BLE peripheral] GATT reply error: {:?}", e);
                            }
                        }
                        _ => {}
                    }
                }
            },
            // Send buffered points newest-first, then stream live data.
            async {
                // Phase 1: replay the ring buffer, newest first.
                log::info!("[BLE peripheral] Replaying buffered points...");
                loop {
                    let point = crate::store::pop_newest().await;
                    match point {
                        None => break,
                        Some(p) => {
                            let packed = p.pack();
                            if server.notify(stack, handle, &packed[..]).await.is_err() {
                                log::warn!("[BLE peripheral] notify error during replay");
                                return;
                            }
                        }
                    }
                }

                // Phase 2: live stream — one notification per new DataPoint.
                let Some(mut updated_rx) = crate::store::UPDATED.receiver() else {
                    log::error!("[BLE peripheral] UPDATED: no free receiver slot");
                    return;
                };
                loop {
                    updated_rx.changed().await;
                    if let Some(p) = crate::store::peek_latest().await {
                        let packed = p.pack();
                        if server.notify(stack, handle, &packed[..]).await.is_err() {
                            log::warn!("[BLE peripheral] notify error during live stream");
                            return;
                        }
                    }
                }
            },
        )
        .await;

        log::info!("[BLE peripheral] iOS disconnected");
    }
}
