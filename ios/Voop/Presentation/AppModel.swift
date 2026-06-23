import Foundation
import Observation

@MainActor
@Observable
final class AppModel {
    let ble: BLEManager
    let health: HealthKitService
    let rides: RideStore

    private(set) var pendingPoints: [DataPoint] = []
    private(set) var gpsAnchor: GpsAnchor?

    private(set) var isDevicePaired: Bool = UserDefaults.standard.bool(forKey: "isDevicePaired")

    init() {
        ble = BLEManager()
        health = HealthKitService()
        rides = (try? RideStore()) ?? { fatalError("Failed to create RideStore") }()
    }

    func markDevicePaired() {
        isDevicePaired = true
        UserDefaults.standard.set(true, forKey: "isDevicePaired")
    }

    func startReceiving() async {
        for await point in ble.dataPoints {
            pendingPoints.append(point)
            if gpsAnchor == nil, point.latMicrodeg != nil {
                gpsAnchor = GpsAnchor(monotonicMs: point.monotonicMs, wallClockDate: Date())
            }
        }
    }

    func syncAndSave() async {
        let detectedRides = DetectRides.detect(points: pendingPoints, anchor: gpsAnchor)
        for ride in detectedRides {
            try? await rides.save(ride)
            let metrics = CalculateMetrics.compute(ride: ride)
            try? await health.save(ride: ride, metrics: metrics)
        }
        pendingPoints.removeAll()
        gpsAnchor = nil
    }
}
