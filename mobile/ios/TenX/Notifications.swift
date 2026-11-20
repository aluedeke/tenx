import UIKit
import UserNotifications

/// Notifications from `tenx web` and `tenx notify`. A WKWebView has no Web
/// Push, so the app registers with APNs itself and hands the page its device
/// token (`pushState`, `pushEnable` on the `tenx` message handler); the page
/// gives it to the server (POST /push/apns/subscribe), which sends through
/// APNs. A tap on one opens its task in the page (`NotificationTap`).
@MainActor
final class PushRegistration {
    static let shared = PushRegistration()

    /// The device token as hex, once APNs gave one this launch.
    private(set) var token: String?
    private var waiting: [CheckedContinuation<String?, Never>] = []

    /// Which APNs the token belongs to: Xcode's builds are signed for the
    /// sandbox, TestFlight's and the App Store's for production.
    static var env: String {
        #if DEBUG
        "sandbox"
        #else
        "production"
        #endif
    }

    /// iOS's permission and, when granted, the device token — without
    /// asking the user anything.
    func state() async -> [String: Any] {
        let permission = await Self.permission()
        if permission == "granted" { _ = await register() }
        return reply(permission)
    }

    /// Ask for permission (iOS shows its prompt once), then register.
    func enable() async -> [String: Any] {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
        return await state()
    }

    private func reply(_ permission: String) -> [String: Any] {
        var out: [String: Any] = ["permission": permission, "env": Self.env]
        if let token { out["token"] = token }
        return out
    }

    private static func permission() async -> String {
        switch await UNUserNotificationCenter.current().notificationSettings().authorizationStatus {
        case .authorized, .provisional, .ephemeral: "granted"
        case .denied: "denied"
        default: "notDetermined"
        }
    }

    /// The device token: iOS hands it to the app delegate, usually within a
    /// second; nil when it fails or takes longer than ten.
    private func register() async -> String? {
        if let token { return token }
        return await withCheckedContinuation { continuation in
            waiting.append(continuation)
            UIApplication.shared.registerForRemoteNotifications()
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(10))
                self?.finish(nil)
            }
        }
    }

    func registered(_ deviceToken: Data) {
        token = deviceToken.map { String(format: "%02x", $0) }.joined()
        finish(token)
    }

    func failed() {
        finish(nil)
    }

    private func finish(_ token: String?) {
        let done = waiting
        waiting = []
        for continuation in done { continuation.resume(returning: token) }
    }
}

/// A tapped notification's page address (`/?task=<id>`). Kept until the
/// page takes it: a tap that launched the app comes before the page exists
/// (it asks with `takeTap` once it runs); one into a running app is also
/// handed to the page at once (`WebViewController.openTapped`).
@MainActor
enum NotificationTap {
    static let opened = Notification.Name("tenx.notificationTapped")
    private static var pending: String?

    static func open(_ url: String) {
        pending = url
        NotificationCenter.default.post(name: opened, object: nil)
    }

    static func take() -> String? {
        defer { pending = nil }
        return pending
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        PushRegistration.shared.registered(deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        PushRegistration.shared.failed()
    }

    /// Shown while the app is open too: it may be on another task.
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let url = response.notification.request.content.userInfo["url"] as? String ?? "/"
        await MainActor.run { NotificationTap.open(url) }
    }
}
