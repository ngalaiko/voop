import ActivityKit
import Foundation

/// Owns the lifecycle of the ride Live Activity. The app calls `reconcile` on each tick with
/// the currently ongoing ride (if any) and the content to show; this class decides whether to
/// start, update, or end the activity.
///
/// Identity is keyed on the ride's start date, not `Ride.id`: `DetectRides.detect` mints a fresh
/// `UUID` on every call and re-detection runs on every incoming point, so the id is useless here.
/// A ride's start date stays stable as points append to the same segment and changes only when a
/// new segment begins.
@MainActor
final class RideActivityController {
    private var activity: Activity<RideActivityAttributes>?
    private var activeStartDate: Date?
    private var activeEndDate: Date?

    /// Re-attach to an activity left running from a previous app session so the next `reconcile`
    /// can resume or end it instead of orphaning it. Call once at startup.
    func adoptExisting() {
        guard activity == nil, let existing = Activity<RideActivityAttributes>.activities.first else { return }
        activity = existing
        activeStartDate = existing.attributes.startDate
        activeEndDate = nil
    }

    /// - Parameters:
    ///   - rideStartDate: start of the ongoing ride, or `nil` when no ride is in progress.
    ///   - rideEndDate: end of the ongoing ride (its last point); used to freeze the timer on end.
    ///   - staleDate: when the system should mark the activity stale if no further update arrives,
    ///     so a stranded activity (app killed before the heartbeat ends it) stops its live timer.
    ///   - state: content to display while a ride is ongoing.
    func reconcile(
        rideStartDate: Date?,
        rideEndDate: Date?,
        staleDate: Date?,
        state: RideActivityAttributes.ContentState?
    ) async {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else {
            await end()
            return
        }
        guard let rideStartDate, let rideEndDate, let state else {
            await end()
            return
        }

        if let current = activity, activeStartDate == rideStartDate {
            activeEndDate = rideEndDate
            // `Activity` isn't Sendable; it's only ever touched on the main actor here, so the
            // unsafe escape hatch lets us await its nonisolated `update` under strict concurrency.
            nonisolated(unsafe) let act = current
            await act.update(ActivityContent(state: state, staleDate: staleDate))
        } else {
            // Nothing running, or a different ride is — end the old one and start fresh.
            await end()
            start(rideStartDate: rideStartDate, rideEndDate: rideEndDate, staleDate: staleDate, state: state)
        }
    }

    /// Ends the active activity, if any, freezing the timer at the true ride duration. Idempotent.
    func end() async {
        guard let current = activity, let startDate = activeStartDate else { return }
        // For an adopted activity that was never updated this session, `activeEndDate` is nil —
        // fall back to the last data point its own content carries, not `startDate`, which
        // would freeze the "Ride ended" card at 0:00.
        let frozenEnd = max(startDate, activeEndDate ?? current.content.state.lastPointDate)
        activity = nil
        activeStartDate = nil
        activeEndDate = nil

        var finalState = current.content.state
        finalState.isFinished = true
        finalState.elapsedInterval = startDate ... frozenEnd
        // See `reconcile`: `Activity` isn't Sendable, but it's main-actor-only here.
        nonisolated(unsafe) let act = current
        await act.end(ActivityContent(state: finalState, staleDate: nil), dismissalPolicy: .default)
    }

    private func start(rideStartDate: Date, rideEndDate: Date, staleDate: Date?, state: RideActivityAttributes.ContentState) {
        let attributes = RideActivityAttributes(startDate: rideStartDate)
        let content = ActivityContent(state: state, staleDate: staleDate)
        do {
            activity = try Activity.request(attributes: attributes, content: content, pushType: nil)
            activeStartDate = rideStartDate
            activeEndDate = rideEndDate
        } catch {
            activity = nil
            activeStartDate = nil
            activeEndDate = nil
        }
    }
}
