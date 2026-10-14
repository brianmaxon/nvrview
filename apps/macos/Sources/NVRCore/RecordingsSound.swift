import Foundation

// Sound in the Recordings window (docs/UX.md "Recordings", "Sound";
// FFI-RECORDINGS.md section 2a): the sound kinds and their names, the
// loudness lanes' answer and scale, and how many cameras one device may
// listen to. Views read these; unit tests pin them, with the same vectors
// as the Windows app's RecordingsSound.cs.

/// The sound kinds of section 2a (the classifier's labels and the core's
/// alarm detector map onto them; unknown kinds pass through).
public enum SoundKinds {
    /// The documented kinds, in display order.
    public static let all = [
        "smoke_alarm", "co_alarm", "siren", "car_alarm", "glass_break", "dog_bark", "baby_cry", "shout", "knock",
        "doorbell",
    ]

    /// The `source` of events from sound (the core's detector or the app's labels).
    public static let localAudio = "local_audio"

    /// Whether an event is from sound: its source says so, or it is one of
    /// the sound kinds.
    public static func isSound(kind: String, source: String?) -> Bool {
        source == localAudio || all.contains(kind)
    }

    public static func isSound(_ e: RecordedEvent) -> Bool { isSound(kind: e.kind, source: e.source) }

    /// "Smoke alarm", "Dog barking"; an unknown kind's own words.
    public static func name(_ kind: String) -> String {
        switch kind {
        case "smoke_alarm": "Smoke alarm"
        case "co_alarm": "CO alarm"
        case "siren": "Siren"
        case "car_alarm": "Car alarm"
        case "glass_break": "Glass breaking"
        case "dog_bark": "Dog barking"
        case "baby_cry": "Baby crying"
        case "shout": "Shouting"
        case "knock": "Knocking"
        case "doorbell": "Doorbell"
        default: kind.replacingOccurrences(of: "_", with: " ").capitalizedFirst
        }
    }

    /// The distinct sounds of these events, by name in first-seen order.
    public static func names(_ events: [RecordedEvent]) -> [String] {
        var out: [String] = []
        for e in events where isSound(e) {
            let n = name(e.kind)
            if !out.contains(n) { out.append(n) }
        }
        return out
    }
}

extension String {
    fileprivate var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

/// A run of empty loudness buckets and why they are empty, as the core
/// knows it (`gaps` of `nv_recordings_loudness`, minor 15): inclusive
/// device-local seconds; `reason` is `not_recorded`, `no_audio`, or
/// `not_analyzed`; `analyzable` means a finished download covers the gap, so
/// "Analyze this gap" can measure it now. The Windows app has the same
/// words in `LoudnessGap`.
public struct LoudnessGap: Sendable, Equatable, Decodable {
    public var start: Int64
    public var end: Int64
    public var reason: String
    public var analyzable: Bool

    public init(start: Int64, end: Int64, reason: String, analyzable: Bool = false) {
        self.start = start
        self.end = end
        self.reason = reason
        self.analyzable = analyzable
    }

    /// The short label drawn on the gap.
    public var label: String {
        switch reason {
        case "not_recorded": "Not recorded"
        case "no_audio": "No audio"
        case "not_analyzed": "Not analyzed yet"
        default: "No sound analyzed"
        }
    }

    /// What hovering the gap says.
    public var help: String {
        switch reason {
        case "not_recorded": "Not recorded: the device has no recording of this camera here"
        case "no_audio": "No audio: this camera's recording here has no sound"
        case "not_analyzed":
            analyzable
                ? "Not analyzed yet: the downloaded video here can be analyzed (right-click, Analyze this gap)"
                : "Not analyzed yet: download this time first, then analyze its sound"
        default: "No sound analyzed here"
        }
    }

    /// Whether the action "Analyze this gap" applies.
    public var canAnalyze: Bool { reason == "not_analyzed" && analyzable }
}

/// One camera's loudness lane (`nv_recordings_loudness`): whole dBFS per
/// bucket, nil where no audio was analyzed (not silence).
public struct LoudnessLane: Sendable, Equatable, Decodable {
    public var camera: RecordingCamera
    public var bucket: Int64
    /// Device-local second of the first bucket.
    public var start: Int64
    public var peak: [Int?]
    public var rms: [Int?]
    /// Why the empty stretches are empty (asked for with `gaps`).
    public var gaps: [LoudnessGap]

