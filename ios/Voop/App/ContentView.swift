import SwiftUI

struct ContentView: View {
    @Environment(AppModel.self) private var appModel

    var body: some View {
        // Ingestion and the activity heartbeat are app-lifetime tasks owned by AppModel —
        // deliberately not a `.task` here, which dies with the scene.
        Group {
            if appModel.isDevicePaired {
                RootTabView()
            } else {
                SetupView()
            }
        }
        // Notification permission for the ride-end notification, asked only once there's
        // something to notify about (a paired device). `.task` runs on scene appear — i.e. a
        // real foreground where the prompt can show — and re-runs when pairing completes.
        .task(id: appModel.isDevicePaired) {
            guard appModel.isDevicePaired else { return }
            await appModel.endNotifier.ensureAuthorization()
        }
    }
}
