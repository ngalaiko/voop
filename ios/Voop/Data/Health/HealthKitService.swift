import CoreLocation
import Foundation
import HealthKit
import os

private let log = Logger(subsystem: "com.galaiko.voop", category: "health")

/// Mirrors ended rides into Apple Health as cycling workouts — total distance, per-interval
/// cadence samples, and the GPS route. Which rides are already mirrored is tracked in a local
/// ledger keyed by the ride's stable identity (its start date), not by reading workouts back:
/// read access is privacy-gated, and a denial must not read as "nothing synced yet".
@MainActor
final class HealthKitService {
    private let store = HKHealthStore()

    /// Everything a ride writes. Workout type first — it's the one `canSave` gates on; the
    /// sample types degrade individually (a per-type denial just omits that data from the
    /// workout).
    private static let shareTypes: [HKSampleType] = [
        HKObjectType.workoutType(),
        HKQuantityType(.distanceCycling),
        HKQuantityType(.cyclingCadence),
        HKSeriesType.workoutRoute(),
    ]

    /// What's been mirrored, persisted across launches. `pointCount` doubles as the version:
    /// rides are append-only, so a count mismatch means the ride grew after it was synced
    /// (a late replay of buffered points) and its workout must be replaced.
    private struct SyncedRide: Codable {
        let pointCount: Int
        let workoutID: UUID
    }

    private static let ledgerKey = "healthKitSyncedRides"
    private var ledger: [String: SyncedRide]

