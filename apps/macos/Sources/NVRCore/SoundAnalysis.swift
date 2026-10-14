import Foundation

/// Analyzing a recorded day's sound (`nv_sound_analyze_day`; docs/UX.md
/// "Recordings", "Sound"; FFI-RECORDINGS.md "Sound from downloaded days"):
/// the request, the state a camera-day is in as `sound_analyze` events
/// arrive, and the words for each outcome. The Windows app has the same
/// vectors in `SoundAnalysis`.
public enum SoundAnalysis {
    public enum State: String, Sendable, Equatable {
        case queued, running, done, failed, canceled
    }

    /// One camera-day's latest `sound_analyze` state.
    public struct Entry: Sendable, Equatable {
        public var request: UInt64
        public var state: State
        public var reason: String?
        public var seconds: Int?
        public var labels: Int?

        public init(request: UInt64, state: State, reason: String? = nil, seconds: Int? = nil, labels: Int? = nil) {
            self.request = request
            self.state = state
            self.reason = reason
            self.seconds = seconds
            self.labels = labels
        }

        /// Waiting or running: the lane shows progress and a cancel.
        public var active: Bool { state == .queued || state == .running }
    }

    /// `nv_sound_analyze_day`'s input for a camera and a day (any second of it).
    public static func query(camera: RecordingCamera, day: Int64) -> String {
        #"{"camera":\#(camera.json),"day":\#(day)}"#
    }

    /// `nv_sound_analyze_day`'s input for one span of a day (a loudness
    /// gap), inclusive device-local seconds inside the day.
    public static func query(camera: RecordingCamera, day: Int64, start: Int64, end: Int64) -> String {
        #"{"camera":\#(camera.json),"day":\#(day),"start":\#(start),"end":\#(end)}"#
    }

    /// Folds an event into a camera-day's entry. A late `queued` or
    /// `running` never takes back a finished state of the same request.
    public static func fold(
        _ old: Entry?, request: UInt64, state: State, reason: String?, seconds: Int?, labels: Int?
    ) -> Entry {
        if let old, old.request == request, !old.active, state == .queued || state == .running { return old }
        return Entry(request: request, state: state, reason: reason, seconds: seconds, labels: labels)
    }

    /// The words for a failed analysis.
    public static func failure(reason: String?) -> String {
        switch reason {
        case "no_audio": "Nothing downloaded for this day — download the day or an incident first"
        case "no_source": "Set a downloads folder in Settings"
        case "source_unavailable": "The downloaded video could not be read"
        case "not_enabled": "Choose this camera in \"Cameras to search\" first"
        case let r?: "Could not analyze the sound (\(r.replacingOccurrences(of: "_", with: " ")))"
        case nil: "Could not analyze the sound"
        }
    }

    /// The words for a call the core refused.
    public static func refusal(_ status: CoreStatus) -> String {
        switch status {
        case .notConfigured: failure(reason: "no_source")
        case .invalidArgument: failure(reason: "not_enabled")
        case .limit: "Too many sound analyses are waiting; try again in a moment"
        default: "Could not analyze the sound (status \(status.code))"
        }
    }

    /// What a lane's progress says (nil once the analysis is over).
    public static func progress(_ e: Entry) -> String? {
        switch e.state {
        case .queued: "Waiting to analyze sound"
        case .running: "Analyzing sound"
        default: nil
        }
    }

    /// The message a finished analysis leaves (nil while it is active).
    public static func outcome(_ e: Entry) -> String? {
        switch e.state {
        case .queued, .running: return nil
        case .failed: return failure(reason: e.reason)
        case .canceled: return "Sound analysis canceled"
        case .done:
            let seconds = e.seconds ?? 0, labels = e.labels ?? 0
            if seconds == 0 { return "No sound found in the downloaded video" }
            let found = labels == 0 ? "" : "; found \(labels) \(labels == 1 ? "sound" : "sounds")"
            return "Analyzed \(duration(seconds)) of sound" + found
        }
    }

    /// "45 s", "12 min", "3 h 5 min".
    public static func duration(_ seconds: Int) -> String {
        if seconds < 60 { return "\(seconds) s" }
        if seconds < 3600 { return "\(seconds / 60) min" }
        let m = (seconds % 3600) / 60
        return m == 0 ? "\(seconds / 3600) h" : "\(seconds / 3600) h \(m) min"
    }
}
