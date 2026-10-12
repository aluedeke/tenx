import SwiftUI
import WebKit

/// The server's address (what `tenx web` prints, token included), and
/// what's wrong with it when the page couldn't be shown.
struct SettingsView: View {
    let current: Server?
    let problem: String?
    let onSave: (Server) -> Void
    let onReload: () -> Void
    let onSignOut: () -> Void

    @State private var address = ""
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                if let problem {
                    Section { Text(problem).foregroundStyle(.orange) }
                }
                Section {
                    TextField("https://mac.tailnet.ts.net/?token=…", text: $address, axis: .vertical)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(.body, design: .monospaced))
                        .onSubmit(connect)
                    PasteButton(payloadType: String.self) { strings in
                        if let text = strings.first { address = text; connect() }
                    }
                    Button("Connect", action: connect).disabled(address.isEmpty)
                } header: {
                    Text(current == nil ? "Server" : "Change server")
                } footer: {
                    Text(error ?? "Run `tenx web` on your Mac and paste the address it prints, with ?token=. It must be https: put tenx web behind `tailscale serve`.")
                        .foregroundStyle(error == nil ? Color.secondary : Color.red)
                }
                if let current {
                    Section {
                        LabeledContent("Server", value: current.base.host ?? current.base.absoluteString)
                        Button("Reload page", action: onReload)
                        Button("Sign out", role: .destructive, action: onSignOut)
                    } footer: {
                        Text("Hold two fingers on the page for a second, or press ⌘, , to come back here.")
                    }
                }
            }
            .navigationTitle("tenx")
            .toolbar {
                if current != nil {
                    ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                }
            }
        }
    }

    private func connect() {
        do {
            onSave(try Server.parse(address))
            address = ""
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }
}

enum WKWebsiteDataStoreReset {
    /// Cookies and storage of every site: signing out leaves nothing behind.
    @MainActor static func all() {
        let store = WKWebsiteDataStore.default()
        store.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast) {}
    }
}
