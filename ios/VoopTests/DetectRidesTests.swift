import Foundation
import Testing
@testable import Voop

struct DetectRidesTests {
    /// A point timed by a device unix observation, so `absoluteDate` is deterministic
    /// (independent of `receivedAt`) and the gap arithmetic is exact. Takes seconds for
    /// readability and widens to the wire's millisecond unit.
    private func point(unix: UInt32, revs: UInt16 = 0) -> RawPoint {
        RawPoint(from: DataPoint(uptimeMs: unix, unixMillis: UInt64(unix) * 1000,
                                 latMicrodeg: nil, lonMicrodeg: nil, crankRevs: revs, crankEventTime: 0))
    }

    @Test func splitsSegmentsOnGapLongerThanThreshold() {
        let points = [
            point(unix: 1000, revs: 0),
            point(unix: 1001, revs: 5),
            // 99 s gap > 60 s threshold → new segment
            point(unix: 1100, revs: 0),
            point(unix: 1101, revs: 5),
        ]
        let rides = DetectRides.detect(points: points, gapThreshold: 60)
        #expect(rides.count == 2)
        #expect(rides[0].startDate == Date(timeIntervalSince1970: 1000))
        #expect(rides[1].startDate == Date(timeIntervalSince1970: 1100))
    }

    @Test func keepsContiguousPointsInOneRide() {
        let points = (0 ..< 5).map { point(unix: 1000 + UInt32($0), revs: UInt16($0)) }
        let rides = DetectRides.detect(points: points, gapThreshold: 60)
        #expect(rides.count == 1)
        #expect(rides[0].points.count == 5)
    }

    @Test func dropsSinglePointSegments() {
        let points = [
            point(unix: 1000, revs: 0),
            point(unix: 1001, revs: 5),
            point(unix: 2000, revs: 0), // isolated by a big gap → segment of 1, dropped
        ]
        let rides = DetectRides.detect(points: points, gapThreshold: 60)
        #expect(rides.count == 1)
    }

    @Test func emptyOrSingleInputProducesNoRides() {
        #expect(DetectRides.detect(points: [], gapThreshold: 60).isEmpty)
        #expect(DetectRides.detect(points: [point(unix: 1000)], gapThreshold: 60).isEmpty)
    }

    @Test func absoluteDateUsesUnixTimeWhenPresent() {
        #expect(DetectRides.absoluteDate(for: point(unix: 1_234_567)) == Date(timeIntervalSince1970: 1_234_567))
    }

    @Test func sortsOutOfOrderPointsByReconstructedTime() {
        // A buffered batch replays newest-first, so arrival order ≠ capture order. Detection
        // must reorder by each point's own reconstructed time into one contiguous ride.
        let points = [
            point(unix: 1003, revs: 30),
            point(unix: 1000, revs: 0),
            point(unix: 1002, revs: 20),
            point(unix: 1001, revs: 10),
        ]
        let rides = DetectRides.detect(points: points, gapThreshold: 60)
        #expect(rides.count == 1)
        #expect(rides[0].startDate == Date(timeIntervalSince1970: 1000))
        #expect(rides[0].points.map(\.cumulativeCrankRevs) == [0, 10, 20, 30])
    }

    @Test func reconstructsPreSyncPointsFromBootSessionAnchor() {
        // Points captured after boot but before the first time sync carry no unixMillis. They
        // must be reconstructed from a later synced point in the same boot via uptimeMs deltas,
        // not collapsed onto receivedAt (here a much later, unrelated arrival time).
        func raw(uptimeMs: UInt32, unixMillis: UInt64?, revs: UInt16, at: TimeInterval) -> RawPoint {
            RawPoint(from: DataPoint(uptimeMs: uptimeMs, unixMillis: unixMillis,
                                     latMicrodeg: nil, lonMicrodeg: nil,
                                     crankRevs: revs, crankEventTime: 0),
                     receivedAt: Date(timeIntervalSince1970: at))
        }
        // First sync at uptime 3000 ms = unix 1_000_000_003 s → offset 1_000_000_000_000 ms.
        let points = [
            raw(uptimeMs: 1000, unixMillis: nil, revs: 0, at: 9_000_000),
            raw(uptimeMs: 2000, unixMillis: nil, revs: 10, at: 9_000_001),
            raw(uptimeMs: 3000, unixMillis: 1_000_000_003_000, revs: 20, at: 9_000_002),
        ]
        let rides = DetectRides.detect(points: points, gapThreshold: 60)
        #expect(rides.count == 1)
        #expect(rides[0].startDate == Date(timeIntervalSince1970: 1_000_000_001))
        #expect(rides[0].endDate == Date(timeIntervalSince1970: 1_000_000_003))
        #expect(rides[0].points.map(\.cumulativeCrankRevs) == [0, 10, 20])
    }
}
