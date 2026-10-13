import Foundation

// The recording player's stream choice (docs/UX.md "Recordings", "Quality"):
// each camera of a playback plays its main or its sub-stream, and the user
// can change it during playback. Pure, so the rules have unit tests; the
// player only applies the changes (`StreamTier` through the core's tile tier
// call, which restarts that camera's play at the current position). Windows
// has the same type (`PlayerStreams` in NvrView.Shell) with the same tests.

/// What a player tile asks of its camera's stream.
public enum PlayerQuality: String, Sendable, CaseIterable, Equatable {
    /// The sub-stream in the grid and at 2x or faster; the main stream in a
    /// single large view at 1x or slower, and when zoomed past 2x.
    case auto
    /// The main stream.
    case high
    /// The sub-stream.
    case low

    /// The words the menu and the header use.
    public var title: String {
        switch self {
        case .auto: "Auto"
        case .high: "High"
        case .low: "Low"
        }
    }

    /// The menu's hint.
    public var detail: String {
        switch self {
        case .auto: "The sub-stream in the grid and at 2x or faster; the main stream alone at 1x or zoomed in"
        case .high: "The main stream"
        case .low: "The sub-stream"
        }
    }
}

/// Which stream each camera of the player plays.
public struct PlayerStreams: Equatable, Sendable {
    /// Auto's speed rule: from this speed up, the sub-stream (devices send
    /// keyframes only from here, so the main stream buys nothing).
    public static let sourceRate = 2.0

    private var choices: [RecordingCamera: PlayerQuality] = [:]
    /// Cameras Auto holds on the main stream because of their zoom
    /// (`ZoomHysteresis`: up at 2x, back down below 1.6x).
    private var zoomedHigh: Set<RecordingCamera> = []
    /// The tier last asked of the core for each camera (what it plays).
    public private(set) var applied: [RecordingCamera: StreamTier] = [:]

    public init() {}

    // MARK: Choices

    public func quality(_ camera: RecordingCamera) -> PlayerQuality { choices[camera] ?? .auto }

    public mutating func setQuality(_ quality: PlayerQuality, for camera: RecordingCamera) {
        choices[camera] = quality == .auto ? nil : quality
    }

    // MARK: The rule

    /// Whether the player shows `camera` alone, or as its only camera: the
    /// "single large view" of Auto.
    public static func isLarge(_ camera: RecordingCamera, in cameras: [RecordingCamera], mode: PlayerGridMode) -> Bool {
        guard let i = cameras.firstIndex(of: camera) else { return false }
        if cameras.count == 1 { return true }
        if case .single(let j) = PlayerGrid.normalized(mode, count: cameras.count) { return i == j }
        return false
    }

    /// Auto for one camera, given whether it was held high by its zoom.
    public static func autoTier(large: Bool, rate: Double, zoomHigh: Bool) -> StreamTier {
        if rate >= sourceRate { return .sub }
        return large || zoomHigh ? .main : .sub
    }

    /// The stream `camera` should play now. Updates the zoom memory.
    public mutating func wanted(
        _ camera: RecordingCamera, cameras: [RecordingCamera], mode: PlayerGridMode, rate: Double, zoomScale: Double
    ) -> StreamTier {
        switch quality(camera) {
        case .high: return .main
        case .low: return .sub
        case .auto:
            let high = ZoomHysteresis.next(high: zoomedHigh.contains(camera), scale: zoomScale)
            if high { zoomedHigh.insert(camera) } else { zoomedHigh.remove(camera) }
            return Self.autoTier(
                large: Self.isLarge(camera, in: cameras, mode: mode), rate: rate, zoomHigh: high)
        }
    }

    // MARK: Applying

    /// The tiers the playback opens with (nothing applied yet).
    public mutating func opening(
        _ cameras: [RecordingCamera], mode: PlayerGridMode, rate: Double, zoom: (RecordingCamera) -> Double
    ) -> [StreamTier] {
        cameras.map { c in
            let tier = wanted(c, cameras: cameras, mode: mode, rate: rate, zoomScale: zoom(c))
            applied[c] = tier
            return tier
        }
    }

    /// A camera that joined with this tier.
    public mutating func joined(_ camera: RecordingCamera, tier: StreamTier) { applied[camera] = tier }

    /// Cameras whose stream differs from what plays, with the stream they
    /// need; they count as applied (the caller sends them).
    public mutating func changes(
        _ cameras: [RecordingCamera], mode: PlayerGridMode, rate: Double, zoom: (RecordingCamera) -> Double
    ) -> [(camera: RecordingCamera, tier: StreamTier)] {
        var out: [(camera: RecordingCamera, tier: StreamTier)] = []
        for c in cameras {
            let tier = wanted(c, cameras: cameras, mode: mode, rate: rate, zoomScale: zoom(c))
            guard applied[c] != tier else { continue }
            applied[c] = tier
            out.append((c, tier))
        }
        return out
    }

    /// Keeps the cameras that are still in the player.
    public mutating func retain(_ cameras: [RecordingCamera]) {
        let keep = Set(cameras)
        choices = choices.filter { keep.contains($0.key) }
        zoomedHigh = zoomedHigh.filter(keep.contains)
        applied = applied.filter { keep.contains($0.key) }
    }

    /// What a tile header says about its stream: "main stream", "sub stream",
    /// with the choice in front when it is not Auto ("High · main stream").
    public func label(_ camera: RecordingCamera) -> String {
        let stream = (applied[camera] ?? .sub) == .main ? "main stream" : "sub stream"
        let q = quality(camera)
        return q == .auto ? stream : "\(q.title) · \(stream)"
    }
}
