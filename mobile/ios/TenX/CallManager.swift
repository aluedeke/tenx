import AVFoundation
import CallKit
@preconcurrency import WebRTC

/// Talk mode as a CallKit call: iOS keeps the app, its microphone and its
/// voice running with the screen off, shows the call on the lock screen
/// with mute and end, and puts it on hold for a real phone call. The audio
/// is `VoiceCall`'s; CallKit only says when the audio session may run.
///
/// The page starts, pauses and ends it through the `tenx` message handler
/// (`WebViewController`); what happens goes back to it as events (`onEvent`):
/// `message` (a data channel message, as it came), `connected` (again after
/// a reconnect), `reconnecting`, `paused` and `ended`.
@MainActor
final class CallManager: NSObject {
    static let shared = CallManager()

    /// To the page: `window.__tenxVoice(event)`.
    var onEvent: ([String: Any]) -> Void = { _ in }

    private let provider: CXProvider
    private let controller = CXCallController()
    private var call: VoiceCall?
    private var uuid: UUID?
    /// What to start once CallKit has said yes (`CXStartCallAction`).
    private var pending: VoiceCall?
    /// The call CallKit was told is connected.
    private var reported: UUID?

    override init() {
        let config = CXProviderConfiguration()
        config.supportsVideo = false
        config.maximumCallGroups = 1
        config.maximumCallsPerCallGroup = 1
        config.supportedHandleTypes = [.generic]
        config.includesCallsInRecents = false
        provider = CXProvider(configuration: config)
        super.init()
        provider.setDelegate(self, queue: nil) // the main queue
    }

    var active: Bool { call != nil }

    /// The simulator ends every CallKit call at once: there the call runs
    /// without it, so the rest can be tried on this Mac.
    #if targetEnvironment(simulator)
    private static let callKit = false
    #else
    private static let callKit = true
    #endif

    /// A tap on 🎧. A call already running — the page was reloaded under it —
    /// is handed to the new page instead.
    func start(server: Server, task: String?, session: String?) async throws {
        if let call {
            onEvent(["kind": "connected"])
            if call.paused { onEvent(["kind": "paused", "paused": true]) }
            return
        }
        let uuid = UUID()
        let call = VoiceCall(server: server, task: task, session: session)
        call.onMessage = { [weak self] text in self?.onEvent(["kind": "message", "data": text]) }
        call.onConnected = { [weak self] in
            guard let self, self.uuid == uuid else { return }
            // Once: CallKit's call goes on through a reconnect.
            if Self.callKit, reported != uuid {
                reported = uuid
                provider.reportOutgoingCall(with: uuid, connectedAt: nil)
            }
            onEvent(["kind": "connected"])
        }
        call.onReconnecting = { [weak self] in
            guard let self, self.uuid == uuid else { return }
            onEvent(["kind": "reconnecting"])
        }
        call.onEnd = { [weak self] error in self?.ended(uuid, error: error) }
        self.uuid = uuid
        self.call = call
        guard Self.callKit else {
            configureAudio()
            let rtc = RTCAudioSession.sharedInstance()
            rtc.lockForConfiguration()
            try? rtc.setActive(true)
            rtc.unlockForConfiguration()
            rtc.isAudioEnabled = true
            Task { await call.start() }
            return
        }
        pending = call
        let start = CXStartCallAction(call: uuid, handle: CXHandle(type: .generic, value: "tenx"))
        start.contactIdentifier = "tenx"
        do {
            try await controller.request(CXTransaction(action: start))
        } catch {
            self.call = nil
            self.uuid = nil
            pending = nil
            throw VoiceError("iOS didn't start the call: \(error.localizedDescription)")
        }
    }

    /// The page's pause: through CallKit, so its mute button shows it too.
    func setPaused(_ paused: Bool) {
        guard let uuid else { return }
        guard Self.callKit else {
            call?.setPaused(paused)
            return onEvent(["kind": "paused", "paused": paused])
        }
        controller.request(CXTransaction(action: CXSetMutedCallAction(call: uuid, muted: paused))) { _ in }
    }

    func look(at task: String?) { call?.look(at: task) }

    func stop() {
        guard let uuid else { return }
        voiceLog.info("end requested")
        guard Self.callKit else {
            let call = self.call
            finish(error: nil)
            call?.onEnd = { _ in }
            call?.stop()
            return
        }
        controller.request(CXTransaction(action: CXEndCallAction(call: uuid))) { _ in }
    }

    /// The call ended without CallKit asking: the program went away, or the
    /// connection failed.
    private func ended(_ uuid: UUID, error: String?) {
        guard self.uuid == uuid else { return }
        if Self.callKit { provider.reportCall(with: uuid, endedAt: nil, reason: error == nil ? .remoteEnded : .failed) }
        finish(error: error)
    }

    private func finish(error: String?) {
        call = nil
        uuid = nil
        pending = nil
        var event: [String: Any] = ["kind": "ended"]
        if let error { event["error"] = error }
        onEvent(event)
    }

    /// Speech in and out, as a call: voice processing (echo cancellation),
    /// the loudspeaker unless something else is connected.
    private func configureAudio() {
        let session = RTCAudioSession.sharedInstance()
        session.lockForConfiguration()
        defer { session.unlockForConfiguration() }
        try? session.setCategory(.playAndRecord, mode: .voiceChat, options: [.defaultToSpeaker, .allowBluetoothHFP])
    }
}

// CallKit calls these on the main queue (`setDelegate(_:queue: nil)`);
// `@preconcurrency` checks that at run time.
extension CallManager: @preconcurrency CXProviderDelegate {
    func providerDidReset(_ provider: CXProvider) {
        call?.stop()
    }

    func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        guard let call = pending, uuid == action.callUUID else { return action.fail() }
        pending = nil
        configureAudio()
        provider.reportOutgoingCall(with: action.callUUID, startedConnectingAt: nil)
        action.fulfill()
        Task { await call.start() }
    }

    func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        if uuid == action.callUUID {
            let call = self.call
            finish(error: nil)
            call?.onEnd = { _ in }
            call?.stop()
        }
        action.fulfill()
    }

    func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        call?.setPaused(action.isMuted)
        onEvent(["kind": "paused", "paused": action.isMuted])
        action.fulfill()
    }

    /// A phone call came in and you took it: pause, as the mute button does.
    func provider(_ provider: CXProvider, perform action: CXSetHeldCallAction) {
        call?.setPaused(action.isOnHold)
        onEvent(["kind": "paused", "paused": action.isOnHold])
        action.fulfill()
    }

    func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        let rtc = RTCAudioSession.sharedInstance()
        rtc.audioSessionDidActivate(audioSession)
        rtc.isAudioEnabled = true
    }

    func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        let rtc = RTCAudioSession.sharedInstance()
        rtc.isAudioEnabled = false
        rtc.audioSessionDidDeactivate(audioSession)
    }
}
