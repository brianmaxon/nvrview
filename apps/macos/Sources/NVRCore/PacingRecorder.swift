import Foundation
import Synchronization

/// A pacing trace of one playback tile (docs/RENDERING.md, "Playback
/// pacing"): when each picture's Decode reached the app (the engine released
/// it), when the decoder answered it, and when a frame first drew it. Off
/// until `enabled` is set (the self-test's `playback-pacing` step); then each
/// stage keeps its newest `capacity` entries. The same measurement as the
/// Windows app's PacingTrace.
public final class PacingRecorder: Sendable {
    public static let capacity = 4096

    public struct Report: CustomStringConvertible, Sendable {
        public var released = PacingSummary()
        public var shown = PacingSummary()
        public var decodeP50Ms = 0.0
        public var decodeMaxMs = 0.0
        public var decoded = 0
        /// Pictures decoded but never drawn (replaced before a frame took them).
        public var skipped = 0

        public var description: String {
            "released: \(released); shown: \(shown); decode p50 \(String(format: "%.1f", decodeP50Ms)) max "
                + "\(String(format: "%.1f", decodeMaxMs)) ms; decoded \(decoded), skipped \(skipped)"
        }
    }

    private struct Stages {
        var arrived: [(at: Int64, pts: Int64)] = []
        var decoded: [(at: Int64, pts: Int64)] = []
        var shown: [(at: Int64, pts: Int64)] = []
    }

    private let on = Atomic<Bool>(false)
    private let stages = Mutex(Stages())

    public init() {}

    /// Whether the stages are recorded.
    public var enabled: Bool {
        get { on.load(ordering: .relaxed) }
        set { on.store(newValue, ordering: .relaxed) }
    }

    /// Microseconds of the host's uptime clock (the renderer's frame clock).
    public static func now() -> Int64 { Int64(DispatchTime.now().uptimeNanoseconds / 1000) }

    public func arrived(_ pts: Int64) { add(\.arrived, pts, Self.now()) }
    public func decoded(_ pts: Int64) { add(\.decoded, pts, Self.now()) }
    /// A frame at `at` (host µs) drew the picture `pts` for the first time.
    public func shown(_ pts: Int64, at: Int64) { add(\.shown, pts, at) }

    private func add(_ stage: WritableKeyPath<Stages, [(at: Int64, pts: Int64)]>, _ pts: Int64, _ at: Int64) {
        guard enabled else { return }
        stages.withLock { s in
            if s[keyPath: stage].count >= Self.capacity { s[keyPath: stage].removeFirst() }
            s[keyPath: stage].append((at, pts))
        }
    }

    public func reset() { stages.withLock { $0 = Stages() } }

    public func report() -> Report {
        let s = stages.withLock { $0 }
        var r = Report()
        r.released = PacingStats.of(atUs: s.arrived.map(\.at), ptsUs: s.arrived.map(\.pts))
        r.shown = PacingStats.of(atUs: s.shown.map(\.at), ptsUs: s.shown.map(\.pts))
        var arrivedAt: [Int64: Int64] = [:]
        for a in s.arrived { arrivedAt[a.pts] = a.at }
        let decode = s.decoded.compactMap { d in arrivedAt[d.pts].map { Double(d.at - $0) / 1000 } }.sorted()
        r.decodeP50Ms = PacingStats.percentile(decode, 50)
        r.decodeMaxMs = decode.last ?? 0
        r.decoded = s.decoded.count
        r.skipped = PacingStats.skipped(decodedPts: s.decoded.map(\.pts), shownPts: s.shown.map(\.pts))
        return r
    }
}
