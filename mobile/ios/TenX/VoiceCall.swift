import Foundation
import OSLog
@preconcurrency import WebRTC

/// Talk mode's log: `log stream --predicate 'subsystem == "it.luedeke.mobile.tenx"'`.
let voiceLog = Logger(subsystem: "it.luedeke.mobile.tenx", category: "voice")

/// Talk mode's audio, here instead of in the page: the WebRTC connection to
/// `tenx-voice` that web/src/lib/voice.ts makes in a browser, made the same
/// way — microphone on the first transceiver, a `chat` data channel with
/// RTVI's `client-ready` and a `ping: <ms>` every second, the offer and its
/// ICE candidates through `tenx web`'s `/voice/offer`. A web view's
/// microphone stops when the screen turns off; this one, inside a CallKit
/// call (`CallManager`), keeps going.
///
/// The page still draws the conversation: every data channel message goes to
/// it as it came (`onMessage`).
@MainActor
final class VoiceCall: NSObject {
    var onMessage: (String) -> Void = { _ in }
    var onConnected: () -> Void = {}
    /// Ended on its own (`error` set), or by `stop`.
    var onEnd: (String?) -> Void = { _ in }

    private let server: Server
    private var task: String?
    private let session: String?
    private let http: URLSession
    private var pc: RTCPeerConnection?
    private var dc: RTCDataChannel?
    private var mic: RTCAudioTrack?
    private var pcId: String?
    /// Candidates found before the answer named the connection.
    private var early: [RTCIceCandidate] = []
    private var ping: Timer?
    private var connected = false
    private var ended = false
    private(set) var paused = false

    /// One factory for every call. `RTCAudioSession.useManualAudio` is set
    /// before it exists: CallKit, not WebRTC, says when the audio may run.
    private static let factory: RTCPeerConnectionFactory = {
        RTCAudioSession.sharedInstance().useManualAudio = true
        RTCAudioSession.sharedInstance().isAudioEnabled = false
        return RTCPeerConnectionFactory()
    }()

    /// How long to wait for ICE candidates before sending the offer anyway
    /// (the rest follow with PATCH), as the page does.
    private static let gatherWait: Duration = .milliseconds(1500)

    init(server: Server, task: String?, session: String?) {
        self.server = server
        self.task = task
        self.session = session
        // Its own cookie jar: signed in with the token, like the web view.
        http = URLSession(configuration: .ephemeral)
        super.init()
    }

    func start() async {
        guard !ended else { return }
        do {
            try await signIn()
            try await connect()
        } catch {
            end(error.localizedDescription)
        }
    }

    /// The page now shows `task`: "it" means that one.
    func look(at task: String?) {
        guard task != self.task else { return }
        self.task = task
        guard let pcId, !ended else { return }
        post("/voice/context", ["pc_id": pcId, "task": task as Any])
    }

    /// Paused: the microphone sends silence, the voice is muted, and the
    /// program stops mid-sentence and keeps its news for later.
    func setPaused(_ paused: Bool) {
        guard paused != self.paused, !ended else { return }
        self.paused = paused
        mic?.isEnabled = !paused
        remote?.isEnabled = !paused
        if let pcId { post("/voice/pause", ["pc_id": pcId, "paused": paused]) }
    }

    func stop() { end(nil) }

    // MARK: - Setup

    /// `?token=` gets the cookie every `/voice` request needs.
    private func signIn() async throws {
        // `end` invalidates the session, and a task in an invalidated session
        // is an exception, not an error: ended, nothing more is asked.
        guard !ended else { throw CancellationError() }
        let (_, response) = try await http.data(from: server.signInURL)
        voiceLog.info("signed in: \((response as? HTTPURLResponse)?.statusCode ?? 0)")
        if let http = response as? HTTPURLResponse, http.statusCode == 401 {
            throw VoiceError("the server refused the token")
        }
    }

    private func connect() async throws {
        let config = RTCConfiguration()
        config.sdpSemantics = .unifiedPlan
        config.continualGatheringPolicy = .gatherOnce
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let pc = Self.factory.peerConnection(with: config, constraints: constraints, delegate: self) else {
            throw VoiceError("couldn't make the audio connection")
        }
        self.pc = pc
        let source = Self.factory.audioSource(with: constraints)
        let mic = Self.factory.audioTrack(with: source, trackId: "mic")
        self.mic = mic
        // The program expects the microphone on the first transceiver.
        let send = RTCRtpTransceiverInit()
        send.direction = .sendRecv
        send.streamIds = ["mic"]
        pc.addTransceiver(with: mic, init: send)
        let channel = RTCDataChannelConfiguration()
        channel.isOrdered = true
        dc = pc.dataChannel(forLabel: "chat", configuration: channel)
        dc?.delegate = self
        try await negotiate()
    }

