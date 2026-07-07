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
    let endNotifier = RideEndNotifier()

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

    /// `lastCadenceDate` freshness as *stored* observable state. Going stale is driven by time,
    /// not data: when points stop arriving nothing mutates, so a view deriving this from
    /// `Date.now` at render time never re-renders — it shows the last answer until something
    /// unrelated invalidates it. The heartbeat drives the transition instead
    /// (see `refreshTimeDerivedState`).
    private(set) var isCadenceLive = false

    /// Identity of the ride currently in progress, or nil once the stop-pause gap has elapsed.
    /// Stored for the same reason as `isCadenceLive`: a ride *ends* by time passing, and views
    /// (the history list) must observe that transition, not re-derive it on render.
    private(set) var ongoingRideID: Ride.ID?

    private struct LastCrank {
        let revs: UInt16
        let eventTime: UInt16
        let uptimeMs: UInt32
        let date: Date
    }

    private var lastCrankPoint: LastCrank?

    /// In-memory mirror of every stored point, kept in detection-time order. Detection reads
    /// this instead of re-fetching the whole store from disk on every incoming point.
    private var allPoints: [RawPoint] = []

    /// Identity of a point as captured on the device — used to drop re-deliveries. The MCU
    /// removes a point from its buffer only after a notify it believes was delivered, so the
    /// stream is at-least-once: a disconnect mid-notify re-sends the same point on reconnect.
    private struct PointKey: Hashable {
        let uptimeMs: Int
        let crankRevs: Int
        let unixMillis: Int?
    }

    private var seenPointKeys: Set<PointKey> = []

    private static func key(of point: RawPoint) -> PointKey {
        PointKey(uptimeMs: point.uptimeMs, crankRevs: point.crankRevs, unixMillis: point.unixMillis)
    }

    /// Points inserted into the store but not yet flushed to disk; saved in batches.
    private var unsavedCount = 0
    private static let saveBatchSize = 10

    /// Set while a coalesced re-detection is pending, so a burst of points (e.g. a reconnect
    /// replay) collapses into one re-derive instead of a full O(n log n) pass per point.
    private var redetectPending = false

    /// Memoized per-ride metrics. Keyed by ride identity (stable start date), point count
    /// (rides are append-only, so a grown ride naturally misses), and the gear/wheel config.
    /// Without this the ride list recomputed every historical ride's metrics on every List
    /// render — every second while riding, over the whole history.
    private var metricsCache: [MetricsKey: RideMetrics] = [:]

    private struct MetricsKey: Hashable {
        let rideID: Ride.ID
        let pointCount: Int
        let gearRatio: Double
        let wheelCircumferenceMeters: Double
    }

    /// App-lifetime pipeline tasks. Deliberately retained (and retaining self) for the life of
    /// the process — see `init`.
    private var ingestTask: Task<Void, Never>?
    private var heartbeatTask: Task<Void, Never>?

    init() {
        ble = BLEManager()
        health = HealthKitService()
        pointStore = PointStore.openOrRecover()
        settings = AppSettings()
        reloadPoints()
        // Adopt any activity left running from a prior session *before* the first point can
        // arrive and drive `reconcileActivity`, so we never orphan it and start a duplicate.
        activityController.adoptExisting()
        // Ingestion must live as long as the process, not a view: hung off a SwiftUI `.task`,
        // a background BLE relaunch (scene never connects) would leave the stream unconsumed —
        // nothing persisted — and a scene teardown would cancel the `for await`, *finishing*
        // the single-shot AsyncStream so every later point is silently dropped.
        ingestTask = Task { await self.startReceiving() }
        heartbeatTask = Task { await self.runActivityHeartbeat() }
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
            refreshTimeDerivedState()
        }
    #endif

    private func startReceiving() async {
        for await point in ble.dataPoints {
            // Drop re-deliveries (see `PointKey`): a duplicate is a point already stored, so
            // neither the live metrics nor the store should see it again.
            let key = PointKey(
                uptimeMs: Int(point.uptimeMs),
                crankRevs: Int(point.crankRevs),
                unixMillis: point.unixMillis.map(Int.init)
            )
            guard seenPointKeys.insert(key).inserted else { continue }
            do {
                // Every point is a crank event, so it always carries a rev count. Time the
                // delta by the point's own clocks (crank event time, then MCU uptime), not BLE
                // arrival: a reconnect replay delivers points milliseconds apart while the
                // revs span real seconds — arrival-time math flashes thousands of rpm into
                // the dashboard and the Live Activity.
                let revs = point.crankRevs
                let now = Date.now
                if let last = lastCrankPoint {
                    let delta = revs &- last.revs
                    let uptimeDeltaMs = point.uptimeMs &- last.uptimeMs
                    let close = uptimeDeltaMs > 0 && uptimeDeltaMs < 60000
                    let ticks = point.crankEventTime &- last.eventTime
                    let dt: TimeInterval = if close, ticks > 0 {
                        TimeInterval(ticks) / 1024.0
                    } else if close {
                        TimeInterval(uptimeDeltaMs) / 1000.0
                    } else {
                        now.timeIntervalSince(last.date)
                    }
                    if dt > 0, delta > 0, delta < 1000 {
                        currentRpm = Int((Double(delta) / dt * 60.0).rounded())
                    }
                }
                lastCrankPoint = LastCrank(
                    revs: revs, eventTime: point.crankEventTime, uptimeMs: point.uptimeMs, date: now
                )
                lastCadenceDate = now
                if !isCadenceLive { isCadenceLive = true }
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
            scheduleRedetect()
            // No per-point Activity.update: at 1 Hz riding (or thousands of points in a replay
            // burst) it floods the system's update budget while `detectedRides` is still stale
            // behind the redetect debounce anyway. The 3 s heartbeat reconciles instead.
        }
    }

    /// Cached metrics for a ride (see `metricsCache`).
    func metrics(for ride: Ride) -> RideMetrics {
        let key = MetricsKey(
            rideID: ride.id,
            pointCount: ride.points.count,
            gearRatio: settings.gearRatio,
            wheelCircumferenceMeters: settings.wheelCircumferenceMeters
        )
        if let cached = metricsCache[key] { return cached }
        // Stale keys (a growing ride, changed settings) accumulate slowly; reset rather than
        // track precise invalidation.
        if metricsCache.count > 10000 { metricsCache.removeAll(keepingCapacity: true) }
        let computed = CalculateMetrics.compute(
            ride: ride,
            config: .init(gearRatio: settings.gearRatio,
                          wheelCircumferenceMeters: settings.wheelCircumferenceMeters)
        )
        metricsCache[key] = computed
        return computed
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
    /// last point hasn't elapsed yet. For time-driven callers (the per-second live card, the
    /// heartbeat). Views that must *react* to the ended transition observe `ongoingRideID`.
    func ongoingRide(at now: Date = .now) -> Ride? {
        guard let last = detectedRides.last,
              now.timeIntervalSince(last.endDate) < settings.gapThreshold
        else { return nil }
        return last
    }

    /// Recomputes the observable state that decays with time rather than with data
    /// (`ongoingRideID`, `isCadenceLive`). Assigns only on change so the 3 s heartbeat doesn't
    /// invalidate observing views when nothing actually transitioned.
    private func refreshTimeDerivedState(at now: Date = .now) {
        let ongoing = ongoingRide(at: now)?.id
        if ongoingRideID != ongoing { ongoingRideID = ongoing }
        let live = lastCadenceDate.map { now.timeIntervalSince($0) < Self.liveCadenceTimeout } ?? false
        if isCadenceLive != live { isCadenceLive = live }
    }

    /// Drives the Live Activity to match the ongoing ride (or ends it when none is in progress),
    /// and keeps the pending ride-end notification aimed at the moment the ride will end.
    func reconcileActivity(at now: Date = .now) async {
        guard let ride = ongoingRide(at: now) else {
            // Normally the pending ride-end notification has already fired — its fire time is
            // the same moment the ride stopped counting as ongoing. Cancelling here only sweeps
            // up the exceptions: a deleted ride, or a stale schedule from a previous session.
            endNotifier.cancelPending()
            await activityController.reconcile(rideStartDate: nil, rideEndDate: nil, staleDate: nil, state: nil)
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
            lastPointDate: ride.endDate,
            isFinished: false
        )
        // Stale once the ride would no longer count as ongoing (last point + stop-pause gap), so
        // the system stops the live timer even if the app is killed before the heartbeat ends it.
        let staleDate = ride.endDate.addingTimeInterval(settings.gapThreshold)
        // The ride-end notification fires at that same moment. Only rides that qualify get
        // one — short rolls end silently; a ride that qualifies mid-way schedules on the tick
        // it crosses the gate.
        if DetectRides.qualifies(ride, settings: settings) {
            endNotifier.reschedule(fireAt: staleDate, distanceMeters: distance,
                                   rideDuration: ride.duration, at: now)
        } else {
            endNotifier.cancelPending()
        }
        await activityController.reconcile(
            rideStartDate: ride.startDate,
            rideEndDate: ride.endDate,
            staleDate: staleDate,
            state: state
        )
    }

    /// Wall-clock loop that keeps the Live Activity honest. The data stream can't detect a ride
    /// *ending* (points stop arriving when the rider stops), so this periodic tick is what ends it.
    /// No teardown work after the loop: cancellation means the process is dying, not the ride —
    /// ending the activity here would kill the one surface meant to outlive the app.
    private func runActivityHeartbeat() async {
        while !Task.isCancelled {
            // Also acts as the batch-save heartbeat, so pending points are durable within a few
            // seconds even on a slow trickle that never reaches `saveBatchSize`.
            flush()
            refreshTimeDerivedState()
            await reconcileActivity()
            try? await Task.sleep(for: .seconds(3))
        }
    }

    /// Every stored raw point as CSV, including the absolute date used for detection.
    func exportCSV() -> String {
        let points = allPoints
        let dates = DetectRides.resolvedDates(for: points)
        let header = ["index", "receivedAt", "absoluteDate", "uptimeMs", "unixMillis",
                      "latMicrodeg", "lonMicrodeg", "crankRevs", "crankEventTime"]
        var lines = [header.joined(separator: ",")]
        for (index, p) in points.enumerated() {
            let absolute = (dates[ObjectIdentifier(p)] ?? DetectRides.absoluteDate(for: p)).ISO8601Format()
            let columns: [String] = [
                String(index),
                p.receivedAt.ISO8601Format(),
                absolute,
                String(p.uptimeMs),
                p.unixMillis.map(String.init) ?? "",
                p.latMicrodeg.map(String.init) ?? "",
                p.lonMicrodeg.map(String.init) ?? "",
                String(p.crankRevs),
                String(p.crankEventTime),
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
        // Use the same resolved (boot-session-aware) dates as detection, so a ride containing
        // reconstructed pre-sync points selects exactly those points and leaves no orphans.
        let dates = DetectRides.resolvedDates(for: allPoints)
        let toDelete = allPoints.filter { raw in
            let date = dates[ObjectIdentifier(raw)] ?? DetectRides.absoluteDate(for: raw)
            return date >= ride.startDate && date <= ride.endDate
        }
        try? pointStore.delete(toDelete)
        let removed = Set(toDelete.map(ObjectIdentifier.init))
        allPoints.removeAll { removed.contains(ObjectIdentifier($0)) }
        seenPointKeys.subtract(toDelete.map(Self.key(of:)))
        redetect()
    }

    /// Loads the full store into the in-memory mirror, ordered by detection time. Sorting by
    /// `absoluteDate` (GPS time when present, else arrival time) keeps the mirror consistent with
    /// how `DetectRides` segments and times points — the store's own `receivedAt` order can differ.
    private func reloadPoints() {
        allPoints = ((try? pointStore.fetchAll()) ?? [])
            .sorted { DetectRides.absoluteDate(for: $0) < DetectRides.absoluteDate(for: $1) }
        seenPointKeys = Set(allPoints.map(Self.key(of:)))
        redetect()
    }

    private func redetect() {
        detectedRides = DetectRides.detect(points: allPoints, gapThreshold: settings.gapThreshold)
        // In the same pass, so there is no window where a fresh segment sits in `detectedRides`
        // but isn't yet excluded from the history list as the ongoing ride.
        refreshTimeDerivedState()
    }

    /// Coalesced re-detection for the hot per-point path: collapses a burst of points (e.g. a
    /// reconnect replay) into a single re-derive within the debounce window, rather than running
    /// a full pass per point. One-shot callers (`reloadPoints`, `deleteRide`) use `redetect`.
    private func scheduleRedetect() {
        guard !redetectPending else { return }
        redetectPending = true
        Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard let self else { return }
            redetectPending = false
            redetect()
        }
    }
}
