#if DEBUG
    import CoreLocation
    import Foundation

    /// Synthetic rides for visually verifying the redesign in the simulator (no real device).
    /// Gated behind launch arguments in `VoopApp`; never compiled into release builds.
    enum DemoData {
        /// A populated multi-day history (oldest → newest), matching the spread in the mockups.
        static var rides: [Ride] {
            let cal = Calendar.current
            let today = cal.startOfDay(for: .now)
            func day(_ offset: Int) -> Date {
                cal.date(byAdding: .day, value: offset, to: today)!
            }
            func at(_ base: Date, _ hour: Int, _ minute: Int) -> Date {
                base.addingTimeInterval(TimeInterval(hour * 3600 + minute * 60))
            }

            return [
                makeRide(start: at(day(-5), 19, 10), minutes: 44, lat: 47.376, lon: 8.541, cadence: 76),
                makeRide(start: at(day(-3), 8, 2), minutes: 33, lat: 47.390, lon: 8.515, cadence: 80),
                makeRide(start: at(day(-3), 12, 5), minutes: 19, lat: 47.366, lon: 8.555, cadence: 70),
                makeRide(start: at(day(-3), 17, 22), minutes: 48, lat: 47.400, lon: 8.530, cadence: 75),
                makeRide(start: at(day(-1), 7, 15), minutes: 41, lat: 47.378, lon: 8.540, cadence: 78),
                makeRide(start: at(day(-1), 18, 30), minutes: 37, lat: 47.384, lon: 8.500, cadence: 82),
                makeRide(start: at(day(0), 7, 14), minutes: 22, lat: 47.372, lon: 8.548, cadence: 81),
            ]
        }

        /// Builds a ride as a 5-second-cadence GPS track with revs accumulating at roughly
        /// `cadence` rpm (plus some wiggle), so distance/speed/cadence all compute realistically.
        static func makeRide(start: Date, minutes: Int, lat: Double, lon: Double, cadence: Double) -> Ride {
            let step = 5.0
            let count = max(2, Int(Double(minutes) * 60 / step))
            var points: [TimestampedPoint] = []
            var revs = 0.0
            for i in 0 ..< count {
                let t = Double(i) * step
                let c = max(0, cadence + 12 * sin(t / 45) + 6 * sin(t / 8))
                if i > 0 { revs += c / 60 * step }
                let coord = CLLocationCoordinate2D(
                    latitude: lat + 0.00016 * Double(i) + 0.0009 * sin(t / 70),
                    longitude: lon + 0.00012 * Double(i) + 0.0011 * sin(t / 38)
                )
                points.append(TimestampedPoint(
                    date: start.addingTimeInterval(t),
                    coordinate: coord,
                    cumulativeCrankRevs: UInt16(clamping: Int(revs))
                ))
            }
            return Ride(
                startDate: start,
                endDate: start.addingTimeInterval(Double(count - 1) * step),
                points: points
            )
        }
    }
#endif
