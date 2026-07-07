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
    }
}
