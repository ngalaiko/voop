import CoreLocation
import Foundation
import Observation

@MainActor
@Observable
final class AppModel {
    let ble: BLEManager
    let health: HealthKitService
    let pointStore: PointStore
    let settings: AppSettings
    let activityController = RideActivityController()

    private(set) var detectedRides: [Ride] = []
    private(set) var isDevicePaired: Bool = UserDefaults.standard.bool(forKey: "isDevicePaired")
    private(set) var currentRpm: Int = 0
    private(set) var currentLocation: CLLocationCoordinate2D?

    /// When a cadence reading last arrived. The sensor's battery/status report is
    /// infrequent, so this is the reliable signal that the sensor is actually live.
    private(set) var lastCadenceDate: Date?

    /// Cadence counts as "live" only if a reading arrived within this window. The MCU emits a
    /// point only when the crank moves, so a silent stream means the rider coasted/stopped — the
    /// last rpm must not linger as if they were still pedaling.
    static let liveCadenceTimeout: TimeInterval = 5

    private var lastCrankPoint: (revs: UInt16, date: Date)?

    /// In-memory mirror of every stored point, kept in detection-time order. Detection reads
    /// this instead of re-fetching the whole store from disk on every incoming point.
    private var allPoints: [RawPoint] = []
    /// Points inserted into the store but not yet flushed to disk; saved in batches.
    private var unsavedCount = 0
    private static let saveBatchSize = 10

    init() {
        ble = BLEManager()
        health = HealthKitService()
        pointStore = PointStore.openOrRecover()
        settings = AppSettings()
        reloadPoints()
        // Adopt any activity left running from a prior session *before* the first point can
        // arrive and drive `reconcileActivity`, so we never orphan it and start a duplicate.
        activityController.adoptExisting()
    }

    func markDevicePaired() {
        isDevicePaired = true
        UserDefaults.standard.set(true, forKey: "isDevicePaired")
    }

    #if DEBUG
        /// Loads synthetic rides + a fake connected device for simulator visual checks.
        func loadDemoData(riding: Bool) {
            isDevicePaired = true
            ble.applyDemoStatus()
            var rides = DemoData.rides
            if riding {
                let now = Date.now
                rides.append(DemoData.makeRide(start: now.addingTimeInterval(-12 * 60), minutes: 12,
                                               lat: 47.374, lon: 8.544, cadence: 80))
                currentRpm = 78
                lastCadenceDate = now
            }
            detectedRides = rides
        }
    #endif

    func startReceiving() async {
        for await point in ble.dataPoints {
            if let revs = point.crankRevs {
                let now = Date.now
                if let last = lastCrankPoint {
                    let dt = now.timeIntervalSince(last.date)
                    let delta = Int32(revs) - Int32(last.revs)
                    if dt > 0, delta > 0 {
                        currentRpm = Int((Double(delta) / dt * 60.0).rounded())
                    }
                }
                lastCrankPoint = (revs: revs, date: now)
                lastCadenceDate = now
            }
            if let lat = point.latMicrodeg, let lon = point.lonMicrodeg {
                currentLocation = CLLocationCoordinate2D(
                    latitude: Double(lat) / 1_000_000.0,
                    longitude: Double(lon) / 1_000_000.0
                )
            }
            // Live points arrive newest-last, so appending keeps `allPoints` in detection-time
            // order. Persist in batches rather than fsyncing the store on every point.
            allPoints.append(pointStore.insert(point))
            unsavedCount += 1
            if unsavedCount >= Self.saveBatchSize { flush() }
            redetect()
            await reconcileActivity()
        }
    }

    /// Current cadence, or 0 when the stream has gone quiet (see `liveCadenceTimeout`).
    func liveRpm(at now: Date = .now) -> Int {
        guard let last = lastCadenceDate, now.timeIntervalSince(last) < Self.liveCadenceTimeout else { return 0 }
        return currentRpm
    }

    /// Flush any inserted-but-unsaved points to disk. Cheap when there's nothing pending.
    private func flush() {
        guard unsavedCount > 0 else { return }
        try? pointStore.save()
        unsavedCount = 0
    }

