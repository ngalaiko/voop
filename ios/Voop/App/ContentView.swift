import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        Group {
            if appModel.isDevicePaired {
                RootTabView()
            } else {
                SetupView()
            }
        }
        .task {
            async let receive: Void = appModel.startReceiving()
            async let heartbeat: Void = appModel.runActivityHeartbeat()
            _ = await (receive, heartbeat)
        }
    }
}
