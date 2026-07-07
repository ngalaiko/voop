import SwiftUI

/// The Dashboard — the app's single content tab, a stock grouped `List`: a live status section
/// (refreshed each second), a device section, then the ride history grouped by day.
struct MainView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let groups = dayGroups()

        List {
            Section("Status") {
                TimelineView(.periodic(from: .now, by: 1)) { ctx in
                    liveStatus(at: ctx.date)
                }
            }

            Section("Device") {
                LabeledContent("Voop Device") { deviceValue }
                LabeledContent("Cadence Sensor") { sensorValue }
            }

            if groups.isEmpty {
                Section("Rides") {
                    Text("No rides yet").foregroundStyle(.secondary)
                }
            } else {
                ForEach(groups) { group in
                    Section {
                        ForEach(group.rides) { ride in
                            NavigationLink(value: ride) { rideRow(ride) }
                                .swipeActions(edge: .trailing) {
                                    Button("Delete", role: .destructive) { appModel.deleteRide(ride) }
                                }
                        }
                    } header: {
                        HStack {
                            Text(dayLabel(group.day))
                            Spacer()
                            Text("\(VoopFormat.kilometers(group.totalMeters, fractionDigits: 1)) km")
                        }
                    }
                }
            }
        }
        .navigationTitle("Voop")
        .navigationDestination(for: Ride.self) { ride in
            RideDetailView(ride: ride)
        }
    }

    // MARK: - Live status

    @ViewBuilder
    private func liveStatus(at now: Date) -> some View {
        if let ride = appModel.ongoingRide(at: now) {
            let rpm = appModel.liveRpm(at: now)
            let speed = Double(rpm) / 60.0 * config.gearRatio * config.wheelCircumferenceMeters * 3.6
            let dist = CalculateMetrics.cadenceDistance(points: ride.points, config: config)
            VStack(alignment: .leading, spacing: 8) {
                Label("Riding", systemImage: "figure.outdoor.cycle")
                    .font(.headline)
                    .foregroundStyle(.orange)
                HStack(spacing: 20) {
                    metric(String(format: "%.1f", speed), "km/h")
                    metric("\(rpm)", "rpm")
                    metric(VoopFormat.kilometers(dist), "km")
                }
                Text("Elapsed \(elapsed(since: ride.startDate, now: now))")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            .padding(.vertical, 4)
        } else {
            Label("Ready to ride", systemImage: "bicycle")
        }
    }

    private func metric(_ value: String, _ unit: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 3) {
            Text(value).font(.title3.monospacedDigit())
            Text(unit).font(.caption).foregroundStyle(.secondary)
        }
    }

    private func elapsed(since start: Date, now: Date) -> String {
        Duration.seconds(now.timeIntervalSince(start))
            .formatted(.time(pattern: .hourMinuteSecond(padHourToLength: 2)))
    }

    // MARK: - Device status

    private func connectedValue(percent: Int?) -> Text {
        let connected = Text("Connected").foregroundStyle(.green)
        guard let percent else { return connected }
        return connected + Text(" · \(percent)%").foregroundStyle(.secondary)
    }

    private var deviceValue: Text {
        switch appModel.ble.connectionState {
        case .connected where appModel.ble.protocolMismatch:
            // The firmware speaks a different protocol revision: it looks connected but every
            // point it sends is being dropped (and popped from its buffer). Say so.
            Text("Update needed").foregroundStyle(.red)
        case .connected:
            connectedValue(percent: appModel.ble.deviceStatus.map { Int($0.mcuBattery.percent) })
        case .scanning, .connecting:
            Text("Searching…").foregroundStyle(.secondary)
        default:
            Text("Offline").foregroundStyle(.secondary)
        }
    }

    private var sensorValue: Text {
        guard case .connected = appModel.ble.connectionState else {
            return Text("Offline").foregroundStyle(.secondary)
        }
        // `isCadenceLive`, not a `Date.now` window check: stored observable state is what
        // makes this row go back to "Searching…" when the stream falls silent (same
        // mechanism as `dayGroups`'s ongoing-ride exclusion).
        let live = appModel.ble.deviceStatus?.sensorConnected == true || appModel.isCadenceLive
        guard live else { return Text("Searching…").foregroundStyle(.secondary) }
        return connectedValue(percent: appModel.ble.deviceStatus?.sensorBattery.map(Int.init))
    }

    // MARK: - Ride rows

    private func rideRow(_ ride: Ride) -> some View {
        LabeledContent {
            VStack(alignment: .trailing, spacing: 2) {
                Text(distance(ride))
                Text(avgCadence(ride)).font(.subheadline).foregroundStyle(.secondary)
            }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(startTime(ride))
                Text(durationLabel(ride)).font(.subheadline).foregroundStyle(.secondary)
            }
        }
    }

    private func dayLabel(_ day: Date) -> String {
        let cal = Calendar.current
        if cal.isDateInToday(day) { return "Today" }
        if cal.isDateInYesterday(day) { return "Yesterday" }
        return day.formatted(.dateTime.month(.abbreviated).day())
    }

    private func startTime(_ ride: Ride) -> String {
        ride.startDate.formatted(
            .dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)
                .locale(Locale(identifier: "en_GB"))
        )
    }

    private func durationLabel(_ ride: Ride) -> String {
        let minutes = Int(ride.duration) / 60
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes) min"
    }

    private func distance(_ ride: Ride) -> String {
        VoopFormat.distance(distanceMeters(ride))
    }

    private func avgCadence(_ ride: Ride) -> String {
        "avg \(Int(appModel.metrics(for: ride).averageCadenceRpm)) rpm"
    }

    // MARK: - Data

    private var config: CalculateMetrics.Config {
        .init(gearRatio: settings.gearRatio, wheelCircumferenceMeters: settings.wheelCircumferenceMeters)
    }

    private func distanceMeters(_ ride: Ride) -> Double {
        appModel.metrics(for: ride).totalDistanceMeters
    }

    private func dayGroups() -> [DayGroup] {
        // The stored (heartbeat-driven) id, NOT `ongoingRide()?.id`: a ride ends by time
        // passing with no data mutation, so a `Date.now` check here would never re-render —
        // the finished ride stayed out of the list until something unrelated invalidated it.
        let ongoingID = appModel.ongoingRideID
        let rides = appModel.detectedRides
            .filter { $0.id != ongoingID && DetectRides.qualifies($0, settings: settings) }
            .sorted { $0.startDate > $1.startDate }

        let calendar = Calendar.current
        let buckets = Dictionary(grouping: rides) { calendar.startOfDay(for: $0.startDate) }
        return buckets.keys.sorted(by: >).map { day in
            let dayRides = buckets[day]!.sorted { $0.startDate > $1.startDate }
            let total = dayRides.reduce(0.0) { $0 + distanceMeters($1) }
            return DayGroup(day: day, rides: dayRides, totalMeters: total)
        }
    }
}

struct DayGroup: Identifiable {
    let day: Date
    let rides: [Ride]
    let totalMeters: Double
    var id: Date {
        day
    }
}
