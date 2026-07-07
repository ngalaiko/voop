import CoreLocation
import Foundation

enum DetectRides {
    /// Splits raw points into time-contiguous segments and returns each as a `Ride`.
    /// A pause longer than `gapThreshold` starts a new segment. No qualification rules
    /// are applied here — every segment becomes a ride so the live card can show
    /// in-progress values (slow rolling, short distance). Use `qualifies(_:settings:)`
    /// to decide which segments count as actual rides.
    static func detect(points: [RawPoint], gapThreshold: TimeInterval) -> [Ride] {
        guard points.count >= 2 else { return [] }

        // Resolve each point's absolute time once (pre-sync points are reconstructed from a
        // synced point in the same boot session), then order by it: a buffered batch and
        // post-reboot points don't arrive in capture order, so this restores the true timeline.
        let dates = resolvedDates(for: points)
        func date(_ p: RawPoint) -> Date { dates[ObjectIdentifier(p)] ?? absoluteDate(for: p) }
        let points = points.sorted { date($0) < date($1) }

        var segments: [[RawPoint]] = []
        var current: [RawPoint] = [points[0]]
        for point in points.dropFirst() {
            if max(0, date(point).timeIntervalSince(date(current.last!))) > gapThreshold {
                segments.append(current)
                current = [point]
            } else {
                current.append(point)
            }
        }
        segments.append(current)

        return segments.compactMap { segment in
            guard segment.count >= 2 else { return nil }

            let timestamped: [TimestampedPoint] = segment.map { raw in
                let p = raw.dataPoint
                let coord = p.latMicrodeg.flatMap { lat in
                    p.lonMicrodeg.map { lon in
                        CLLocationCoordinate2D(
                            latitude: Double(lat) / 1_000_000.0,
                            longitude: Double(lon) / 1_000_000.0
                        )
                    }
                }
                return TimestampedPoint(
                    date: date(raw),
                    uptimeMs: p.uptimeMs,
                    coordinate: coord,
                    cumulativeCrankRevs: p.crankRevs,
                    crankEventTime: p.crankEventTime
                )
            }

            return Ride(
                startDate: timestamped.first!.date,
                endDate: timestamped.last!.date,
                points: timestamped
            )
        }
    }

    /// Whether a segment is long and fast enough to count as an actual ride.
    static func qualifies(_ ride: Ride, settings: AppSettings) -> Bool {
        let config = CalculateMetrics.Config(
            gearRatio: settings.gearRatio,
            wheelCircumferenceMeters: settings.wheelCircumferenceMeters
        )
        let distance = CalculateMetrics.cadenceDistance(points: ride.points, config: config)
        let longEnough = distance >= Double(settings.minDistanceMeters)
        let fastEnough = averageCadence(points: ride.points) >= Double(settings.minCadenceRpm)
        return longEnough && fastEnough
    }

    /// Average of the per-interval cadences, timed by the same source chain as the metrics
    /// (`CalculateMetrics.intervalSeconds`), not wall-clock dates: a never-synced boot session
    /// replayed after hours collapses every date onto `receivedAt` (milliseconds apart), which
    /// made wall-clock cadence compute in the thousands of rpm — trivially "qualifying"
    /// garbage segments as rides.
    private static func averageCadence(points: [TimestampedPoint]) -> Double {
        var sum = 0.0
        var count = 0
        for i in 1 ..< points.count {
            let dt = CalculateMetrics.intervalSeconds(from: points[i - 1], to: points[i])
            guard dt > 0 else { continue }
            let delta = CalculateMetrics.crankRevDelta(from: points[i - 1], to: points[i])
            if delta > 0 {
                sum += Double(delta) / dt * 60.0
                count += 1
            }
        }
        return count > 0 ? sum / Double(count) : 0
    }

    /// Resolves an absolute date for every point. Points carrying device wall-clock time
    /// (`unixMillis`) use it directly. Pre-sync points (`unixMillis == nil`, captured after a
    /// reboot before the first GPS/iOS time sync) are reconstructed from a synced point in the
    /// same boot session via the monotonic `uptimeMs` delta, instead of collapsing onto
    /// `receivedAt`. A boot session that never synced falls back to `receivedAt`.
    static func resolvedDates(for points: [RawPoint]) -> [ObjectIdentifier: Date] {
        // Walk in arrival order. `uptimeMs` is monotonic within a boot and resets on reboot,
        // so a decrease marks a new boot session.
        let ordered = points.sorted { $0.receivedAt < $1.receivedAt }
        var result: [ObjectIdentifier: Date] = [:]

        func resolve(_ session: ArraySlice<RawPoint>) {
            // The first synced point's (unixMillis − uptimeMs) offset anchors the whole session.
            let offset = session.first { $0.unixMillis != nil }
                .flatMap { a in a.unixMillis.map { $0 - a.uptimeMs } }
            for p in session {
                if let ms = p.unixMillis {
                    result[ObjectIdentifier(p)] = Date(timeIntervalSince1970: Double(ms) / 1000)
                } else if let offset {
                    result[ObjectIdentifier(p)] =
                        Date(timeIntervalSince1970: Double(offset + p.uptimeMs) / 1000)
                } else {
                    result[ObjectIdentifier(p)] = p.receivedAt
                }
            }
        }

        var sessionStart = 0
        var i = 1
        while i < ordered.count {
            if ordered[i].uptimeMs < ordered[i - 1].uptimeMs {
                resolve(ordered[sessionStart ..< i])
                sessionStart = i
            }
            i += 1
        }
        if !ordered.isEmpty { resolve(ordered[sessionStart ..< ordered.count]) }
        return result
    }

    /// Per-point fallback time: the device's wall-clock estimate when present, else the time
    /// iOS received the point. `detect` and metrics go through `resolvedDates` instead, which
    /// also reconstructs pre-sync points; this remains for single-point/contextless callers.
    static func absoluteDate(for raw: RawPoint) -> Date {
        if let ms = raw.unixMillis {
            return Date(timeIntervalSince1970: TimeInterval(ms) / 1000.0)
        }
        return raw.receivedAt
    }
}
