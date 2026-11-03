import UIKit
import WebKit

/// The tenx web page, full screen. What it adds to the page in Safari:
/// the microphone granted by the app (no WebKit prompt per visit), the
/// clipboard read without the Paste bubble (`tenx` message handler,
/// web/src/lib/native.ts), talk mode as a call that goes on with the screen
/// off (`CallManager`, web/src/lib/voice.ts), and no iPadOS shortcut bar over
/// a hardware keyboard.
final class WebViewController: UIViewController {
    let server: Server
    /// The page can't be shown: the server refused the token, or isn't there.
    var onProblem: (String) -> Void = { _ in }
    var onSettings: () -> Void = {}

    private var webView: WKWebView!
    /// Talk mode's events while the app is in the background, for the page
    /// once it is back: it runs nothing meanwhile, and a long call would
    /// pile them up. The last few are enough to draw the captions.
    private var backlog: [String] = []
    private static let backlogSize = 40

    init(server: Server) {
        self.server = server
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func loadView() {
        let config = WKWebViewConfiguration()
        // The default store keeps the sign-in cookie between launches.
        config.websiteDataStore = .default()
        config.allowsInlineMediaPlayback = true
        config.mediaTypesRequiringUserActionForPlayback = []
        config.userContentController.addScriptMessageHandler(Bridge(owner: self), contentWorld: .page, name: "tenx")
        // Tells the page talk mode is the app's: lib/voice.ts hands it over.
        config.userContentController.addUserScript(WKUserScript(
            source: "window.tenxNative = Object.assign(window.tenxNative || {}, { voice: 1 });",
            injectionTime: .atDocumentStart, forMainFrameOnly: true))

        webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = false
        webView.allowsLinkPreview = false
        // The page pads itself with the safe-area insets (viewport-fit=cover),
        // and it is a terminal: no rubber band, no zoom.
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.scrollView.bounces = false
        webView.isOpaque = false
        webView.backgroundColor = .ground
        webView.scrollView.backgroundColor = webView.backgroundColor
        #if DEBUG
        webView.isInspectable = true
        #endif
        view = webView

        // Settings without a menu in the way: hold two fingers for a second.
        let hold = UILongPressGestureRecognizer(target: self, action: #selector(openSettings))
        hold.numberOfTouchesRequired = 2
        hold.minimumPressDuration = 1
        hold.cancelsTouchesInView = false
        webView.addGestureRecognizer(hold)

        NotificationCenter.default.addObserver(self, selector: #selector(keyboardWillShow), name: UIResponder.keyboardWillShowNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(becameActive), name: UIApplication.didBecomeActiveNotification, object: nil)
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        webView.load(URLRequest(url: server.signInURL))
        CallManager.shared.onEvent = { [weak self] event in self?.voiceEvent(event) }
    }

    /// What the call did, to the page (`window.__tenxVoice`, lib/voice.ts).
    /// With the screen off the page runs nothing; it catches up when shown.
    private func voiceEvent(_ event: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: event), let json = String(data: data, encoding: .utf8) else { return }
        guard UIApplication.shared.applicationState == .active else {
            backlog.append(json)
            if backlog.count > Self.backlogSize { backlog.removeFirst(backlog.count - Self.backlogSize) }
            return
        }
        deliver([json])
    }

    private func deliver(_ events: [String]) {
        guard !events.isEmpty else { return }
        let calls = events.map { "window.__tenxVoice(\($0));" }.joined()
        webView.evaluateJavaScript("if (window.__tenxVoice) { \(calls) }", completionHandler: nil)
    }

    @objc private func becameActive() {
        let events = backlog
        backlog.removeAll()
        deliver(events)
    }

    override var keyCommands: [UIKeyCommand]? {
        [UIKeyCommand(title: "Settings", action: #selector(openSettings), input: ",", modifierFlags: .command)]
    }

    override var prefersStatusBarHidden: Bool { false }
    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }
    override var prefersHomeIndicatorAutoHidden: Bool { true }

    @objc func openSettings() { onSettings() }

    @objc private func keyboardWillShow() { ShortcutBar.hide(in: webView) }
}

extension WebViewController: WKNavigationDelegate {
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = action.request.url else { return .cancel }
        if server.owns(url) || url.scheme == "about" || url.scheme == "blob" || url.scheme == "data" { return .allow }
        // GitHub, Linear, …: in Safari (or their app), not in place of the terminal.
        if action.targetFrame?.isMainFrame ?? true {
            await UIApplication.shared.open(url)
            return .cancel
        }
        return .allow
    }

    func webView(_ webView: WKWebView, decidePolicyFor response: WKNavigationResponse) async -> WKNavigationResponsePolicy {
        if response.isForMainFrame, let http = response.response as? HTTPURLResponse, http.statusCode == 401 {
            onProblem("The server refused the token — `tenx web --rotate-token` may have replaced it. Paste the new address.")
            return .cancel
        }
        return .allow
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        ShortcutBar.hide(in: webView)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        let e = error as NSError
        if e.domain == NSURLErrorDomain && e.code == NSURLErrorCancelled { return }
        if e.domain == "WebKitErrorDomain" && e.code == 102 { return } // our own .cancel
        onProblem("Couldn't reach \(server.base.host ?? "the server"): \(error.localizedDescription)")
    }

    // iOS ends a web view's process while the app is away; start again.
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webView.load(URLRequest(url: server.signInURL))
    }
}

extension WebViewController {
    /// The page's talk mode: `start`, `pause`, `look`, `stop`.
    func voice(_ body: [String: Any]) async -> (Any?, String?) {
        let calls = CallManager.shared
        voiceLog.info("page: voice \(body["op"] as? String ?? "?", privacy: .public)")
        switch body["op"] as? String {
        case "start":
            do {
                try await calls.start(server: server, task: body["task"] as? String, session: body["session"] as? String)
                return (nil, nil)
            } catch {
                return (nil, error.localizedDescription)
            }
        case "pause":
            calls.setPaused(body["paused"] as? Bool ?? false)
        case "look":
            calls.look(at: body["task"] as? String)
        case "stop":
            calls.stop()
        default:
            return (nil, "unknown voice op")
        }
        return (nil, nil)
    }
}

extension WebViewController: WKUIDelegate {
    /// The page's microphone, on our own server only: iOS asks once for the
    /// app (NSMicrophoneUsageDescription), WebKit never asks again.
    func webView(_ webView: WKWebView, decideMediaCapturePermissionsFor origin: WKSecurityOrigin, initiatedBy frame: WKFrameInfo, type: WKMediaCaptureType) async -> WKPermissionDecision {
        origin.host.lowercased() == server.base.host?.lowercased() ? .grant : .deny
    }

    /// `target=_blank` and `window.open`: outside the app.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = action.request.url {
            if server.owns(url) { webView.load(action.request) } else { UIApplication.shared.open(url) }
        }
        return nil
    }
}

/// The page's calls into the app (web/src/lib/native.ts).
@MainActor
private final class Bridge: NSObject, WKScriptMessageHandlerWithReply {
    weak var owner: WebViewController?

    init(owner: WebViewController) { self.owner = owner }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) async -> (Any?, String?) {
        guard let owner, let url = message.frameInfo.request.url, owner.server.owns(url) else { return (nil, "not this server's page") }
        let body = message.body as? [String: Any]
        switch body?["action"] as? String {
        case "clipboard":
            let items = Pasteboard.items()
            if Pasteboard.blocked(items) {
                return (nil, "iOS kept the clipboard from the app. Settings → Apps → tenx → Paste from Other Apps → Allow")
            }
            return (items, nil)
        case "settings":
            owner.openSettings()
            return (nil, nil)
        case "voice":
            return await owner.voice(body ?? [:])
        default:
            return (nil, "unknown action")
        }
    }
}
