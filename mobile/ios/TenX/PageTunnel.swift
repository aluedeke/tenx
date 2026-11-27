import Foundation
import Network

/// How a page's `localhost` reaches the Mac (`tenx_core::pagetunnel`): a
/// small HTTP `CONNECT` proxy on the phone's own loopback, which the pages'
/// web views use for `localhost` only (`configuration`). Each connection it
/// gets goes to `tenx web` as a WebSocket, `/pagetunnel?host=&port=` with
/// the page token, on the address the app already uses — so wherever the
/// app reaches `tenx web`, its pages reach the Mac's dev servers too.
///
/// Another app on the phone can reach this loopback port while the app
/// runs, so the proxy asks for a password of its own, made at launch and
/// known only to the web views.
final class PageTunnel: NSObject, URLSessionWebSocketDelegate, @unchecked Sendable {
    private let server: Server
    private let secret = UUID().uuidString
    /// Every field below is touched on this queue only.
    private let queue = DispatchQueue(label: "tenx.pagetunnel")
    private var listener: NWListener?
    /// The port the web views were given: a listener started again (after
    /// the app was in the background) asks for the same one first.
    private var port: NWEndpoint.Port?
    private var started = false
    /// The listener now running reached `.ready` once.
    private var listened = false
    private var waiting: [(NWEndpoint.Port?) -> Void] = []
    /// A socket's answer to its upgrade, by task id.
    private var opening: [Int: (Bool) -> Void] = [:]
    private var session: URLSession!

    /// A request head longer than this is refused unread.
    private static let maxHead = 8 * 1024

    init(server: Server) {
        self.server = server
        super.init()
        session = URLSession(configuration: .ephemeral, delegate: self, delegateQueue: nil)
    }

    /// The proxy for a page's web view and the port it names, once it
    /// listens; `nil` when it can't. A web view made for another port is
    /// made again (`PagePanes.show`).
    func configuration() async -> (proxy: ProxyConfiguration, port: UInt16)? {
        guard let port = await ready() else { return nil }
        var proxy = ProxyConfiguration(httpCONNECTProxy: .hostPort(host: .ipv4(.loopback), port: port), tlsOptions: nil)
        proxy.applyCredential(username: "tenx", password: secret)
        // `localhost` alone, as the page assumes (pages.ts `viaTunnel`):
        // every other site goes its own way.
        proxy.matchDomains = ["localhost"]
        return (proxy, port.rawValue)
    }

    /// The app is back in front: a listener iOS closed meanwhile starts again.
    func wake() {
        queue.async { if self.listener == nil, self.started { self.start() } }
    }

    /// No more pages from this one (the app's view is made anew): the
    /// listener, its tunnels and the session go. The session holds its
    /// delegate, this, until then.
    func close() {
        queue.async {
            self.started = false
            self.listener?.cancel()
            self.listener = nil
            self.session.invalidateAndCancel()
        }
    }

    private func ready() async -> NWEndpoint.Port? {
        await withCheckedContinuation { done in
            queue.async {
                if self.listener != nil, let port = self.port, self.waiting.isEmpty {
                    done.resume(returning: port)
                    return
                }
                self.waiting.append { done.resume(returning: $0) }
                if self.listener == nil { self.start() }
            }
        }
    }

