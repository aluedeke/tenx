import SwiftUI

@main
struct TenXApp: App {
    var body: some Scene {
        WindowGroup { RootView() }
            .commands { ReloadCommand() }
    }
}

/// Window → Reload Page (⌘R) in the iPad's menu bar: a new web view on the
/// page, so a newer `tenx web` page shows without quitting the app.
struct ReloadCommand: Commands {
    @FocusedValue(\.reloadPage) private var reload

    var body: some Commands {
        CommandGroup(after: .windowArrangement) {
            Button("Reload Page") { reload?.run() }
                .keyboardShortcut("r")
                .disabled(reload == nil)
        }
    }
}

extension FocusedValues {
    /// The focused window's page, reloaded (`RootView.reload`).
    @Entry var reloadPage: PageReload?
}

/// `RootView.reload`, compared by the page it reloads: a closure alone
/// can't be, and would update the menu on every change of the view.
struct PageReload: Equatable {
    let generation: Int
    let run: () -> Void

    static func == (a: Self, b: Self) -> Bool { a.generation == b.generation }
}

extension UIColor {
    /// The page's background (web/src/palette.ts GROUND), shown before it loads.
    static let ground = UIColor(red: 0x17 / 255, green: 0x18 / 255, blue: 0x20 / 255, alpha: 1)
}

/// Settings until there is a server; then the page, with settings over it
/// when asked for (two-finger hold, ⌘, or the page's menu) or when the page
/// can't be shown.
struct RootView: View {
    @State private var server = ServerStore.load()
    @State private var settings = false
    @State private var problem: String?
    /// A new value reloads the page (a new web view).
    @State private var generation = 0

    var body: some View {
        Group {
            if let server {
                WebPage(server: server, onProblem: { problem = $0; settings = true }, onSettings: { problem = nil; settings = true })
                    .id(generation)
                    .focusedSceneValue(\.reloadPage, PageReload(generation: generation, run: reload))
                    .ignoresSafeArea()
                    .sheet(isPresented: $settings) {
                        SettingsView(current: server, problem: problem, onSave: save, onReload: reload, onSignOut: signOut)
                    }
            } else {
                SettingsView(current: nil, problem: nil, onSave: save, onReload: {}, onSignOut: {})
            }
        }
        .background(Color(uiColor: .ground))
        .preferredColorScheme(.dark)
    }

    private func save(_ new: Server) {
        ServerStore.save(new)
        server = new
        reload()
    }

    private func reload() {
        problem = nil
        settings = false
        generation += 1
    }

    private func signOut() {
        ServerStore.clear()
        WKWebsiteDataStoreReset.all()
        settings = false
        server = nil
    }
}

struct WebPage: UIViewControllerRepresentable {
    let server: Server
    let onProblem: (String) -> Void
    let onSettings: () -> Void

    func makeUIViewController(context: Context) -> WebViewController {
        let vc = WebViewController(server: server)
        vc.onProblem = onProblem
        vc.onSettings = onSettings
        return vc
    }

    func updateUIViewController(_ vc: WebViewController, context: Context) {
        vc.onProblem = onProblem
        vc.onSettings = onSettings
    }
}
