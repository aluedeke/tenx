import Network
import UIKit
import WebKit

/// A task's pages (web/src/lib/pages.ts, PagePane.tsx): web pages in its tab
/// bar after agent, shell and nvim. The tenx page draws the bar and leaves a
/// hole over the terminal; this lays a web view of its own there. Each page
/// has its web view while it's among the last few shown, so going back to it
/// finds it as it was left; each workspace has its own cookie store
/// (`PageStores`), so a login in one workspace's pages is never another's.
@MainActor
final class PagePanes: NSObject {
    private weak var host: UIView?
    /// The tenx server: its host and token reach a page's `localhost`
    /// through `tenx web`'s page proxy.
    private let server: Server
    /// Runs a script in the tenx page: where a page went (`tenx-page`).
    private let report: (String) -> Void
    private var views: [String: PageWebView] = [:]
    /// Keys, the last shown last: the first ones go when there are too many.
    private var order: [String] = []
    private static let keep = 6

    init(over host: UIView, server: Server, report: @escaping (String) -> Void) {
        self.host = host
        self.server = server
        self.report = report
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(storeForgotten(_:)), name: PageStores.forgetting, object: nil)
    }

    /// Show page `key` over `rect` (the tenx page's CSS pixels, which are the
    /// host's points), loading `url` only into a web view made now.
    func show(key: String, url: URL, store: UUID, storeName: String, proxy: UInt16?, rect: CGRect) {
        guard let host else { return }
        PageStores.remember(store, as: storeName)
        let page: PageWebView
        if let known = views[key], known.store == store, known.proxy == proxy {
            page = known
        } else {
            views[key]?.removeFromSuperview()
            page = PageWebView(key: key, store: store, proxy: proxy.map { PageProxy(server: server, port: $0) }, report: report)
            views[key] = page
            page.load(URLRequest(url: url))
        }
        order.removeAll { $0 == key }
        order.append(key)
        for (other, view) in views where other != key { view.park() }
        if page.superview == nil { host.addSubview(page) }
        page.frame = rect
        page.isHidden = false
        page.setAllMediaPlaybackSuspended(false)
        page.reportState()
        while order.count > Self.keep {
            let old = order.removeFirst()
            views.removeValue(forKey: old)?.removeFromSuperview()
        }
    }

    func hide() {
        for view in views.values { view.park() }
    }

    func perform(key: String, what: String) {
        guard let page = views[key] else { return }
        switch what {
        case "back": page.goBack()
        case "forward": page.goForward()
        case "reload":
            if page.isLoading {
                page.stopLoading()
            } else if let failed = page.failed {
                page.load(URLRequest(url: failed))
            } else {
                page.reload()
            }
        default: break
        }
    }

    func close(key: String) {
        views.removeValue(forKey: key)?.removeFromSuperview()
        order.removeAll { $0 == key }
    }

    /// The store is about to be removed: no web view may still use it.
    @objc private func storeForgotten(_ note: Notification) {
        guard let store = note.object as? UUID else { return }
        for (key, view) in views where view.store == store { close(key: key) }
    }
}

/// One page's web view, with its workspace's cookie store.
@MainActor
private final class PageWebView: WKWebView {
    let key: String
    let store: UUID
    let proxy: UInt16?
    private let via: PageProxy?
    private let report: (String) -> Void
    /// The `localhost` address whose failure the page shows: what the bar
    /// says and what reload tries again.
    private(set) var failed: URL?
    private var watching: [NSKeyValueObservation] = []

