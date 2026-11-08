import GameController
import UIKit
import WebKit

/// The tenx web page, full screen. What it adds to the page in Safari:
/// the microphone granted by the app (no WebKit prompt per visit), the
/// clipboard read and written without the Paste bubble, kept in step with
/// the Mac's (`tenx` message handler, web/src/lib/native.ts and clipsync.ts),
/// whether a hardware keyboard is attached, and no iPadOS shortcut bar over it.
final class WebViewController: UIViewController {
    let server: Server
    /// The page can't be shown: the server refused the token, or isn't there.
    var onProblem: (String) -> Void = { _ in }
    var onSettings: () -> Void = {}

    private var webView: WKWebView!

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
        NotificationCenter.default.addObserver(self, selector: #selector(hardwareKeyboardChanged), name: .GCKeyboardDidConnect, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(hardwareKeyboardChanged), name: .GCKeyboardDidDisconnect, object: nil)
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        webView.load(URLRequest(url: server.signInURL))
    }

    override var keyCommands: [UIKeyCommand]? {
        [UIKeyCommand(title: "Settings", action: #selector(openSettings), input: ",", modifierFlags: .command)]
    }

    override var prefersStatusBarHidden: Bool { false }
    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }
    override var prefersHomeIndicatorAutoHidden: Bool { true }

    @objc func openSettings() { onSettings() }

    @objc private func keyboardWillShow() { ShortcutBar.hide(in: webView) }

    /// The page hides its key bar while a hardware keyboard is attached
    /// (web/src/lib/native.ts `HARDWARE_KEYBOARD_EVENT`). The notification
    /// can come before `coalesced` has caught up, so the next turn reads it.
    @objc private func hardwareKeyboardChanged() {
        DispatchQueue.main.async { [weak self] in
            let js = "window.dispatchEvent(new CustomEvent('tenx-hardware-keyboard', { detail: \(HardwareKeyboard.attached) }))"
            self?.webView.evaluateJavaScript(js)
        }
    }
}

/// A hardware keyboard (Magic Keyboard, Smart Keyboard, Bluetooth) is
/// attached: GameController sees every one, and no web API does.
enum HardwareKeyboard {
    static var attached: Bool { GCKeyboard.coalesced != nil }
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
        case "clipboardPeek":
            return (Pasteboard.peek(since: body?["since"] as? Int), nil)
        case "clipboardWrite":
            guard let text = body?["text"] as? String, let ifCount = body?["ifCount"] as? Int else { return (nil, "no text") }
            return (Pasteboard.write(text, ifCount: ifCount) ?? NSNull(), nil)
        case "hardwareKeyboard":
            return (HardwareKeyboard.attached, nil)
        case "settings":
            owner.openSettings()
            return (nil, nil)
        default:
            return (nil, "unknown action")
        }
    }
}
