@preconcurrency import CoreBluetooth
import Foundation
import os

private let log = Logger(subsystem: "com.galaiko.voop", category: "ble")

private nonisolated(unsafe) let mcuServiceUUID = CBUUID(string: serviceUuid())
private nonisolated(unsafe) let streamCharUUID = CBUUID(string: streamCharUuid())
private nonisolated(unsafe) let statusCharUUID = CBUUID(string: statusCharUuid())
private nonisolated(unsafe) let timeSyncCharUUID = CBUUID(string: timeSyncCharUuid())

final class MCUPeripheral: NSObject, CBPeripheralDelegate, @unchecked Sendable {
    var onDataPoint: (@MainActor (DataPoint) -> Void)?
    var onStatusUpdate: (@MainActor (DeviceStatus) -> Void)?
    /// Fired when a notification fails the version-checked decode: the firmware speaks a
    /// different protocol revision than this app.
    var onProtocolMismatch: (@MainActor () -> Void)?

    private let peripheral: CBPeripheral

    init(peripheral: CBPeripheral) {
        self.peripheral = peripheral
    }

    func discoverServices() {
        peripheral.discoverServices([mcuServiceUUID])
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices _: (any Error)?) {
        guard let service = peripheral.services?.first(where: { $0.uuid == mcuServiceUUID }) else { return }
        peripheral.discoverCharacteristics([streamCharUUID, statusCharUUID, timeSyncCharUUID], for: service)
    }

    func peripheral(
        _ peripheral: CBPeripheral,
        didDiscoverCharacteristicsFor service: CBService,
        error _: (any Error)?
    ) {
        guard let chars = service.characteristics else { return }
        for char in chars {
            switch char.uuid {
            case streamCharUUID:
                peripheral.setNotifyValue(true, for: char)
            case statusCharUUID:
                peripheral.readValue(for: char)
                peripheral.setNotifyValue(true, for: char)
            case timeSyncCharUUID:
                var ts = UInt32(Date().timeIntervalSince1970).littleEndian
                let data = Data(bytes: &ts, count: 4)
                peripheral.writeValue(data, for: char, type: .withResponse)
            default:
                break
            }
        }
    }

    func peripheral(
        _: CBPeripheral,
        didUpdateNotificationStateFor characteristic: CBCharacteristic,
        error: (any Error)?
    ) {
        // A failed CCCD write means notifications never arm: the connection looks healthy but
        // no data will ever flow. Don't let that be silent.
        if let error {
            log.error("enabling notifications failed for \(characteristic.uuid): \(error)")
        }
    }

    func peripheral(
        _: CBPeripheral,
        didUpdateValueFor characteristic: CBCharacteristic,
        error: (any Error)?
    ) {
        guard error == nil, let data = characteristic.value else { return }
        // CoreBluetooth delivers these callbacks on the central manager's queue, which is
        // `.main` (see `BLEManager.init`), so we're already on the main actor. Calling the
        // `@MainActor` callbacks synchronously avoids a per-point `Task` hop — and the runloop
        // of latency and reorder risk that came with it.
        switch characteristic.uuid {
        case streamCharUUID:
            if let point = unpackDataPoint(bytes: data) {
                MainActor.assumeIsolated { onDataPoint?(point) }
            } else {
                // Version-checked decode failed. Silently dropping these reads as "connected
                // but no data" — worse, the MCU pops each point once it's delivered, so a
                // mismatched app quietly *erases* the ride from the device. Make it loud.
                let versions = "firmware byte0: \(data.first ?? 0), app expects v\(protocolVersion())"
                log.error("dropping data point: protocol mismatch (\(data.count) bytes, \(versions))")
                MainActor.assumeIsolated { onProtocolMismatch?() }
            }
        case statusCharUUID:
            if let status = unpackDeviceStatus(bytes: data) {
                MainActor.assumeIsolated { onStatusUpdate?(status) }
            } else if !data.allSatisfy({ $0 == 0 }) {
                // All-zero is the characteristic's placeholder value before the MCU writes the
                // first real snapshot — not a mismatch.
                log.error("dropping device status: protocol mismatch (app expects v\(protocolVersion()))")
                MainActor.assumeIsolated { onProtocolMismatch?() }
            }
        default:
            break
        }
    }
}
