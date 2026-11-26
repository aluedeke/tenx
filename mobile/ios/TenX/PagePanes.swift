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
    /// Runs a script in the tenx page: where a page went (`tenx-page`).
    private let report: (String) -> Void
    private var views: [String: PageWebView] = [:]
    /// Keys, the last shown last: the first ones go when there are too many.
    private var order: [String] = []
    private static let keep = 6

    init(over host: UIView, report: @escaping (String) -> Void) {
        self.host = host
        self.report = report
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(storeForgotten(_:)), name: PageStores.forgetting, object: nil)
    }

    /// Show page `key` over `rect` (the tenx page's CSS pixels, which are the
    /// host's points), loading `url` only into a web view made now.
    func show(key: String, url: URL, store: UUID, storeName: String, rect: CGRect) {
        guard let host else { return }
        PageStores.remember(store, as: storeName)
        let page: PageWebView
        if let known = views[key], known.store == store {
            page = known
        } else {
            views[key]?.removeFromSuperview()
            page = PageWebView(key: key, store: store, report: report)
            views[key] = page
            page.load(URLRequest(url: url))
        }
        order.removeAll { $0 == key }
        order.append(key)
        for (other, view) in views where other != key { view.isHidden = true }
        if page.superview == nil { host.addSubview(page) }
        page.frame = rect
        page.isHidden = false
        page.reportState()
        while order.count > Self.keep {
            let old = order.removeFirst()
            views.removeValue(forKey: old)?.removeFromSuperview()
        }
    }

    func hide() {
        for view in views.values {
            view.isHidden = true
            view.resignFirstResponder()
        }
    }

    func perform(key: String, what: String) {
        guard let page = views[key] else { return }
        switch what {
        case "back": page.goBack()
        case "forward": page.goForward()
        case "reload": if page.isLoading { page.stopLoading() } else { page.reload() }
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
    private let report: (String) -> Void
    private var watching: [NSKeyValueObservation] = []

    init(key: String, store: UUID, report: @escaping (String) -> Void) {
        self.key = key
        self.store = store
        self.report = report
        let config = WKWebViewConfiguration()
        config.websiteDataStore = WKWebsiteDataStore(forIdentifier: store)
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

    /// Where the page is, for the tenx page's bar and the task's list
    /// (web/src/lib/native.ts `PAGE_EVENT`). Only while it's shown.
    func reportState() {
        guard !isHidden else { return }
        let state: [String: Any] = [
            "key": key,
            "url": url?.absoluteString ?? "",
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
        if ["http", "https", "about", "blob", "data"].contains(scheme) { return .allow }
        // mailto:, tel:, another app's link: iOS's.
        await UIApplication.shared.open(url)
        return .cancel
    }

    /// `target=_blank` and `window.open`: in this page, which has no tabs of
    /// its own (the task's bar is the tabs).
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        webView.load(action.request)
        return nil
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webView.reload()
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
