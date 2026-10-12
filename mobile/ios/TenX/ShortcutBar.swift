import UIKit
import WebKit

/// iPadOS shows a bar of shortcut and suggestion buttons at the bottom of the
/// screen whenever a text field has the focus, even over a hardware keyboard.
/// A terminal needs none of it. There is no WebKit setting for it: the bar
/// comes from the `inputAssistantItem` of the web view's content view
/// (WKContentView, private), so this finds that view by its class name and
/// empties the bar's button groups. If a later iOS renames or restructures
/// it, nothing is found and the bar simply stays.
@MainActor
enum ShortcutBar {
    static func hide(in webView: WKWebView) {
        guard UIDevice.current.userInterfaceIdiom == .pad else { return }
        for view in webView.scrollView.subviews where String(describing: type(of: view)).hasPrefix("WKContent") {
            let item = view.inputAssistantItem
            item.leadingBarButtonGroups = []
            item.trailingBarButtonGroups = []
        }
    }
}
