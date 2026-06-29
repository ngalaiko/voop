import Charts
import MapKit
import SwiftUI

/// A completed ride — a stock grouped `List`: route map, summary + stat rows (`LabeledContent`),
/// a Swift Charts speed graph, and a destructive delete row.
struct RideDetailView: View {
    let ride: Ride
    @Environment(AppModel.self) private var appModel
    @Environment(AppSettings.self) private var settings
    @Environment(\.dismiss) private var dismiss
    @State private var showDeleteConfirm = false
    @State private var selectedElapsed: Double?

    private var config: CalculateMetrics.Config {
        .init(gearRatio: settings.gearRatio, wheelCircumferenceMeters: settings.wheelCircumferenceMeters)
    }

    var body: some View {
        let samples = CalculateMetrics.samples(ride: ride, config: config)
        let metrics = CalculateMetrics.compute(ride: ride, config: config)
        let coordinates = samples.compactMap(\.coordinate)
        let speedPoints = Self.speedPoints(from: samples)
        let selected = selectedElapsed.flatMap { elapsed in
            samples.min(by: { abs($0.elapsed - elapsed) < abs($1.elapsed - elapsed) })
        }

        List {
            Section("Summary") {
                LabeledContent("Time", value: ride.clockRange)
                LabeledContent("Distance", value: VoopFormat.distance(metrics.totalDistanceMeters))
                LabeledContent("Duration",
                               value: Duration.seconds(ride.duration).formatted(.time(pattern: .hourMinute)))
            }

            Section("Speed & Cadence") {
                LabeledContent("Avg Speed", value: "\(Int(metrics.averageSpeedKph)) km/h")
                LabeledContent("Max Speed", value: "\(Int(metrics.maxSpeedKph)) km/h")
                LabeledContent("Avg Cadence", value: "\(Int(metrics.averageCadenceRpm)) rpm")
                LabeledContent("Max Cadence", value: "\(Int(metrics.maxCadenceRpm)) rpm")
            }

            if coordinates.count >= 2 {
                Section {
                    Map {
                        MapPolyline(coordinates: coordinates).stroke(.orange, lineWidth: 4)
                        if let coordinate = selected?.coordinate {
                            Annotation("", coordinate: coordinate) {
                                Circle().fill(.orange)
                                    .frame(width: 12, height: 12)
                                    .overlay(Circle().stroke(.white, lineWidth: 2))
                            }
                        }
                    }
                    .frame(height: 280)
                    .listRowInsets(EdgeInsets())
                }
            }

            if speedPoints.count >= 2 {
                Section("Speed") {
                    Chart {
                        ForEach(speedPoints) { point in
                            AreaMark(x: .value("Time", point.elapsed), y: .value("Speed", point.kph))
                                .foregroundStyle(.orange.opacity(0.2))
                                .interpolationMethod(.catmullRom)
                            LineMark(x: .value("Time", point.elapsed), y: .value("Speed", point.kph))
                                .foregroundStyle(.orange)
                                .interpolationMethod(.catmullRom)
                        }
                        if let selected {
                            RuleMark(x: .value("Time", selected.elapsed))
                                .foregroundStyle(.secondary)
                        }
                    }
                    .chartXSelection(value: $selectedElapsed)
                    .chartXAxis {
                        AxisMarks { value in
                            AxisGridLine()
                            AxisValueLabel {
                                if let seconds = value.as(Double.self) {
                                    Text(Duration.seconds(seconds).formatted(.time(pattern: .minuteSecond)))
                                }
                            }
                        }
                    }
                    .frame(height: 180)
                }
            }

            Section {
                Button("Delete Ride", role: .destructive) { showDeleteConfirm = true }
                    .frame(maxWidth: .infinity)
            }
        }
        .navigationTitle(ride.startDate.formatted(date: .abbreviated, time: .omitted))
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog("Delete this ride?", isPresented: $showDeleteConfirm, titleVisibility: .visible) {
            Button("Delete Ride", role: .destructive) {
                appModel.deleteRide(ride)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This permanently removes the ride's recorded points.")
        }
    }

    /// Chart points with speed run through a centered moving average — cadence-derived speed
    /// quantizes hard at low cadence, so smoothing recovers the trend the rider felt.
    private static func speedPoints(from samples: [RideSample], window: Int = 11) -> [SpeedPoint] {
        let speeds = samples.map(\.speedKph)
        let half = max(window / 2, 1)
        return samples.indices.map { i in
            let lo = max(0, i - half)
            let hi = min(speeds.count - 1, i + half)
            let mean = speeds[lo ... hi].reduce(0, +) / Double(hi - lo + 1)
            return SpeedPoint(id: i, elapsed: samples[i].elapsed, kph: mean)
        }
    }
}

struct SpeedPoint: Identifiable {
    let id: Int
    let elapsed: TimeInterval
    let kph: Double
}
