import Foundation
import Security

/// Where `tenx web` is and the token that signs in to it: what the address
/// `tenx web` prints carries (`https://mac.tailnet.ts.net/?token=…`).
struct Server: Equatable {
    var base: URL
    var token: String

    enum ParseError: LocalizedError {
        case notAnAddress, notHTTPS, noToken

        var errorDescription: String? {
            switch self {
            case .notAnAddress: "That isn't a web address."
            case .notHTTPS: "The address must start with https:// (put tenx web behind tailscale serve). Only localhost may use http://."
            case .noToken: "The address has no ?token=. Paste the whole address tenx web printed."
            }
        }
    }

    /// The pasted address, checked. HTTPS only — the microphone and the page's
    /// own clipboard need it — except on this machine, for the simulator.
    static func parse(_ text: String) throws -> Server {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var parts = URLComponents(string: trimmed), let scheme = parts.scheme?.lowercased(), let host = parts.host, !host.isEmpty else {
            throw ParseError.notAnAddress
        }
        let local = host == "localhost" || host == "127.0.0.1"
        guard scheme == "https" || (scheme == "http" && local) else { throw ParseError.notHTTPS }
        guard let token = parts.queryItems?.first(where: { $0.name == "token" })?.value, !token.isEmpty else {
            throw ParseError.noToken
        }
        parts.query = nil
        parts.fragment = nil
        if parts.path.isEmpty { parts.path = "/" }
        guard let base = parts.url else { throw ParseError.notAnAddress }
        return Server(base: base, token: token)
    }

    /// The page with the token: the server swaps it for its cookie and
    /// redirects to the address without it. Every launch signs in this way,
    /// so a lost cookie never matters.
    var signInURL: URL {
        var parts = URLComponents(url: base, resolvingAgainstBaseURL: false)!
        parts.queryItems = [URLQueryItem(name: "token", value: token)]
        return parts.url!
    }

    /// A URL on this server (same scheme, host and port): it stays in the app.
    func owns(_ url: URL) -> Bool {
        url.scheme?.lowercased() == base.scheme?.lowercased() && url.host?.lowercased() == base.host?.lowercased() && url.port == base.port
    }
}

/// The server in the Keychain, on this device only.
enum ServerStore {
    private static let service = "it.luedeke.mobile.tenx.server"

    static func load() -> Server? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnAttributes as String: true,
            kSecReturnData as String: true,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let found = item as? [String: Any],
              let account = found[kSecAttrAccount as String] as? String,
              let base = URL(string: account),
              let data = found[kSecValueData as String] as? Data,
              let token = String(data: data, encoding: .utf8)
        else { return nil }
        return Server(base: base, token: token)
    }

    static func save(_ server: Server) {
        clear()
        let item: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: server.base.absoluteString,
            kSecValueData as String: Data(server.token.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        SecItemAdd(item as CFDictionary, nil)
    }

    static func clear() {
        SecItemDelete([kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service] as CFDictionary)
    }
}
