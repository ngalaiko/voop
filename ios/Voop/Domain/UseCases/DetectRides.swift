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

        var segments: [[RawPoint]] = []
        var current: [RawPoint] = [points[0]]
        for point in points.dropFirst() {
            if elapsed(from: current.last!, to: point) > gapThreshold {
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
                    date: absoluteDate(for: raw),
                    coordinate: coord,
                    cumulativeCrankRevs: p.crankRevs ?? 0
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

    private static func averageCadence(points: [TimestampedPoint]) -> Double {
        var sum = 0.0
        var count = 0
        for i in 1 ..< points.count {
            let dt = points[i].date.timeIntervalSince(points[i - 1].date)
            guard dt > 0 else { continue }
            let delta = Int32(points[i].cumulativeCrankRevs) - Int32(points[i - 1].cumulativeCrankRevs)
            if delta > 0 {
                sum += Double(delta) / dt * 60.0
                count += 1
            }
        }
        return count > 0 ? sum / Double(count) : 0
    }

    private static func elapsed(from a: RawPoint, to b: RawPoint) -> TimeInterval {
        max(0, absoluteDate(for: b).timeIntervalSince(absoluteDate(for: a)))
    }

    /// The time used for detection and metrics: the device's GPS time when available,
    /// otherwise the time iOS received the point. The two agree within ~1s for
    /// live-streamed points, so `receivedAt` is a reliable fallback when there's no fix.
    static func absoluteDate(for raw: RawPoint) -> Date {
        switch raw.dataPoint.time {
        case let .unix(s):
            Date(timeIntervalSince1970: TimeInterval(s))
        case .monotonic:
            raw.receivedAt
        }
    }
}
