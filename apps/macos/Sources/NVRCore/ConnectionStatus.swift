/// What the status line says, built from the core's events and a tile's
/// status. Pure, so it is tested without a window.
public struct ConnectionStatus: Sendable, Equatable {
    public var device: DeviceState?
    public var deviceName: String?
    public var deviceModel: String?
    public var tile: TileStatus?
    public var tileEnded = false
    /// The tile's latest `audio_stats`, if its stream has audio.
    public var audio: AudioStatsEvent?

    public init() {}

    /// Folds in one event (events for other devices or tiles are the
    /// caller's to filter).
    public mutating func apply(_ event: CoreEvent) {
        switch event {
        case .deviceState(_, let state):
            device = state
            if state == .closed { tile = nil }
        case .deviceInfo(_, let name, let model):
            deviceName = name
            deviceModel = model
        case .tileClosed:
            tileEnded = true
        case .audioStats(_, let stats):
            audio = stats
        case .channels, .tileStats, .tileTier, .tileError, .detection, .layoutsChanged, .timing, .talk, .ptz,
            .discoveryStarted, .discovered, .discoveryDone, .update, .recordings, .playback, .recording, .smartAlert:
            break
        }
    }

    public var isTerminal: Bool {
        if case .loginFailed = device { return true }
        return device == .closed
    }

    public var text: String {
        guard let device else { return "Starting" }
        let name = deviceName.map { " to \($0)" } ?? ""
        switch device {
        case .connecting:
            return "Connecting\(name)..."
        case .connected:
            var text = "Connected\(name)"
            if let tile {
                switch tile {
                case .waiting: text += ", waiting for video"
                case .playing: text += ", playing"
                case .recovering: text += ", recovering"
                case .undecodable: text += ", video cannot be decoded"
                }
            }
            if let audio, audio.audible {
                text += ", listening"
                if let d = audio.playoutDelayMilliseconds { text += String(format: " (audio %.0f ms behind)", d) }
            }
            if tileEnded { text += " (stream ended)" }
            return text
        case .backoff(let attempt, let retry):
            let seconds = Double(retry) / 1000
            let wait = seconds == seconds.rounded() ? "\(Int(seconds))" : String(format: "%.1f", seconds)
            return "Connection lost; attempt \(attempt) failed, retrying in \(wait) s"
        case .loginFailed(let status, let remain, let reason, let unlock):
            var text = "Login failed"
            if let reason { text += ": \(reason)" }
            if let status { text += " (status \(status))" }
            if let unlock, unlock > 0 {
                let minutes = (unlock + 59) / 60
                return text + "; the account is locked, try again in \(minutes) min. Not retrying."
            }
            if let remain { text += "; \(remain) attempt\(remain == 1 ? "" : "s") remaining before lockout" }
            return text + ". Not retrying; check the credentials."
        case .closed:
            return "Disconnected"
        case .other(let s):
            return "Device state: \(s)"
        }
    }
}
