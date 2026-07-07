@preconcurrency import CoreBluetooth
import Foundation
import Observation

private nonisolated(unsafe) let mcuServiceUUID = CBUUID(string: serviceUuid())

@MainActor
@Observable
final class BLEManager: NSObject {
    private(set) var connectionState: ConnectionState = .idle
    private(set) var deviceStatus: DeviceStatus?
    /// True when the connected firmware speaks a different protocol revision than this app —
    /// its data can't be decoded (and is being lost), so the UI should say so.
    private(set) var protocolMismatch = false

    private var central: CBCentralManager?
    private var peripheral: MCUPeripheral?
    private var connectingPeripheral: CBPeripheral?
    /// Peripheral handed back by state restoration before the manager is powered on; resumed
    /// (rediscover or reconnect) once `centralManagerDidUpdateState` reports `.poweredOn`.
    private var restoredPeripheral: CBPeripheral?

    /// Doubling delay before the next rescan after a failure, so a device at range edge doesn't
    /// spin in a tight scan→connect→fail loop (the MCU side has the matching backoff).
    private var rescanDelay: TimeInterval = 1
    private var rescanTask: Task<Void, Never>?

    private var dataPointContinuation: AsyncStream<DataPoint>.Continuation?
    private(set) var dataPoints: AsyncStream<DataPoint>

    enum ConnectionState: Equatable {
        case idle, scanning, connecting, connected, disconnected(Error?)

        static func == (lhs: ConnectionState, rhs: ConnectionState) -> Bool {
            switch (lhs, rhs) {
            case (.idle, .idle), (.scanning, .scanning), (.connecting, .connecting), (.connected, .connected),
                 (.disconnected, .disconnected): true
            default: false
            }
        }
    }

    override init() {
        var continuation: AsyncStream<DataPoint>.Continuation!
        dataPoints = AsyncStream { continuation = $0 }
        dataPointContinuation = continuation
        super.init()
        central = CBCentralManager(
            delegate: self,
            queue: .main,
            options: [CBCentralManagerOptionRestoreIdentifierKey: "com.voop.central"]
        )
    }

    func startScan() {
        guard central?.state == .poweredOn else { return }
        connectionState = .scanning
        central?.scanForPeripherals(withServices: [mcuServiceUUID], options: nil)
    }

    func stopScan() {
        central?.stopScan()
        connectionState = .idle
    }

    /// Schedules a rescan after the current backoff delay, doubling it (capped at 30 s) for the
    /// next failure. `didConnect` resets the delay.
    private func scheduleRescan() {
        rescanTask?.cancel()
        let delay = rescanDelay
        rescanDelay = min(rescanDelay * 2, 30)
        rescanTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            self?.startScan()
        }
    }

    /// Wraps a CoreBluetooth peripheral in an `MCUPeripheral` and wires its callbacks. Used on
    /// fresh connects and on state restoration — the restored peripheral must get its delegate
    /// back immediately, or notifications delivered to a relaunched app go nowhere.
    private func attach(_ cbPeripheral: CBPeripheral) -> MCUPeripheral {
        let mcu = MCUPeripheral(peripheral: cbPeripheral)
        mcu.onDataPoint = { [weak self] point in
            self?.dataPointContinuation?.yield(point)
        }
        mcu.onStatusUpdate = { [weak self] status in
            self?.deviceStatus = status
        }
        mcu.onProtocolMismatch = { [weak self] in
            self?.protocolMismatch = true
        }
        protocolMismatch = false
        peripheral = mcu
        cbPeripheral.delegate = mcu
        return mcu
    }

    #if DEBUG
        /// Forces a connected state with a sample battery report, for simulator visual checks.
        func applyDemoStatus() {
            connectionState = .connected
            deviceStatus = DeviceStatus(
                mcuBattery: BatteryStatus(percent: 85, state: .discharging),
                sensorConnected: true,
                sensorBattery: 72
            )
        }
    #endif
}

extension BLEManager: CBCentralManagerDelegate {
    nonisolated func centralManager(_: CBCentralManager, willRestoreState dict: [String: Any]) {
        // Extract the peripheral before crossing into the main actor — dict is [String: Any]
        // which isn't Sendable, so it must not be captured by the assumeIsolated closure.
        guard let peripherals = dict[CBCentralManagerRestoredStatePeripheralsKey] as? [CBPeripheral],
              let p = peripherals.first
        else { return }
        MainActor.assumeIsolated {
            // The manager is still `.unknown` here — a connect() now would be dropped as API
            // misuse, never queued. Reattach the delegate immediately (the app may have been
            // relaunched *because* this peripheral sent a notification; without a delegate,
            // iOS's stack acks data the app never sees while the MCU drains its buffer) and
            // defer connect/rediscover to the poweredOn transition.
            restoredPeripheral = p
            _ = attach(p)
            connectionState = .connecting
        }
    }

    nonisolated func centralManagerDidUpdateState(_ central: CBCentralManager) {
        MainActor.assumeIsolated {
            switch central.state {
            case .poweredOn:
                if let p = restoredPeripheral {
                    restoredPeripheral = nil
                    if p.state == .connected {
                        // Still connected at the system level (background relaunch). A
                        // connected peripheral doesn't advertise, so scanning would never
                        // find it — rediscover services and re-arm notifications instead.
                        connectionState = .connected
                        peripheral?.discoverServices()
                    } else {
                        connectingPeripheral = p
                        connectionState = .connecting
                        central.connect(p, options: nil)
                    }
                } else {
                    startScan()
                }
            case .poweredOff, .unauthorized, .unsupported:
                // Radio gone: drop the session so the UI doesn't keep claiming "Connected"
                // with a stale status. CoreBluetooth invalidates the peripherals anyway.
                peripheral = nil
                connectingPeripheral = nil
                restoredPeripheral = nil
                deviceStatus = nil
                connectionState = .disconnected(nil)
            default:
                break
            }
        }
    }

    nonisolated func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData _: [String: Any],
        rssi _: NSNumber
    ) {
        MainActor.assumeIsolated {
            central.stopScan()
            connectionState = .connecting
            connectingPeripheral = peripheral
            central.connect(peripheral, options: nil)
        }
    }

    nonisolated func centralManager(_: CBCentralManager, didConnect peripheral: CBPeripheral) {
        MainActor.assumeIsolated {
            connectingPeripheral = nil
            rescanTask?.cancel()
            rescanDelay = 1
            let mcu = attach(peripheral)
            mcu.discoverServices()
            connectionState = .connected
        }
    }

    nonisolated func centralManager(
        _: CBCentralManager,
        didDisconnectPeripheral _: CBPeripheral,
        error: (any Error)?
    ) {
        MainActor.assumeIsolated {
            self.peripheral = nil
            self.connectingPeripheral = nil
            connectionState = .disconnected(error)
            scheduleRescan()
        }
    }

    nonisolated func centralManager(
        _: CBCentralManager,
        didFailToConnect _: CBPeripheral,
        error: (any Error)?
    ) {
        MainActor.assumeIsolated {
            connectingPeripheral = nil
            connectionState = .disconnected(error)
            scheduleRescan()
        }
    }
}
