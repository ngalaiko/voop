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
        // Notification permission for the ride-end notification and Health write access for
        // workout sync, asked only once there's something to record (a paired device). `.task`
        // runs on scene appear — i.e. a real foreground where prompts can show — and re-runs
        // when pairing completes. Sequential: the second sheet appears once the first is decided.
        .task(id: appModel.isDevicePaired) {
            guard appModel.isDevicePaired else { return }
            await appModel.endNotifier.ensureAuthorization()
            await appModel.health.ensureAuthorization()
        }
    }
}
