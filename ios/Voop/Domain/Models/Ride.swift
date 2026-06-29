import CoreLocation
import Foundation

struct Ride: Identifiable, Hashable {
    /// Stable identity is the start date, *not* a UUID: `DetectRides` re-runs on every incoming
    /// point and would mint a fresh UUID each time, so a UUID id churns the SwiftUI list (new
    /// identity per tick → full rebuild, lost row state). A segment's start date stays put as
    /// points append and changes only when a new segment begins.
    var id: Date {
        startDate
    }

    let startDate: Date
    let endDate: Date
    let points: [TimestampedPoint]

    var duration: TimeInterval {
        endDate.timeIntervalSince(startDate)
    }

    /// "9:41 AM – 10:23 AM" — the ride's clock span, for compact row and summary headers.
    var clockRange: String {
        let from = startDate.formatted(date: .omitted, time: .shortened)
        let to = endDate.formatted(date: .omitted, time: .shortened)
        return "\(from) – \(to)"
    }

    static func == (lhs: Ride, rhs: Ride) -> Bool {
        lhs.startDate == rhs.startDate
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(startDate)
    }
}

struct TimestampedPoint {
    let date: Date
    let coordinate: CLLocationCoordinate2D?
    let cumulativeCrankRevs: UInt16
}