    init(key: String, store: UUID, proxy: PageProxy?, report: @escaping (String) -> Void) {
        self.key = key
        self.store = store
        self.proxy = proxy?.port
        self.via = proxy
        self.report = report
        let config = WKWebViewConfiguration()
        config.websiteDataStore = WKWebsiteDataStore(forIdentifier: store)
        if let proxy { config.websiteDataStore.proxyConfigurations = [proxy.configuration] }
        config.allowsInlineMediaPlayback = true
        super.init(frame: .zero, configuration: config)
        navigationDelegate = self
        uiDelegate = self
        allowsBackForwardNavigationGestures = true
        backgroundColor = .ground
        #if DEBUG
        isInspectable = true
        #endif
        let changed: @Sendable () -> Void = { [weak self] in
            Task { @MainActor in self?.reportState() }
        }
        watching = [
            observe(\.url) { _, _ in changed() },
            observe(\.title) { _, _ in changed() },
            observe(\.canGoBack) { _, _ in changed() },
            observe(\.canGoForward) { _, _ in changed() },
            observe(\.isLoading) { _, _ in changed() },
        ]
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    /// Out of sight: the keyboard goes with it (its first responder is
    /// WebKit's own view inside), and so does its sound.
    func park() {
        guard !isHidden else { return }
        endEditing(true)
        isHidden = true
        pauseAllMediaPlayback()
        setAllMediaPlaybackSuspended(true)
    }

    /// Where the page is, for the tenx page's bar and the task's list
    /// (web/src/lib/native.ts `PAGE_EVENT`). Only while it's shown.
    func reportState() {
        guard !isHidden else { return }
        let state: [String: Any] = [
            "key": key,
            "url": failed?.absoluteString ?? url?.absoluteString ?? "",
            "title": title ?? "",
            "canGoBack": canGoBack,
            "canGoForward": canGoForward,
            "loading": isLoading,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: state), let json = String(data: data, encoding: .utf8) else { return }
        report("window.dispatchEvent(new CustomEvent('tenx-page', { detail: \(json) }))")
    }
}

extension PageWebView: WKNavigationDelegate, WKUIDelegate {
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = action.request.url, let scheme = url.scheme?.lowercased() else { return .cancel }
        if action.targetFrame?.isMainFrame ?? true, scheme == "http" || scheme == "https" { failed = nil }
        if ["http", "https", "about", "blob", "data"].contains(scheme) { return .allow }
        // mailto:, tel:, another app's link: iOS's, for a link tapped in the
        // page itself; a script or a frame (an ad, an embed) never switches apps.
        if action.navigationType == .linkActivated, action.targetFrame?.isMainFrame ?? true { await UIApplication.shared.open(url) }
        return .cancel
    }

    /// `target=_blank` and `window.open`: a window over the page, with the
    /// same cookies and `window.opener` — what a sign-in popup (GitHub,
    /// Google, Microsoft) needs to hand its result back. It closes itself
    /// (`window.close()`) or with Done.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        guard let owner = window?.rootViewController.map(Self.topmost) else {
            webView.load(action.request)
            return nil
        }
        let popup = PopupController(configuration: configuration)
        owner.present(UINavigationController(rootViewController: popup), animated: true)
        return popup.webView
    }

    private static func topmost(_ vc: UIViewController) -> UIViewController {
        var top = vc
        while let next = top.presentedViewController { top = next }
        return top
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webView.reload()
    }

    /// A `localhost` page that didn't load: say what it takes, in its place.
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        let e = error as NSError
        guard !(e.domain == NSURLErrorDomain && e.code == NSURLErrorCancelled),
              let url = e.userInfo[NSURLErrorFailingURLErrorKey] as? URL, PageProxy.isLocal(url)
        else { return }
        failed = url
        webView.loadHTMLString(PageProxy.failure(url: url, via: via, error: e.localizedDescription), baseURL: nil)
    }
}

/// `tenx web`'s page proxy (`tenx_core::pageproxy`) as the server's host
/// sees it: a page's `localhost` connections go there, the Mac's own ports
/// instead of the phone's. Through Tailscale's TLS for an https server.
struct PageProxy {
    let server: Server
    let port: UInt16

    var host: String { server.base.host ?? "localhost" }

