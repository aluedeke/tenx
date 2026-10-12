import UIKit
import UniformTypeIdentifiers

/// The pasteboard in the shape the page's `navigator.clipboard.read()` gives
/// (web/src/lib/native.ts): per item, MIME type → text, or base64 for an
/// image. Read here, the app's "Paste from Other Apps: Allow" setting applies
/// instead of WebKit's Paste bubble on every read. With that setting on
/// "Ask", iOS shows no question for a read the page started: it just hands
/// back nothing, so `blocked` tells the two cases apart.
@MainActor
enum Pasteboard {
    static func items(_ board: UIPasteboard = .general) -> [[String: Any]] {
        (0..<board.numberOfItems).compactMap { index in
            let set = IndexSet(integer: index)
            let utis = board.types(forItemSet: set)?.first ?? []
            var data: [String: String] = [:]
            var types: [String] = []
            func add(_ mime: String, _ value: String) {
                guard data[mime] == nil else { return }
                data[mime] = value
                types.append(mime)
            }
            for uti in utis {
                guard let type = UTType(uti), let raw = board.data(forPasteboardType: uti, inItemSet: set)?.first else { continue }
                if type.conforms(to: .html), let text = String(data: raw, encoding: .utf8) {
                    add("text/html", text)
                } else if type.conforms(to: .plainText) || type.conforms(to: .url), let text = String(data: raw, encoding: .utf8) {
                    add("text/plain", text)
                } else if type.conforms(to: .png) || type.conforms(to: .jpeg) || type.conforms(to: .gif) || type.conforms(to: .webP) {
                    add(type.preferredMIMEType ?? "image/png", raw.base64EncodedString())
                } else if type.conforms(to: .image), let png = UIImage(data: raw)?.pngData() {
                    // HEIC, TIFF and the like: PNG, which the agent can read.
                    add("image/png", png.base64EncodedString())
                }
            }
            return types.isEmpty ? nil : ["types": types, "data": data]
        }
    }

    /// Something is on the pasteboard, but iOS gave none of it to us.
    static func blocked(_ items: [[String: Any]], _ board: UIPasteboard = .general) -> Bool {
        items.isEmpty && (board.hasStrings || board.hasImages || board.hasURLs)
    }
}