    /// An offer, its answer: the first connection or a renegotiation.
    private func negotiate() async throws {
        guard let pc else { return }
        let offer = try await pc.offer(for: RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil))
        try await pc.setLocalDescription(offer)
        let deadline = ContinuousClock.now + Self.gatherWait
        while pc.iceGatheringState != .complete && ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(50))
        }
        guard let local = pc.localDescription, !ended else { return }
        let body: [String: Any] = [
            "sdp": local.sdp,
            "type": "offer",
            "pc_id": pcId as Any,
            "restart_pc": false,
            "task": task as Any,
            "session": session as Any,
        ]
        voiceLog.info("offer (\(self.pcId ?? "new", privacy: .public))")
        let data = try await request("POST", "/voice/offer", body)
        guard let answer = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let sdp = answer["sdp"] as? String, let id = answer["pc_id"] as? String
        else { throw VoiceError("tenx web answered something else") }
        guard !ended else { return }
        pcId = id
        voiceLog.info("answer for \(id, privacy: .public)")
        try await pc.setRemoteDescription(RTCSessionDescription(type: .answer, sdp: sdp))
        if !early.isEmpty {
            trickle(early)
            early.removeAll()
        }
    }

    private func trickle(_ candidates: [RTCIceCandidate]) {
        guard let pcId, !ended else { return }
        let list = candidates.map { ["candidate": $0.sdp, "sdp_mid": $0.sdpMid ?? "0", "sdp_mline_index": $0.sdpMLineIndex] }
        post("/voice/offer", ["pc_id": pcId, "candidates": list], method: "PATCH")
    }

    private var remote: RTCAudioTrack? {
        pc?.transceivers.first?.receiver.track as? RTCAudioTrack
    }

    // MARK: - HTTP

    /// A request as the page makes it: with the cookie, and from the page's
    /// origin, which `tenx web` checks.
    private func request(_ method: String, _ path: String, _ body: [String: Any]) async throws -> Data {
        guard !ended else { throw CancellationError() }
        var parts = URLComponents(url: server.base, resolvingAgainstBaseURL: false)!
        parts.path = path
        parts.query = nil
        var req = URLRequest(url: parts.url!)
        req.httpMethod = method
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(origin, forHTTPHeaderField: "Origin")
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        req.timeoutInterval = 40
        let (data, response) = try await http.data(for: req)
        guard let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status) else {
            let text = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            throw VoiceError(text.isEmpty ? "tenx web refused the call" : text)
        }
        return data
    }

    /// Fire and forget, as the page's own `.catch(() => {})`.
    private func post(_ path: String, _ body: [String: Any], method: String = "POST") {
        nonisolated(unsafe) let body = body
        Task { _ = try? await request(method, path, body) }
    }

    private var origin: String {
        var o = "\(server.base.scheme ?? "https")://\(server.base.host ?? "")"
        if let port = server.base.port { o += ":\(port)" }
        return o
    }

    // MARK: - Ending

    private func end(_ error: String?) {
        guard !ended else { return }
        voiceLog.info("call ended: \(error ?? "by request", privacy: .public)")
        ended = true
        ping?.invalidate()
        ping = nil
        dc?.close()
        pc?.close()
        http.invalidateAndCancel()
        onEnd(error)
    }

    private func opened() {
        guard let dc else { return }
        let ready: [String: Any] = [
            "label": "rtvi-ai", "type": "client-ready", "id": UUID().uuidString,
            "data": ["version": "1.0.0", "about": ["library": "tenx-ios"]],
        ]
        send(json: ready)
        ping = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            MainActor.assumeIsolated {
                guard dc.readyState == .open else { return }
                dc.sendData(RTCDataBuffer(data: Data("ping: \(Int(Date().timeIntervalSince1970 * 1000))".utf8), isBinary: false))
            }
        }
    }

    private func send(json: [String: Any]) {
        guard let dc, let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        dc.sendData(RTCDataBuffer(data: data, isBinary: false))
    }

    private func received(_ text: String) {
        // The program asks for a new offer over the channel.
        if let msg = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
           msg["type"] as? String == "signalling",
           (msg["message"] as? [String: Any])?["type"] as? String == "renegotiate" {
            Task {
                do { try await negotiate() } catch { end(error.localizedDescription) }
            }
            return
        }
        onMessage(text)
    }
}

// WebRTC calls these on its own threads; everything here runs on the main one.
extension VoiceCall: RTCPeerConnectionDelegate {
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didChange state: RTCPeerConnectionState) {
        Task { @MainActor in
            voiceLog.info("connection state \(state.rawValue)")
            switch state {
            case .connected:
                if !connected {
                    connected = true
                    onConnected()
                }
            case .failed: end("the audio connection failed")
            case .closed: end(nil)
            default: break
            }
        }
    }

    nonisolated func peerConnection(_ pc: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {
        Task { @MainActor in
            if pcId != nil { trickle([candidate]) } else { early.append(candidate) }
        }
    }

    nonisolated func peerConnection(_ pc: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    nonisolated func peerConnectionShouldNegotiate(_ pc: RTCPeerConnection) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didChange newState: RTCIceConnectionState) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    nonisolated func peerConnection(_ pc: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}
}

extension VoiceCall: RTCDataChannelDelegate {
    nonisolated func dataChannelDidChangeState(_ channel: RTCDataChannel) {
        let open = channel.readyState == .open
        Task { @MainActor in if open { opened() } }
    }

    nonisolated func dataChannel(_ channel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        guard !buffer.isBinary, let text = String(data: buffer.data, encoding: .utf8) else { return }
        Task { @MainActor in received(text) }
    }
}

struct VoiceError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}
