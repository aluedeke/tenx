import UIKit

/// A link held in the terminal (web/src/lib/native.ts `nativeLinkMenu`):
/// iOS's own edit menu at the finger — Open as page, Copy, Open, Share. The terminal's
/// links are text it draws, not anchors, so WebKit offers no menu of its
/// own for them; in Safari the page shows its menu instead.
@MainActor
final class LinkMenu: NSObject, @MainActor UIEditMenuInteractionDelegate {
    private weak var owner: UIViewController?
    private var interaction: UIEditMenuInteraction!
    private var url: URL?
    private var point = CGPoint.zero
    private let openPage: (URL) -> Void

    init(over owner: UIViewController, openPage: @escaping (URL) -> Void) {
        self.owner = owner
        self.openPage = openPage
        super.init()
        interaction = UIEditMenuInteraction(delegate: self)
        owner.view.addInteraction(interaction)
    }

    /// `point` in the page's CSS pixels, which are the view's points: the
    /// page is never zoomed.
    func show(_ url: URL, at point: CGPoint) {
        self.url = url
        self.point = point
        interaction.presentEditMenu(with: UIEditMenuConfiguration(identifier: nil, sourcePoint: point))
    }

    func editMenuInteraction(_ interaction: UIEditMenuInteraction, menuFor configuration: UIEditMenuConfiguration, suggestedActions: [UIMenuElement]) -> UIMenu? {
        guard let url else { return nil }
        let web = ["http", "https"].contains(url.scheme?.lowercased() ?? "")
        // A page of the task even when the site's app is installed.
        let asPage = UIAction(title: "Open as page", image: UIImage(systemName: "rectangle.on.rectangle")) { [weak self] _ in
            self?.openPage(url)
        }
        return UIMenu(children: (web ? [asPage] : []) + [
            UIAction(title: "Copy", image: UIImage(systemName: "doc.on.doc")) { _ in
                // As text: pasted into the terminal (or carried to the Mac by
                // the clipboard sync) it is the address, not a file promise.
                UIPasteboard.general.string = url.absoluteString
            },
            UIAction(title: "Open", image: UIImage(systemName: "safari")) { _ in
                UIApplication.shared.open(url)
            },
            UIAction(title: "Share…", image: UIImage(systemName: "square.and.arrow.up")) { [weak self] _ in
                self?.share(url)
            },
        ])
    }

    private func share(_ url: URL) {
        guard let owner else { return }
        let sheet = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        // An iPad shows it as a popover, pointing at the link.
        sheet.popoverPresentationController?.sourceView = owner.view
        sheet.popoverPresentationController?.sourceRect = CGRect(origin: point, size: .zero)
        owner.present(sheet, animated: true)
    }
}