    /// Rides whose save failed for a non-transient reason, skipped until the next launch so
    /// the heartbeat doesn't retry a save that will keep failing every few seconds. Transient
    /// failures (the store is sealed while the phone is locked) stay retryable.
    private var failedRideIDs: Set<Ride.ID> = []

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.ledgerKey),
           let decoded = try? JSONDecoder().decode([String: SyncedRide].self, from: data) {
            ledger = decoded
        } else {
            ledger = [:]
        }
    }

    /// Ask for permission once (no-op after the user has decided). Called from the UI on
    /// foreground — same contract as `RideEndNotifier.ensureAuthorization`. Workout read
    /// access is requested only so `deleteWorkout` can look up workouts this app saved;
    /// HealthKit errors that lookup outright while read stands undecided.
    func ensureAuthorization() async {
        guard HKHealthStore.isHealthDataAvailable() else { return }
        guard Self.shareTypes.contains(where: { store.authorizationStatus(for: $0) == .notDetermined })
        else { return }
        do {
            try await store.requestAuthorization(
                toShare: Set(Self.shareTypes),
                read: [HKObjectType.workoutType()]
            )
        } catch {
            log.error("authorization request failed: \(error)")
        }
    }

    /// Whether saving can work at all: health data exists on this device and the user granted
    /// workout write access. Per-sample-type denials don't gate this — see `shareTypes`.
    var canSave: Bool {
        HKHealthStore.isHealthDataAvailable()
            && store.authorizationStatus(for: HKObjectType.workoutType()) == .sharingAuthorized
    }

    /// Cheap per-heartbeat check: does this ride need (re-)mirroring? A ledger entry with the
    /// ride's current point count means it's mirrored as-is; a deliberately failed ride reads
    /// as not needing sync so the sweep skips it.
    func needsSync(_ ride: Ride) -> Bool {
        guard !failedRideIDs.contains(ride.id) else { return false }
        return ledger[Self.key(for: ride.id)]?.pointCount != ride.points.count
    }

    /// Mirror one ride: replace its previous workout if the ride grew, save the new one,
    /// record it. Errors are contained here — sync is best-effort and the sweep moves on.
    func sync(ride: Ride, metrics: RideMetrics, samples: [RideSample]) async {
        let key = Self.key(for: ride.id)
        do {
            if let previous = ledger[key] {
                try await deleteWorkout(id: previous.workoutID)
            }
            let workoutID = try await saveWorkout(ride: ride, metrics: metrics, samples: samples)
            ledger[key] = SyncedRide(pointCount: ride.points.count, workoutID: workoutID)
            persistLedger()
            log.info("synced ride \(ride.id) as workout \(workoutID)")
        } catch {
            if (error as? HKError)?.code != .errorDatabaseInaccessible {
                failedRideIDs.insert(ride.id)
            }
            log.error("failed to sync ride \(ride.id): \(error)")
        }
    }

    /// Delete the mirrored workout of a ride removed in-app. Only ever touches the workout
    /// this app saved for it (looked up by the UUID recorded at sync time); one the user
    /// already deleted in Health is a silent no-op.
    func remove(rideID: Ride.ID) async {
        let key = Self.key(for: rideID)
        guard let record = ledger[key] else { return }
        ledger[key] = nil
        persistLedger()
        do {
            try await deleteWorkout(id: record.workoutID)
        } catch {
            log.error("failed to delete workout for ride \(rideID): \(error)")
        }
    }

    // MARK: - Saving

    private enum SyncError: Error {
        case workoutFinishFailed
    }

    private func saveWorkout(ride: Ride, metrics: RideMetrics, samples: [RideSample]) async throws -> UUID {
        let configuration = HKWorkoutConfiguration()
        configuration.activityType = .cycling
        configuration.locationType = .outdoor
        let builder = HKWorkoutBuilder(healthStore: store, configuration: configuration, device: nil)
        try await builder.beginCollection(at: ride.startDate)
        try await builder.addMetadata([HKMetadataKeyIndoorWorkout: false])

        var hkSamples: [HKSample] = []
        if authorized(HKQuantityType(.distanceCycling)), metrics.totalDistanceMeters > 0 {
            hkSamples.append(HKQuantitySample(
                type: HKQuantityType(.distanceCycling),
                quantity: HKQuantity(unit: .meter(), doubleValue: metrics.totalDistanceMeters),
                start: ride.startDate,
                end: ride.endDate
            ))
        }
        if authorized(HKQuantityType(.cyclingCadence)) {
            hkSamples.append(contentsOf: Self.cadenceSamples(startDate: ride.startDate, samples: samples))
        }
        if !hkSamples.isEmpty {
            try await builder.addSamples(hkSamples)
        }
        try await builder.endCollection(at: ride.endDate)
        guard let workout = try await builder.finishWorkout() else {
            throw SyncError.workoutFinishFailed
        }

        // Past this point the workout exists: a route failure is logged but must not throw,
        // or the retry path would save a second workout the ledger doesn't know about.
        let route = Self.routeLocations(points: ride.points)
        if route.count >= 2, authorized(HKSeriesType.workoutRoute()) {
            do {
                let routeBuilder = HKWorkoutRouteBuilder(healthStore: store, device: nil)
                try await routeBuilder.insertRouteData(route)
                _ = try await routeBuilder.finishRoute(with: workout, metadata: nil)
            } catch {
                log.error("failed to attach route to workout \(workout.uuid): \(error)")
            }
        }
        return workout.uuid
    }

    private func authorized(_ type: HKSampleType) -> Bool {
        store.authorizationStatus(for: type) == .sharingAuthorized
    }

    /// One cadence sample per interval where the crank actually advanced, spanning the same
    /// wall-clock dates as the ride's points (index-aligned with `samples`; the elapsed
    /// offsets were derived from those dates). Coasts contribute no sample — that's how
    /// cadence sensors record — and implausible spikes from degenerate intervals are dropped
    /// rather than clamped into fiction.
    private static func cadenceSamples(startDate: Date, samples: [RideSample]) -> [HKQuantitySample] {
        let type = HKQuantityType(.cyclingCadence)
        let unit = HKUnit.count().unitDivided(by: .minute())
        var result: [HKQuantitySample] = []
        for i in samples.indices.dropFirst() {
            let rpm = samples[i].cadenceRpm
            guard rpm > 0, rpm <= 300 else { continue }
            let start = startDate.addingTimeInterval(samples[i - 1].elapsed)
            let end = startDate.addingTimeInterval(samples[i].elapsed)
            guard end >= start else { continue }
            result.append(HKQuantitySample(
                type: type,
                quantity: HKQuantity(unit: unit, doubleValue: rpm),
                start: start,
                end: end
            ))
        }
        return result
    }

    /// The ride's GPS fixes as route locations. HealthKit wants timestamps strictly
    /// increasing, so collapsed dates (a never-synced boot session falls back to arrival
    /// time) are skipped rather than failing the whole route. Accuracy is nominal — the MCU
    /// doesn't report it — and altitude is marked invalid.
    private static func routeLocations(points: [TimestampedPoint]) -> [CLLocation] {
        var result: [CLLocation] = []
        var lastDate = Date.distantPast
        for point in points {
            guard let coordinate = point.coordinate, CLLocationCoordinate2DIsValid(coordinate),
                  point.date > lastDate
            else { continue }
            lastDate = point.date
            result.append(CLLocation(
                coordinate: coordinate,
                altitude: 0,
                horizontalAccuracy: 10,
                verticalAccuracy: -1,
                timestamp: point.date
            ))
        }
        return result
    }

    // MARK: - Deleting

    /// Delete a workout this app saved, and the samples/route attached to it. The lookup is
    /// a read, but read access was requested at authorization time and app-created objects
    /// remain visible even if the user denied it. Not finding the workout (deleted by the
    /// user in Health) counts as done.
    private func deleteWorkout(id: UUID) async throws {
        let descriptor = HKSampleQueryDescriptor(
            predicates: [.workout(HKQuery.predicateForObject(with: id))],
            sortDescriptors: [],
            limit: 1
        )
        guard let workout = try await descriptor.result(for: store).first else { return }
        let associated = HKQuery.predicateForObjects(from: workout)
        let attachedTypes: [HKSampleType] = [
            HKQuantityType(.distanceCycling),
            HKQuantityType(.cyclingCadence),
            HKSeriesType.workoutRoute(),
        ]
        for type in attachedTypes {
            // A type that was never granted was never written; deleting nothing throws a
            // not-found error that means exactly that.
            _ = try? await store.deleteObjects(of: type, predicate: associated)
        }
        try await store.delete(workout)
    }

    // MARK: - Ledger

    private static func key(for id: Ride.ID) -> String {
        String(id.timeIntervalSince1970)
    }

    private func persistLedger() {
        UserDefaults.standard.set(try? JSONEncoder().encode(ledger), forKey: Self.ledgerKey)
    }
}