    private func start() {
        started = true
        let pinned = port
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: pinned ?? .any)
        guard let l = try? NWListener(using: params) else {
            port = nil
            return settle(nil)
        }
        listener = l
        listened = false
        l.stateUpdateHandler = { [weak self, weak l] state in
            guard let self, let l, self.listener === l else { return }
            switch state {
            case .ready:
                self.listened = true
                self.port = l.port
                self.settle(l.port)
            // `.waiting` too: it may never end, and a page waits on this.
            case .failed, .waiting, .cancelled:
                l.cancel()
                self.listener = nil
                self.settle(nil)
                if self.listened {
                    // It worked, then went (iOS closes it now and then): the
                    // pages on screen use this port, so it starts again on it.
                    self.queue.asyncAfter(deadline: .now() + 1) {
                        if self.started, self.listener == nil { self.start() }
                    }
                } else if pinned != nil {
                    // The old port is taken: the next start takes any, and
                    // the pages are made anew for it (`PagePanes.show`).
                    self.port = nil
                }
            default: break
            }
        }
        l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
        l.start(queue: queue)
    }

    private func settle(_ port: NWEndpoint.Port?) {
        let all = waiting
        waiting = []
        for w in all { w(port) }
    }

    // MARK: One connection

    private func accept(_ conn: NWConnection) {
        conn.start(queue: queue)
        readHead(conn, Data())
    }

    private func readHead(_ conn: NWConnection, _ head: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] data, _, done, error in
            guard let self else { return conn.cancel() }
            var head = head
            if let data { head.append(data) }
            if let end = head.range(of: Data("\r\n\r\n".utf8)) {
                self.connect(conn, head: head[..<end.lowerBound], early: head[end.upperBound...])
            } else if done || error != nil || head.count > Self.maxHead {
                self.answer(conn, "400 Bad Request")
            } else {
                self.readHead(conn, head)
            }
        }
    }

    /// `CONNECT host:port` with this proxy's password: on to the Mac.
    private func connect(_ conn: NWConnection, head: Data, early: Data) {
        let lines = String(decoding: head, as: UTF8.self).components(separatedBy: "\r\n")
        let first = lines.first?.split(separator: " ") ?? []
        guard first.count >= 2, first[0].uppercased() == "CONNECT",
              let colon = first[1].lastIndex(of: ":"), let port = UInt16(first[1][first[1].index(after: colon)...])
        else { return answer(conn, "400 Bad Request") }
        let host = first[1][..<colon].trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        let expected = "Basic " + Data("tenx:\(secret)".utf8).base64EncodedString()
        let auth = lines.dropFirst().first { $0.lowercased().hasPrefix("proxy-authorization:") }
            .map { $0.dropFirst("proxy-authorization:".count).trimmingCharacters(in: .whitespaces) }
        guard auth == expected else {
            return answer(conn, "407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"tenx\"")
        }
        guard let url = tunnelURL(host: host, port: port) else { return answer(conn, "502 Bad Gateway") }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(server.token)", forHTTPHeaderField: "Authorization")
        let socket = session.webSocketTask(with: request)
        socket.maximumMessageSize = 16 * 1024 * 1024
        opening[socket.taskIdentifier] = { [weak self] open in
            guard let self else { return }
            guard open else {
                // The Mac's answer: 403 is a port no task listens on.
                let status = (socket.response as? HTTPURLResponse)?.statusCode
                socket.cancel()
                return self.answer(conn, status == 403 ? "403 Forbidden" : "502 Bad Gateway")
            }
            conn.send(content: Data("HTTP/1.1 200 Connection Established\r\n\r\n".utf8), completion: .contentProcessed { _ in })
            let pump = {
                self.up(conn, socket)
                self.down(conn, socket)
            }
            if early.isEmpty {
                pump()
            } else {
                socket.send(.data(Data(early))) { error in
                    self.queue.async { error == nil ? pump() : self.end(conn, socket) }
                }
            }
        }
        socket.resume()
    }

    /// `wss://` (or `ws://` for an http server) on the server's own address.
    private func tunnelURL(host: String, port: UInt16) -> URL? {
        guard var parts = URLComponents(url: server.base, resolvingAgainstBaseURL: false) else { return nil }
        parts.scheme = parts.scheme?.lowercased() == "http" ? "ws" : "wss"
        parts.path = "/pagetunnel"
        parts.queryItems = [URLQueryItem(name: "host", value: host), URLQueryItem(name: "port", value: String(port))]
        parts.fragment = nil
        return parts.url
    }

    /// The page's bytes to the Mac.
    private func up(_ conn: NWConnection, _ socket: URLSessionWebSocketTask) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, done, error in
            guard let self else { return conn.cancel() }
            if error != nil { return self.end(conn, socket) }
            guard let data, !data.isEmpty else { return done ? self.drain(conn, socket) : self.up(conn, socket) }
            socket.send(.data(data)) { error in
                self.queue.async { error != nil ? self.end(conn, socket) : done ? self.drain(conn, socket) : self.up(conn, socket) }
            }
        }
    }

    /// The Mac's bytes to the page; when the Mac's side ends, the page's does.
    private func down(_ conn: NWConnection, _ socket: URLSessionWebSocketTask) {
        socket.receive { [weak self] result in
            guard let self else { return }
            self.queue.async {
                switch result {
                case .success(.data(let data)):
                    conn.send(content: data, completion: .contentProcessed { error in
                        error == nil ? self.down(conn, socket) : self.end(conn, socket)
                    })
                case .success:
                    self.down(conn, socket)
                case .failure:
                    conn.send(content: nil, isComplete: true, completion: .contentProcessed { _ in self.end(conn, socket) })
                }
            }
        }
    }

    /// The page is done sending. A WebSocket has no half-close, so its
    /// answer gets a while to come (`down` ends the connection when the Mac's
    /// side ends); then the tunnel goes, so a page that went away (its socket
    /// closed) never holds one open on the Mac.
    private func drain(_ conn: NWConnection, _ socket: URLSessionWebSocketTask) {
        queue.asyncAfter(deadline: .now() + 10) { self.end(conn, socket) }
    }

    private func end(_ conn: NWConnection, _ socket: URLSessionWebSocketTask) {
        socket.cancel(with: .normalClosure, reason: nil)
        conn.cancel()
    }

    private func answer(_ conn: NWConnection, _ status: String) {
        let response = "HTTP/1.1 \(status)\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        conn.send(content: Data(response.utf8), isComplete: true, completion: .contentProcessed { _ in conn.cancel() })
    }

    // MARK: URLSessionWebSocketDelegate

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        let id = webSocketTask.taskIdentifier
        queue.async { self.opening.removeValue(forKey: id)?(true) }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let id = task.taskIdentifier
        queue.async { self.opening.removeValue(forKey: id)?(false) }
    }
}