    private enum CodingKeys: String, CodingKey {
        case camera, start, gaps
        case bucket = "bucket_s"
        case peak = "peak_db"
        case rms = "rms_db"
    }

    public init(
        camera: RecordingCamera, bucket: Int64, start: Int64, peak: [Int?], rms: [Int?], gaps: [LoudnessGap] = []
    ) {
        self.camera = camera
        self.bucket = bucket
        self.start = start
        self.peak = peak
        self.rms = rms
        self.gaps = gaps
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(RecordingCamera.self, forKey: .camera)
        bucket = try c.decode(Int64.self, forKey: .bucket)
        start = try c.decode(Int64.self, forKey: .start)
        peak = try c.decode([Int?].self, forKey: .peak)
        rms = try c.decode([Int?].self, forKey: .rms)
        gaps = try c.decodeIfPresent([LoudnessGap].self, forKey: .gaps) ?? []
    }

    /// The gap that holds a device-local second.
    public func gap(at second: Int64) -> LoudnessGap? {
        gaps.first { $0.start <= second && second <= $0.end }
    }

    /// Whether any bucket has audio.
    public var heard: Bool { peak.contains { $0 != nil } }
}

/// `nv_recordings_loudness`.
public struct LoudnessAnswer: Sendable, Equatable, Decodable {
    public var revision: Int
    public var lanes: [LoudnessLane]
}

/// The loudness strips' arithmetic.
public enum LoudnessScale {
    /// The quietest level drawn (dBFS); quieter is drawn as this.
    public static let floorDB = -80
    /// Bucket sizes the window asks for, in seconds.
    public static let buckets: [Int64] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600]

    /// The bucket for lanes `width` points wide showing `visibleSeconds`:
    /// the smallest at least two points wide.
    public static func bucket(visibleSeconds: Double, width: Double) -> Int64 {
        let perPoint = visibleSeconds / max(width, 1)
        return buckets.first { Double($0) >= perPoint * 2 } ?? 3600
    }

    /// 0...1 for a level: 0 at `floorDB` and below, 1 at 0 dBFS.
    public static func level(_ db: Int) -> Double {
        min(max(Double(db - floorDB) / Double(-floorDB), 0), 1)
    }

    /// The camera whose loudness a lane shows: loudness belongs to a
    /// camera's first lens, since a microphone is not tied to a lens
    /// (FFI-RECORDINGS.md, C12 clarification 1).
    public static func microphone(of c: RecordingCamera) -> RecordingCamera {
        RecordingCamera(device: c.device, channel: c.channel, lens: 0)
    }

    /// The cameras to ask for, for these lanes (each microphone once, in order).
    public static func microphones(_ cameras: [RecordingCamera]) -> [RecordingCamera] {
        var out: [RecordingCamera] = []
        for c in cameras.map(microphone) where !out.contains(c) { out.append(c) }
        return out
    }

    /// `nv_recordings_loudness` for `cameras` over `[start, end]`, with the
    /// reasons for empty stretches when `gaps`.
    public static func query(
        start: Int64, end: Int64, bucket: Int64, cameras: [RecordingCamera], gaps: Bool = false
    ) -> String {
        let more = gaps ? #","gaps":true"# : ""
        return #"{"start":\#(start),"end":\#(end),"bucket_s":\#(bucket),"cameras":\#(RecordingsRequest.cameras(cameras))\#(more)}"#
    }
}

/// Which cameras listen for sounds (`nv_sound_watch`): each keeps its
/// sub-stream open with no tile, about 110 kbit/s from the device, and
/// counts toward the device's streams like a tile; a device has been seen
/// to carry six sub-streams on one session, and the Recordings player takes
/// up to four, so at most four cameras of a device listen.
public enum SoundListening {
    public static let maxPerDevice = 4

    /// Whether `camera` may be added to `list` (true when it is in it).
    public static func canAdd(_ camera: RecordingCamera, to list: [RecordingCamera]) -> Bool {
        list.contains(camera) || list.filter { $0.device == camera.device }.count < maxPerDevice
    }

    /// What a camera's `sound_watch` state says to the user.
    public static func describe(state: String, reason: String?) -> String {
        switch (state, reason) {
        case ("watching", _): "Listening"
        case ("stopped", _): "Not listening"
        case ("refused", "no_audio"): "No sound from this camera"
        case ("refused", "device_closed"): "The device is not connected"
        case ("refused", "sound_off"): "Sound analysis is off"
        case ("refused", _): "Could not listen"
        default: state
        }
    }
}
