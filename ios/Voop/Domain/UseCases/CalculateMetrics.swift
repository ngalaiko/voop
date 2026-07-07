import CoreLocation
import Foundation

struct RideMetrics {
    let totalDistanceMeters: Double
    let durationSeconds: TimeInterval
    let averageSpeedKph: Double
    let maxSpeedKph: Double
    let averageCadenceRpm: Double
    let maxCadenceRpm: Double
}

/// One moment in a ride's time series: where you were, and how fast you were going
/// and pedaling. `elapsed` is seconds since the ride started. This is the single
/// source that feeds the route gradient, the speed/cadence chart, and per-km splits.
struct RideSample: Identifiable {
    let id: Int
    let elapsed: TimeInterval
    let coordinate: CLLocationCoordinate2D?
    let speedKph: Double
    let cadenceRpm: Double
}

enum CalculateMetrics {
    /// Gear ratio × wheel circumference in meters, used to convert crank revs to distance.
    /// Defaults: 46/16 chainring, 700×25c wheel (2.105 m circumference).
    struct Config {
        var gearRatio: Double = 46.0 / 16.0
        var wheelCircumferenceMeters: Double = 2.105
    }

    /// Distance derived from crank revolutions, gear ratio, and wheel circumference.
    /// Sums forward crank-rev deltas (wrap-aware — see `crankRevDelta`).
    static func cadenceDistance(points: [TimestampedPoint], config: Config = .init()) -> Double {
        guard points.count >= 2 else { return 0 }
        var total = 0.0
        for i in 1 ..< points.count {
            let revDelta = crankRevDelta(from: points[i - 1], to: points[i])
            if revDelta > 0 {
                total += Double(revDelta) * config.gearRatio * config.wheelCircumferenceMeters
            }
        }
        return total
    }

    /// Crank-rev advance between two points, wrap-aware. Within a confirmed-close interval
    /// (uptime gap < 60 s, the same gate the event-time path uses) the u16 counter can wrap at
    /// most once and a real advance can't exceed ~200 revs — so the wrapping delta is trusted
    /// when small, and a large one reads as a sensor reset (battery pull) contributing nothing.
    /// Without that bound (boot/pause boundary) fall back to dropping any non-forward delta:
    /// there a wrap and a reset are indistinguishable.
    static func crankRevDelta(from prev: TimestampedPoint, to cur: TimestampedPoint) -> Int {
        let uptimeDeltaMs = cur.uptimeMs &- prev.uptimeMs
        if uptimeDeltaMs > 0, uptimeDeltaMs < 60000 {
            let wrapped = cur.cumulativeCrankRevs &- prev.cumulativeCrankRevs
            return wrapped < 1000 ? Int(wrapped) : 0
        }
        let delta = Int32(cur.cumulativeCrankRevs) - Int32(prev.cumulativeCrankRevs)
        return delta > 0 ? Int(delta) : 0
    }

    static func compute(ride: Ride, config: Config = .init()) -> RideMetrics {
        let series = samples(ride: ride, config: config)
        // Averages count only intervals where the crank actually advanced (a coast
        // reads as 0); maxes scan the whole series. This preserves the original
        // behavior while keeping `samples` the one place the speed formula lives.
        let moving = series.filter { $0.cadenceRpm > 0 }
        func mean(_ values: [Double]) -> Double {
            values.isEmpty ? 0 : values.reduce(0, +) / Double(values.count)
        }

        return RideMetrics(
            totalDistanceMeters: cadenceDistance(points: ride.points, config: config),
            durationSeconds: ride.duration,
            averageSpeedKph: mean(moving.map(\.speedKph)),
            maxSpeedKph: series.map(\.speedKph).max() ?? 0,
            averageCadenceRpm: mean(moving.map(\.cadenceRpm)),
            maxCadenceRpm: series.map(\.cadenceRpm).max() ?? 0
        )
    }

    /// Per-point time series of speed and cadence, derived the same way as the metrics
    /// (crank-rev deltas → distance → speed). Index 0 is the ride start with no motion
    /// yet; each later sample covers the interval ending at that point. Intervals with
    /// no crank advance read as 0 — a coast — which keeps the chart continuous and honest.
    static func samples(ride: Ride, config: Config = .init()) -> [RideSample] {
        let points = ride.points
        guard let start = points.first?.date else { return [] }

        var result: [RideSample] = [
            RideSample(id: 0, elapsed: 0, coordinate: points[0].coordinate, speedKph: 0, cadenceRpm: 0),
        ]

        for i in 1 ..< points.count {
            let interval = intervalSeconds(from: points[i - 1], to: points[i])
            var speedKph = 0.0
            var cadenceRpm = 0.0
            if interval > 0 {
                let revDelta = crankRevDelta(from: points[i - 1], to: points[i])
                if revDelta > 0 {
                    cadenceRpm = Double(revDelta) / interval * 60.0
                    let distanceM = Double(revDelta) * config.gearRatio * config.wheelCircumferenceMeters
                    speedKph = (distanceM / interval) * 3.6
                }
            }
            result.append(RideSample(
                id: i,
                elapsed: points[i].date.timeIntervalSince(start),
                coordinate: points[i].coordinate,
                speedKph: speedKph,
                cadenceRpm: cadenceRpm
            ))
        }
        return result
    }

    /// Seconds over which a crank-rev delta accrued, from rawest to coarsest source:
    ///   1. the sensor's "Last Crank Event Time" delta (1/1024 s, measured at the crank —
    ///      free of all BLE/MCU jitter), trusted only when the raw uptime gap confirms the
    ///      points are < 64 s apart so the u16 event-time counter wrapped at most once;
    ///   2. the raw MCU uptime delta (ms, always present, monotonic within a boot session);
    ///   3. the wall-clock date delta — last resort for pre-sync points dated by `receivedAt`.
    private static func intervalSeconds(from prev: TimestampedPoint, to cur: TimestampedPoint) -> TimeInterval {
        let uptimeDeltaMs = cur.uptimeMs &- prev.uptimeMs // u32 wrapping; implausibly large on reboot
        let uptimeValid = uptimeDeltaMs > 0 && uptimeDeltaMs < 60000
        if uptimeValid, let a = prev.crankEventTime, let b = cur.crankEventTime {
            let ticks = b &- a // u16 wrapping delta, in 1/1024 s
            if ticks > 0 { return TimeInterval(ticks) / 1024.0 }
        }
        if uptimeValid { return TimeInterval(uptimeDeltaMs) / 1000.0 }
        return cur.date.timeIntervalSince(prev.date) // last resort: wall clock
    }
}
