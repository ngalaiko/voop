import SwiftUI

@main
struct VoopApp: App {
    @State private var appModel: AppModel

    init() {
        let model = AppModel()
        #if DEBUG
            let args = ProcessInfo.processInfo.arguments
            if args.contains("--demo-riding") {
                model.loadDemoData(riding: true)
            } else if args.contains("--demo") {
                model.loadDemoData(riding: false)
            }
        #endif
        _appModel = State(initialValue: model)
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(appModel)
                .environment(appModel.settings)
        }
    }
}
