import ActivityKit
import Foundation

/// Data contract for the ride Live Activity. Shared between the app (which drives updates)
/// and the widget extension (which renders it), so it must stay free of `VoopProtocol` and
/// `CLLocation` types — primitives only, to keep `ContentState` small and trivially Codable.
struct RideActivityAttributes: ActivityAttributes {
    /// Set once when the activity starts; also the ride's stable identity (see
    /// `RideActivityController` — `Ride.id` is regenerated on every re-detection).
    let startDate: Date

    struct ContentState: Codable, Hashable {
        var distanceMeters: Double
        var currentSpeedKph: Double
        var currentCadenceRpm: Int
        /// Drives `Text(timerInterval:)`. While live the upper bound is far in the future so
        /// the clock counts up smoothly on-device; on end it collapses to the true duration.
        var elapsedInterval: ClosedRange<Date>
        /// The ride's last data point as of this update. When the activity goes stale (app
        /// killed before it could end it), the widget freezes the timer at this moment instead
        /// of counting up forever; `end()` uses it to freeze adopted activities correctly.
        var lastPointDate: Date
        var isFinished: Bool
    }
}
