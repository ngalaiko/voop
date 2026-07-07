import CoreLocation
import Testing
@testable import Voop

struct CalculateMetricsTests {
    /// 1 rev = 2 × 2 = 4 m of travel, chosen so the arithmetic is checkable by hand.
    private let config = CalculateMetrics.Config(gearRatio: 2, wheelCircumferenceMeters: 2)
    private let start = Date(timeIntervalSince1970: 1_000_000)

    private func point(_ secondsIn: TimeInterval, revs: UInt16,
                       uptimeMs: UInt32? = nil, evt: UInt16? = nil) -> TimestampedPoint {
        // uptime defaults to align with the wall-clock second, so cases that don't probe the
        // uptime path behave as a plain timeline.
        TimestampedPoint(date: start.addingTimeInterval(secondsIn),
                         uptimeMs: uptimeMs ?? UInt32(secondsIn * 1000),
                         coordinate: nil, cumulativeCrankRevs: revs, crankEventTime: evt)
    }

    @Test func distanceSumsForwardRevDeltas() {
        let points = [point(0, revs: 0), point(1, revs: 10), point(2, revs: 20)]
        // 20 revs × 4 m = 80 m
        #expect(CalculateMetrics.cadenceDistance(points: points, config: config) == 80)
    }

    @Test func distanceCountsCounterWrapWithinCloseInterval() {
        // The u16 counter wraps (65000 → 100 = 636 revs forward). Points 1 s apart are
        // provably close (uptime gap < 60 s), so the wrapping delta is trusted: 636 × 4 m.
        let points = [point(0, revs: 65000), point(1, revs: 100)]
        #expect(CalculateMetrics.cadenceDistance(points: points, config: config) == Double(636 * 4))
    }

    @Test func distanceIgnoresSensorResets() {
        // A battery pull restarts the counter near zero; the wrapping delta is huge (≥ 1000),
        // which can't be real pedaling within a close interval → contributes nothing.
        let points = [point(0, revs: 500), point(1, revs: 0)]
        #expect(CalculateMetrics.cadenceDistance(points: points, config: config) == 0)
    }

    @Test func distanceDropsBackwardDeltasAcrossBootBoundaries() {
        // With no valid uptime bound (device rebooted between the points), a wrap and a reset
        // are indistinguishable — any non-forward delta is dropped, as before.
        let points = [point(0, revs: 65000, uptimeMs: 5_000_000), point(1, revs: 100, uptimeMs: 1000)]
        #expect(CalculateMetrics.cadenceDistance(points: points, config: config) == 0)
    }

    @Test func distanceIsZeroBelowTwoPoints() {
        #expect(CalculateMetrics.cadenceDistance(points: [point(0, revs: 5)], config: config) == 0)
        #expect(CalculateMetrics.cadenceDistance(points: [], config: config) == 0)
    }

    @Test func samplesStartAtRestThenComputeSpeed() {
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(2),
            points: [point(0, revs: 0), point(1, revs: 10), point(2, revs: 10)]
        )
        let samples = CalculateMetrics.samples(ride: ride, config: config)
        #expect(samples.count == 3)
        // Index 0 is the ride start — no motion yet.
        #expect(samples[0].speedKph == 0)
        #expect(samples[0].cadenceRpm == 0)
        // 10 revs in 1 s → 600 rpm; 40 m in 1 s → 144 km/h with this config.
        #expect(samples[1].cadenceRpm == 600)
        #expect(abs(samples[1].speedKph - 144) < 0.0001)
        // Coasting interval (no rev advance) reads as 0.
        #expect(samples[2].speedKph == 0)
        #expect(samples[2].cadenceRpm == 0)
    }

    @Test func samplesUseCrankEventTimeDeltaWhenPresent() {
        // 10 revs, sensor event time advances 512 ticks = 0.5 s — so cadence is 10/0.5 s = 1200 rpm,
        // NOT the 300 rpm the 2 s wall-clock gap would give. Proves event time wins over timestamps.
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(2),
            points: [point(0, revs: 0, evt: 0), point(2, revs: 10, evt: 512)]
        )
        let samples = CalculateMetrics.samples(ride: ride, config: config)
        #expect(samples[1].cadenceRpm == 1200)
        #expect(abs(samples[1].speedKph - 288) < 0.0001) // 40 m / 0.5 s = 80 m/s = 288 km/h
    }

    @Test func crankEventTimeWrapIsHandled() {
        // Counter wraps the u16 (65000 → 200): wrapping delta = 736 ticks = 0.71875 s.
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(2),
            points: [point(0, revs: 0, evt: 65000), point(2, revs: 10, evt: 200)]
        )
        let samples = CalculateMetrics.samples(ride: ride, config: config)
        #expect(abs(samples[1].cadenceRpm - (10.0 / (736.0 / 1024.0) * 60.0)) < 0.0001)
    }

    @Test func usesUptimeDeltaNotWallClockWhenNoEventTime() {
        // No event time. Wall-clock gap is 10 s, but the raw uptime gap is 2 s — cadence must
        // come from uptime (10/2 s = 300 rpm), not the date (which would give 60 rpm).
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(10),
            points: [point(0, revs: 0, uptimeMs: 0), point(10, revs: 10, uptimeMs: 2000)]
        )
        #expect(CalculateMetrics.samples(ride: ride, config: config)[1].cadenceRpm == 300)
    }

    @Test func fallsBackToWallClockWhenUptimeDeltaImplausible() {
        // Reboot mid-stream: uptime resets, so its delta wraps to an implausible value and is
        // rejected → wall-clock (3 s → 200 rpm). Same path covers a >64 s in-ride pause.
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(3),
            points: [point(0, revs: 0, uptimeMs: 5_000_000), point(3, revs: 10, uptimeMs: 1000)]
        )
        #expect(abs(CalculateMetrics.samples(ride: ride, config: config)[1].cadenceRpm - 200) < 0.0001)
    }

    @Test func computeAveragesCountOnlyMovingIntervals() {
        let ride = Ride(
            startDate: start,
            endDate: start.addingTimeInterval(2),
            points: [point(0, revs: 0), point(1, revs: 10), point(2, revs: 10)]
        )
        let metrics = CalculateMetrics.compute(ride: ride, config: config)
        #expect(metrics.totalDistanceMeters == 40) // one 10-rev interval × 4 m
        // The coast (0 rpm) is excluded from averages but still seen by the maxes.
        #expect(metrics.averageCadenceRpm == 600)
        #expect(metrics.maxCadenceRpm == 600)
        #expect(abs(metrics.averageSpeedKph - 144) < 0.0001)
        #expect(abs(metrics.maxSpeedKph - 144) < 0.0001)
    }
}
