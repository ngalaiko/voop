import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        Group {
            if appModel.isDevicePaired {
                TabView {
                    Tab("Status", systemImage: "antenna.radiowaves.left.and.right") {
                        NavigationStack { StatusView() }
                    }
                    Tab("Rides", systemImage: "bicycle") {
                        NavigationStack { RideListView() }
                    }
                    Tab("Pair Sensor", systemImage: "sensor.tag.radiowaves.forward") {
                        NavigationStack { PairingView() }
                    }
                }
            } else {
                SetupView()
            }
        }
        .task {
            await appModel.startReceiving()
        }
    }
}
