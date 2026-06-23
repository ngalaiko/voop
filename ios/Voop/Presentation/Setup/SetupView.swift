import SwiftUI

struct SetupView: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        VStack(spacing: 32) {
            Spacer()

            Image(systemName: "bicycle.circle.fill")
                .resizable()
                .scaledToFit()
                .frame(width: 96, height: 96)
                .foregroundStyle(.blue)

            VStack(spacing: 8) {
                Text("Set Up Voop")
                    .font(.largeTitle.bold())

                Text("Turn on your Voop device and bring it close to your iPhone.")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
            }

            statusView

            Spacer()
        }
        .onChange(of: appModel.ble.connectionState) { _, state in
            if case .connected = state {
                appModel.markDevicePaired()
            }
        }
    }

    @ViewBuilder
    private var statusView: some View {
        switch appModel.ble.connectionState {
        case .scanning:
            HStack(spacing: 8) {
                ProgressView()
                Text("Searching for device…")
                    .foregroundStyle(.secondary)
            }
        case .connecting:
            HStack(spacing: 8) {
                ProgressView()
                Text("Connecting…")
                    .foregroundStyle(.secondary)
            }
        case .connected:
            Label("Connected", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
        default:
            Text("Waiting for Bluetooth…")
                .foregroundStyle(.secondary)
        }
    }
}
