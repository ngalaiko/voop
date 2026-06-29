import SwiftUI

/// Onboarding / pairing — a stock centered layout with a status line driven by the BLE state.
struct SetupView: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "bicycle.circle.fill")
                .font(.system(size: 80))
                .foregroundStyle(.orange)

            Text("Set Up Voop")
                .font(.title.bold())

            Text("Turn on your Voop device and hold it near your iPhone.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            statusView
                .padding(.top, 8)
        }
        .padding()
        .onChange(of: appModel.ble.connectionState) { _, state in
            if case .connected = state { appModel.markDevicePaired() }
        }
    }

    @ViewBuilder
    private var statusView: some View {
        switch appModel.ble.connectionState {
        case .scanning:
            HStack { ProgressView(); Text("Searching for device…") }
                .foregroundStyle(.secondary)
        case .connecting:
            HStack { ProgressView(); Text("Connecting…") }
                .foregroundStyle(.secondary)
        case .connected:
            Label("Connected", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
        default:
            Text("Waiting for Bluetooth…")
                .foregroundStyle(.secondary)
        }
    }
}
