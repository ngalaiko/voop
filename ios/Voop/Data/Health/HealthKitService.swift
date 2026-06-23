import Foundation

@MainActor
final class HealthKitService {
    func requestAuthorization() async throws {}

    func save(ride _: Ride, metrics _: RideMetrics) async throws {}
}
