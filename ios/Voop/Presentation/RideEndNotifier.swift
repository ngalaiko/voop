import Foundation
import UserNotifications

/// Owns the "ride ended" local notification. The end of a ride is not an observable event —
/// it's the stop-pause gap elapsing, which a suspended or killed app sleeps through — so this
/// never fires on the transition. Instead each heartbeat tick (re)schedules a pending
/// notification for the moment the gap *will have* elapsed, carrying the ride's stats as of
/// its last point. New points push it forward; when the rider actually stops, the last
/// schedule fires on time with the final stats, app running or not. Same shape as the Live
/// Activity's `staleDate`.
@MainActor
final class RideEndNotifier {
    /// Single identifier: `add` replaces the pending request with the same id, so there is
    /// only ever one scheduled ride-end notification.
    private static let requestID = "ride-ended"

    /// Ask for permission once (no-op after the user has decided). Called from the UI on
    /// foreground — never from `AppModel.init`, which also runs on background BLE relaunches
    /// where a permission prompt cannot be shown.
    func ensureAuthorization() async {
        let center = UNUserNotificationCenter.current()
        guard await center.notificationSettings().authorizationStatus == .notDetermined else { return }
        _ = try? await center.requestAuthorization(options: [.alert, .sound])
    }

    /// (Re)schedule the notification to fire at `fireAt` — the moment the ongoing ride's
    /// stop-pause gap elapses — with the ride's stats as of its last point.
    func reschedule(fireAt: Date, distanceMeters: Double, rideDuration: TimeInterval, at now: Date = .now) {
        let interval = fireAt.timeIntervalSince(now)
        guard interval > 0 else { return }
        let content = UNMutableNotificationContent()
        content.title = "Ride ended"
        content.body = "\(VoopFormat.distance(distanceMeters)) in \(Self.durationText(rideDuration))"
        content.sound = .default
        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: interval, repeats: false)
        UNUserNotificationCenter.current().add(
            UNNotificationRequest(identifier: Self.requestID, content: content, trigger: trigger)
        )
    }

    /// Drop the pending notification. Harmless when nothing is pending (a fired notification
    /// no longer is); needed when the ongoing ride disappears for a non-time reason — a
    /// deleted ride, or a stale schedule from a previous app session found at launch.
    func cancelPending() {
        UNUserNotificationCenter.current().removePendingNotificationRequests(withIdentifiers: [Self.requestID])
    }

    /// Matches the dashboard's duration style ("42 min", "1h 5m").
    private static func durationText(_ seconds: TimeInterval) -> String {
        let minutes = Int(seconds) / 60
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes) min"
    }
}
