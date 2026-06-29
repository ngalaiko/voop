import SwiftUI

/// Settings — a stock `Form` of `Picker`/`Stepper`/`LabeledContent`/`ShareLink` rows.
struct SettingsView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(AppSettings.self) private var settings
    @State private var exportURL: URL?

    var body: some View {
        @Bindable var settings = settings
        Form {
            Section("Wheel") {
                Picker("Wheel Size", selection: $settings.rimBsdMillimeters) {
                    ForEach(AppSettings.rimPresets, id: \.bsd) { rim in
                        Text("\(rim.label) (\(rim.bsd) mm)").tag(rim.bsd)
                    }
                }
                Stepper("Tire Width: \(settings.tireWidthMillimeters) mm",
                        value: $settings.tireWidthMillimeters, in: 18 ... 60)
                LabeledContent(
                    "Circumference",
                    value: "\(settings.wheelCircumferenceMeters.formatted(.number.precision(.fractionLength(3)))) m"
                )
            }

            Section("Gear") {
                Stepper("Chainring: \(settings.chainringTeeth)t",
                        value: $settings.chainringTeeth, in: 20 ... 60)
                Stepper("Cog: \(settings.cogTeeth)t",
                        value: $settings.cogTeeth, in: 9 ... 30)
                LabeledContent("Ratio", value: settings.gearRatio.formatted(.number.precision(.fractionLength(2))))
                HStack {
                    ForEach(AppSettings.gearPresets, id: \.label) { preset in
                        let active = settings.chainringTeeth == preset.chainring && settings.cogTeeth == preset.cog
                        Button(preset.label) {
                            settings.chainringTeeth = preset.chainring
                            settings.cogTeeth = preset.cog
                        }
                        .buttonStyle(.bordered)
                        .tint(active ? .orange : .gray)
                        .controlSize(.small)
                    }
                }
            }

            Section {
                Stepper("Min Cadence: \(settings.minCadenceRpm) rpm",
                        value: $settings.minCadenceRpm, in: 5 ... 60, step: 5)
                Stepper("Min Distance: \(VoopFormat.distance(Double(settings.minDistanceMeters)))",
                        value: $settings.minDistanceMeters, in: 0 ... 5000, step: 100)
                Stepper("Stop Pause: \(settings.gapThresholdSeconds / 60) min",
                        value: $settings.gapThresholdSeconds, in: 60 ... 1800, step: 60)
            } header: {
                Text("Ride Detection")
            } footer: {
                Text("Filters out walking or short rolls. A pause longer than the threshold ends a ride.")
            }

            Section("Data") {
                if let exportURL {
                    ShareLink("Export Raw Data (CSV)", item: exportURL)
                } else {
                    Text("Export Raw Data (CSV)").foregroundStyle(.secondary)
                }
            }

            Section {} footer: {
                Text("Voop v\(appVersion)")
                    .frame(maxWidth: .infinity, alignment: .center)
            }
        }
        .navigationTitle("Settings")
        .task { exportURL = try? appModel.writeCSVExport() }
    }

    private var appVersion: String {
        let short = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0"
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "1"
        return "\(short) (\(build))"
    }
}