    /// The ride currently in progress: the most recent segment, if the stop-pause gap since its
    /// last point hasn't elapsed yet. Shared by the live card (`MainView`) and the Live Activity.
    func ongoingRide(at now: Date = .now) -> Ride? {
        guard let last = detectedRides.last,
              now.timeIntervalSince(last.endDate) < settings.gapThreshold
        else { return nil }
        return last
    }

    /// Drives the Live Activity to match the ongoing ride (or ends it when none is in progress).
    func reconcileActivity(at now: Date = .now) async {
        guard let ride = ongoingRide(at: now) else {
            await activityController.reconcile(rideStartDate: nil, rideEndDate: nil, state: nil)
            return
        }
        let config = CalculateMetrics.Config(
            gearRatio: settings.gearRatio,
            wheelCircumferenceMeters: settings.wheelCircumferenceMeters
        )
        let distance = CalculateMetrics.cadenceDistance(points: ride.points, config: config)
        // Current speed from the live cadence (0 once the stream goes quiet), using the same
        // gear/wheel formula as `samples`.
        let rpm = liveRpm(at: now)
        let speedKph = Double(rpm) / 60.0 * config.gearRatio * config.wheelCircumferenceMeters * 3.6
        let state = RideActivityAttributes.ContentState(
            distanceMeters: distance,
            currentSpeedKph: speedKph,
            currentCadenceRpm: rpm,
            elapsedInterval: ride.startDate ... .distantFuture,
            isFinished: false
        )
        await activityController.reconcile(rideStartDate: ride.startDate, rideEndDate: ride.endDate, state: state)
    }

    /// Wall-clock loop that keeps the Live Activity honest. The data stream can't detect a ride
    /// *ending* (points stop arriving when the rider stops), so this periodic tick is what ends it.
    func runActivityHeartbeat() async {
        while !Task.isCancelled {
            // Also acts as the batch-save heartbeat, so pending points are durable within a few
            // seconds even on a slow trickle that never reaches `saveBatchSize`.
            flush()
            await reconcileActivity()
            try? await Task.sleep(for: .seconds(3))
        }
        flush()
        await activityController.end()
    }

    /// Every stored raw point as CSV, including the absolute date used for detection.
    func exportCSV() -> String {
        let points = allPoints
        var lines = ["index,receivedAt,absoluteDate,unixSeconds,monotonicMs,latMicrodeg,lonMicrodeg,crankRevs"]
        for (index, p) in points.enumerated() {
            let absolute = DetectRides.absoluteDate(for: p).ISO8601Format()
            let columns: [String] = [
                String(index),
                p.receivedAt.ISO8601Format(),
                absolute,
                p.unixSeconds.map(String.init) ?? "",
                p.monotonicMs.map(String.init) ?? "",
                p.latMicrodeg.map(String.init) ?? "",
                p.lonMicrodeg.map(String.init) ?? "",
                p.crankRevs.map(String.init) ?? "",
            ]
            lines.append(columns.joined(separator: ","))
        }
        return lines.joined(separator: "\n")
    }

    /// Writes the CSV export to a temporary file and returns its URL for sharing.
    func writeCSVExport() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appending(path: "voop-export.csv")
        try exportCSV().write(to: url, atomically: true, encoding: .utf8)
        return url
    }

    /// Removes the raw points that make up a ride, then re-derives the ride list.
    func deleteRide(_ ride: Ride) {
        let toDelete = allPoints.filter { raw in
            let date = DetectRides.absoluteDate(for: raw)
            return date >= ride.startDate && date <= ride.endDate
        }
        try? pointStore.delete(toDelete)
        let removed = Set(toDelete.map(ObjectIdentifier.init))
        allPoints.removeAll { removed.contains(ObjectIdentifier($0)) }
        redetect()
    }

    /// Loads the full store into the in-memory mirror, ordered by detection time. Sorting by
    /// `absoluteDate` (GPS time when present, else arrival time) keeps the mirror consistent with
    /// how `DetectRides` segments and times points — the store's own `receivedAt` order can differ.
    private func reloadPoints() {
        allPoints = ((try? pointStore.fetchAll()) ?? [])
            .sorted { DetectRides.absoluteDate(for: $0) < DetectRides.absoluteDate(for: $1) }
        redetect()
    }

    private func redetect() {
        detectedRides = DetectRides.detect(points: allPoints, gapThreshold: settings.gapThreshold)
    }
}
