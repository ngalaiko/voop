import SwiftUI

enum AppTab: Hashable {
    case dashboard, settings
}

/// The app shell once a device is paired: a standard SwiftUI `TabView` (the iOS 26 Liquid Glass
/// floating tab bar) with two destinations. Using the system bar means its selection highlight
/// is managed by the platform — tinted with the app's orange accent.
struct RootTabView: View {
    @State private var tab: AppTab
    @State private var dashboardPath: [Ride]

    init() {
        var startTab = AppTab.dashboard
        var path: [Ride] = []
        #if DEBUG
            let args = ProcessInfo.processInfo.arguments
            if args.contains("--start-settings") {
                startTab = .settings
            } else if args.contains("--open-ride") {
                path = Array(DemoData.rides.suffix(1))
            }
        #endif
        _tab = State(initialValue: startTab)
        _dashboardPath = State(initialValue: path)
    }

    var body: some View {
        TabView(selection: $tab) {
            Tab("Dashboard", systemImage: "gauge.with.dots.needle.bottom.50percent", value: AppTab.dashboard) {
                NavigationStack(path: $dashboardPath) { MainView() }
            }
            Tab("Settings", systemImage: "gearshape", value: AppTab.settings) {
                NavigationStack { SettingsView() }
            }
        }
        .tint(.orange)
    }
}