    var configuration: ProxyConfiguration {
        let endpoint = NWEndpoint.hostPort(host: .init(host), port: .init(rawValue: port) ?? 7075)
        let tls = server.base.scheme?.lowercased() == "https" ? NWProtocolTLS.Options() : nil
        var proxy = ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: tls)
        proxy.applyCredential(username: "tenx", password: server.token)
        // Only the Mac's own addresses: every other site goes its own way.
        // `localhost` alone, as the page assumes (pages.ts `viaProxy`).
        proxy.matchDomains = ["localhost"]
        return proxy
    }

    /// The Mac's own address as a page names it: every loopback name
    /// (web/src/lib/pages.ts `isLocalUrl`). Only `localhost` goes through
    /// the proxy (`matchDomains`); the page shows a note for the rest
    /// (`viaProxy`).
    static func isLocal(_ url: URL) -> Bool {
        let host = (url.host ?? "").lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        return host == "localhost" || host.hasSuffix(".localhost") || host == "::1" || host.hasPrefix("127.")
    }

    /// The page shown when a `localhost` page didn't load.
    static func failure(url: URL, via: PageProxy?, error: String) -> String {
        func esc(_ s: String) -> String { s.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;") }
        let what = url.host.map { "\($0):\(url.port ?? (url.scheme == "https" ? 443 : 80))" } ?? url.absoluteString
        let how: String
        if let via {
            how = "It goes through tenx web's page proxy on \(esc(via.host)):\(via.port). Either no task listens on that port, or this device can't reach the proxy: on a tailnet the Mac needs <code>tailscale serve --bg --tls-terminated-tcp=\(via.port) tcp://127.0.0.1:\(via.port)</code>."
        } else {
            how = "tenx web runs without its page proxy, so this device can't reach the Mac's own addresses."
        }
        return "<!doctype html><meta name=viewport content=\"width=device-width\"><body style=\"font:15px -apple-system;background:#171820;color:#c8ccd4;padding:24px;line-height:1.45\"><h3 style=\"color:#fff\">\(esc(what)) didn't load</h3><p>\(how)</p><p style=\"opacity:.6\">\(esc(error))</p>"
    }
}

/// A page's popup window (`createWebViewWith`), as a sheet.
@MainActor
private final class PopupController: UIViewController, WKUIDelegate, WKNavigationDelegate {
    let webView: WKWebView

    init(configuration: WKWebViewConfiguration) {
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init(nibName: nil, bundle: nil)
        webView.uiDelegate = self
        webView.navigationDelegate = self
        #if DEBUG
        webView.isInspectable = true
        #endif
        navigationItem.rightBarButtonItem = UIBarButtonItem(systemItem: .done, primaryAction: UIAction { [weak self] _ in self?.dismiss(animated: true) })
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    override func loadView() { view = webView }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        title = webView.url?.host
    }

    func webViewDidClose(_ webView: WKWebView) {
        dismiss(animated: true)
    }

    /// A popup's own popup: in the popup.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        webView.load(action.request)
        return nil
    }
}

/// The workspaces' cookie stores (`tenx_core::pages::store_id`), and the
/// names the settings list them by.
@MainActor
enum PageStores {
    /// Posted with the store's UUID before it is removed.
    static let forgetting = Notification.Name("tenx.pageStores.forgetting")
    private static let namesKey = "pageStoreNames"

    /// Workspace names by store, as last shown.
    static var names: [UUID: String] {
        let raw = UserDefaults.standard.dictionary(forKey: namesKey) as? [String: String] ?? [:]
        return Dictionary(uniqueKeysWithValues: raw.compactMap { k, v in UUID(uuidString: k).map { ($0, v) } })
    }

    static func remember(_ store: UUID, as name: String) {
        var raw = UserDefaults.standard.dictionary(forKey: namesKey) as? [String: String] ?? [:]
        guard raw[store.uuidString] != name else { return }
        raw[store.uuidString] = name
        UserDefaults.standard.set(raw, forKey: namesKey)
    }

    /// The store's cookies, logins and caches, gone.
    static func forget(_ store: UUID) async {
        NotificationCenter.default.post(name: forgetting, object: store)
        // A web view's process lets go of its store a moment after it's gone.
        try? await Task.sleep(for: .milliseconds(300))
        try? await WKWebsiteDataStore.remove(forIdentifier: store)
        var raw = UserDefaults.standard.dictionary(forKey: namesKey) as? [String: String] ?? [:]
        raw.removeValue(forKey: store.uuidString)
        UserDefaults.standard.set(raw, forKey: namesKey)
    }

    /// Every store, also one whose name was never saved: signing out.
    static func forgetAll() async {
        let known = Set(names.keys).union(await WKWebsiteDataStore.allDataStoreIdentifiers)
        for store in known { await forget(store) }
    }
}
