import Foundation
import NVRCore

// Recorded playback (minor 6) in the fake core (docs/FFI-RECORDINGS.md,
// section 6), for the playback views (milestone P, M2) before they meet the
// real core. A playback's tiles are tiles like live ones: on an added
// device, each plays its channel's clip through the usual actions (one
// generation; the fake does not model a seek's new generation in the
// pictures). The shared clock is modeled exactly enough for a scrubber: the
// position runs at the rate on `Options.playbackClock` while playing and
// stops at the end (`ended`). Every enabled camera has recordings all day,
// as in `FakeRecordings.swift`. Events (`playback_state`, `playback_camera`,
// `playback_seek`, `playback_closed`) are queued by the calls and sent
// shortly after (or only on `completePlaybackWork()` with
// `Options.recordingWorkIsManual`), so none arrives before the call that
// caused it has returned; `tile_closed` is sent before
// `playbackRemoveCamera` and `playbackClose` return. Not modeled: device
// removal and login failures, `sync`'s waiting, and `no_recording`.

/// What the fake core keeps for playbacks, under one lock.
final class FakePlaybackState: @unchecked Sendable {
    struct Camera {
        var tile: UInt64
        var camera: RecordingCamera
        var tier: String
        var offsetMs: Int64
        /// `playing`, or `failed` (no added device).
        var state: String
        var reason: String?
    }

    struct Playback {
        var startMs: Int64
        var endMs: Int64
        /// The position at `anchor`.
        var positionMs: Int64
        var anchor: UInt64
        var rate: Double
        var sync: String
        var state: PlaybackState
        var cameras: [Camera]
        var generation: UInt32
    }

    struct World {
        var playbacks: [UInt64: Playback] = [:]
        var events: [String] = []
        var starts: [UInt64] = []
    }

    private let lock = NSLock()
    private var world = World()

    func locked<R>(_ body: (inout World) throws(CoreStatus) -> R) throws(CoreStatus) -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&world)
    }
}

extension FakeCore {
    static let playbackRates: [Double] = [0.25, 0.5, 1, 2, 4, 8, 16]
    static let maxPlaybacks = 8
    static let maxPlaybackCameras = 4
    static let maxPlaybackRangeMs: Int64 = 24 * 3600 * 1000

    private static func playbackJSON(_ object: [String: Any]) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private static func playbackObject(_ text: String) throws(CoreStatus) -> [String: Any] {
        guard let o = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] else {
            throw .invalidArgument
        }
        return o
    }

    private static func playbackCamera(_ any: Any?, defaultTier: String) throws(CoreStatus) -> FakePlaybackState.Camera {
        guard let o = any as? [String: Any], let c = o["camera"] as? [String: Any], let device = c["device"] as? String,
            !device.isEmpty, let channel = c["channel"] as? Int, (0...255).contains(channel)
        else { throw .invalidArgument }
        let lens = c["lens"] as? Int ?? 0
        let tier = o["tier"] as? String ?? defaultTier
        guard (0...1).contains(lens), tier == "sub" || tier == "main" else { throw .invalidArgument }
        let offset = (o["offset_ms"] as? NSNumber)?.int64Value ?? 0
        return FakePlaybackState.Camera(
            tile: 0, camera: RecordingCamera(device: device, channel: channel, lens: lens), tier: tier, offsetMs: offset,
            state: "playing")
    }

    private static func validRate(_ rate: Double) -> Bool { playbackRates.contains(rate) }

    /// The position now, settling an end reached meanwhile (`ended`, queued).
    private func settle(_ id: UInt64, _ p: inout FakePlaybackState.Playback, _ events: inout [String]) {
        let now = options.playbackClock()
        if p.state == .playing {
            let elapsed = Double(now &- p.anchor) / 1_000_000 * p.rate
            p.positionMs = min(p.endMs, p.positionMs + Int64(elapsed))
            if p.positionMs >= p.endMs {
                p.state = .ended
                events.append(Self.stateEvent(id, p))
            }
        }
        p.anchor = now
    }

    private static func stateEvent(_ id: UInt64, _ p: FakePlaybackState.Playback) -> String {
        playbackJSON([
            "type": "playback_state", "playback": id, "state": p.state.name, "position_ms": p.positionMs, "rate": p.rate,
        ])
    }

    private static func seekEvents(_ id: UInt64, _ p: FakePlaybackState.Playback) -> [String] {
        ["starting", "active"].map {
            playbackJSON([
                "type": "playback_seek", "playback": id, "position_ms": p.positionMs, "generation": p.generation,
                "state": $0,
            ])
        }
    }

    private static func cameraEvent(_ id: UInt64, _ c: FakePlaybackState.Camera) -> String {
        var o: [String: Any] = ["type": "playback_camera", "playback": id, "tile": c.tile, "state": c.state]
        if let r = c.reason { o["reason"] = r }
        return playbackJSON(o)
    }

    /// Runs `body` on one playback, queueing its events and sending them soon.
    private func withPlayback<R>(
        _ id: UInt64, _ body: (inout FakePlaybackState.Playback, inout [String]) throws(CoreStatus) -> R
    ) throws(CoreStatus) -> R {
        if isFreed { throw .closed }
        let r = try playbackWorld.locked { w throws(CoreStatus) -> R in
            guard var p = w.playbacks[id] else { throw .unknownID }
            var events: [String] = []
            settle(id, &p, &events)
            let r = try body(&p, &events)
            w.playbacks[id] = p
            w.events += events
            return r
        }
        schedulePlaybackWork()
        return r
    }

    /// Opens a camera's tile and reports it.
    private func openCamera(_ c: inout FakePlaybackState.Camera) throws(CoreStatus) {
        let (tile, streams) = try openPlaybackTile(
            deviceKey: c.camera.device, channel: UInt8(c.camera.channel), tier: c.tier == "main" ? .main : .sub)
        c.tile = tile
        if !streams {
            c.state = "failed"
            c.reason = "session_closed"
        }
    }

    private func checkCameras(_ cameras: [FakePlaybackState.Camera]) throws(CoreStatus) {
        try recordings.with { w throws(CoreStatus) in
            guard w.configured else { throw .notConfigured }
            for c in cameras where w.enabled[c.camera.device] == nil { throw .invalidArgument }
        }
    }

    // MARK: The calls

    public func playbackOpen(json: String) throws(CoreStatus) -> OpenedPlayback {
        checkOutsideCallback("nv_playback_open")
        if isFreed { throw .closed }
        let o = try Self.playbackObject(json)
        let defaultTier = o["tier"] as? String ?? "sub"
        var startMs: Int64
        var endMs: Int64
        var cameras: [FakePlaybackState.Camera] = []
        if let key = o["incident"] as? String {
            let answer = try Self.playbackObject(try incidentGet(key: key))
            guard let inc = answer["incident"] as? [String: Any], let start = (inc["start"] as? NSNumber)?.int64Value,
                let end = (inc["end"] as? NSNumber)?.int64Value, let list = inc["cameras"] as? [Any]
            else { throw .internal }
            let margin = (inc["margin_s"] as? NSNumber)?.int64Value ?? 0
            startMs = (start - margin) * 1000
            endMs = (end + margin + 1) * 1000
            for item in list {
                guard let c = item as? [String: Any] else { throw .internal }
                cameras.append(try Self.playbackCamera(["camera": c["camera"] as Any], defaultTier: defaultTier))
            }
        } else {
            guard let s = (o["start_ms"] as? NSNumber)?.int64Value, let e = (o["end_ms"] as? NSNumber)?.int64Value,
                let list = o["cameras"] as? [Any]
            else { throw .invalidArgument }
            startMs = s
            endMs = e
            for item in list { cameras.append(try Self.playbackCamera(item, defaultTier: defaultTier)) }
        }
        guard startMs >= 0, endMs > startMs, !cameras.isEmpty else { throw .invalidArgument }
        guard endMs - startMs <= Self.maxPlaybackRangeMs, cameras.count <= Self.maxPlaybackCameras else {
            throw .limit
        }
        for (i, c) in cameras.enumerated() where cameras[..<i].contains(where: { $0.camera == c.camera }) {
            throw .invalidArgument
        }
        let rate = (o["rate"] as? NSNumber)?.doubleValue ?? 1
        guard Self.validRate(rate) else { throw .invalidArgument }
        let sync = o["sync"] as? String ?? "wait"
        guard sync == "wait" || sync == "free" else { throw .invalidArgument }
        let position = min(max((o["position_ms"] as? NSNumber)?.int64Value ?? startMs, startMs), endMs)
        try checkCameras(cameras)
        if try playbackWorld.locked({ $0.playbacks.count }) >= Self.maxPlaybacks { throw .limit }

        let id = try newID()
        for i in cameras.indices { try openCamera(&cameras[i]) }
        let p = FakePlaybackState.Playback(
            startMs: startMs, endMs: endMs, positionMs: position, anchor: options.playbackClock(), rate: rate, sync: sync,
            state: .paused, cameras: cameras, generation: 1)
        try playbackWorld.locked { w throws(CoreStatus) in
            w.playbacks[id] = p
            w.events += Self.seekEvents(id, p) + cameras.map { Self.cameraEvent(id, $0) } + [Self.stateEvent(id, p)]
            w.starts += cameras.map(\.tile)
        }
        schedulePlaybackWork()
        return OpenedPlayback(playback: id, tiles: cameras.map(\.tile))
    }

    public func playbackAddCamera(_ playback: UInt64, json: String) throws(CoreStatus) -> UInt64 {
        checkOutsideCallback("nv_playback_add_camera")
        var c = try Self.playbackCamera(try Self.playbackObject(json), defaultTier: "sub")
        try checkCameras([c])
        try withPlayback(playback) { p, _ throws(CoreStatus) in
            if p.cameras.contains(where: { $0.camera == c.camera }) { throw .invalidArgument }
            if p.cameras.count >= Self.maxPlaybackCameras { throw .limit }
        }
        try openCamera(&c)
        let camera = c
        try withPlayback(playback) { p, events throws(CoreStatus) in
            p.cameras.append(camera)
            events.append(Self.cameraEvent(playback, camera))
        }
        try playbackWorld.locked { $0.starts.append(camera.tile) }
        return c.tile
    }

    public func playbackRemoveCamera(_ playback: UInt64, tile: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_remove_camera")
        try withPlayback(playback) { p, _ throws(CoreStatus) in
            guard let i = p.cameras.firstIndex(where: { $0.tile == tile }) else { throw .unknownID }
            p.cameras.remove(at: i)
        }
        closePlaybackTile(tile, reason: "closed")
    }

    public func playbackPlay(_ playback: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_play")
        try withPlayback(playback) { p, events in
            if p.state == .ended { p.positionMs = p.startMs }
            guard p.state != .playing else { return }
            p.state = .playing
            events.append(Self.stateEvent(playback, p))
        }
    }

    public func playbackPause(_ playback: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_pause")
        try withPlayback(playback) { p, events in
            guard p.state == .playing || p.state == .buffering else { return }
            p.state = .paused
            events.append(Self.stateEvent(playback, p))
        }
    }

    public func playbackSeek(_ playback: UInt64, positionMs: Int64, mode: SeekMode) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_seek")
        try withPlayback(playback) { p, events in
            p.positionMs = min(max(positionMs, p.startMs), p.endMs)
            if p.state == .ended { p.state = .paused }
            p.generation += 1
            events += Self.seekEvents(playback, p)
            events.append(Self.stateEvent(playback, p))
        }
    }

    public func playbackSetRate(_ playback: UInt64, rate: Double) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_set_rate")
        guard Self.validRate(rate) else { throw .invalidArgument }
        try withPlayback(playback) { p, events in
            p.rate = rate
            p.generation += 1
            events.append(Self.stateEvent(playback, p))
        }
    }

    public func playbackSetOffset(_ playback: UInt64, tile: UInt64, offsetMs: Int64) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_set_offset")
        try withPlayback(playback) { p, _ throws(CoreStatus) in
            guard let i = p.cameras.firstIndex(where: { $0.tile == tile }) else { throw .unknownID }
            guard abs(offsetMs) <= Self.maxPlaybackRangeMs else { throw .invalidArgument }
            p.cameras[i].offsetMs = offsetMs
        }
    }

    /// Any thread, also inside callbacks.
    public func playbackPosition(_ playback: UInt64) throws(CoreStatus) -> PlaybackPosition {
        try withPlayback(playback) { p, _ in PlaybackPosition(positionMs: p.positionMs, state: p.state) }
    }

    public func playbackStatus(_ playback: UInt64) throws(CoreStatus) -> String {
        checkOutsideCallback("nv_playback_status")
        return try withPlayback(playback) { p, _ in
            Self.playbackJSON([
                "playback": playback, "state": p.state.name, "position_ms": p.positionMs, "rate": p.rate,
                "sync": p.sync, "start_ms": p.startMs, "end_ms": p.endMs,
                "cameras": p.cameras.map { c -> [String: Any] in
                    var o: [String: Any] = [
                        "tile": c.tile,
                        "camera": ["device": c.camera.device, "channel": c.camera.channel, "lens": c.camera.lens],
                        "tier": c.tier, "offset_ms": c.offsetMs, "state": c.state, "generation": p.generation,
                    ]
                    if let r = c.reason { o["reason"] = r }
                    return o
                },
            ])
        }
    }

    public func playbackClose(_ playback: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_playback_close")
        if isFreed { throw .closed }
        // What the playback already reported goes out first.
        completePlaybackWork()
        let p = try playbackWorld.locked { w throws(CoreStatus) in
            guard let p = w.playbacks.removeValue(forKey: playback) else { throw .unknownID }
            return p
        }
        for c in p.cameras { closePlaybackTile(c.tile, reason: "closed") }
        emit(event: Self.playbackJSON(["type": "playback_closed", "playback": playback, "reason": "closed"]))
    }

    /// The playbacks open now (for tests).
    public var openPlaybacks: [UInt64] {
        ((try? playbackWorld.locked { Array($0.playbacks.keys) }) ?? []).sorted()
    }

    // MARK: Queued work

    private func schedulePlaybackWork() {
        guard !options.recordingWorkIsManual else { return }
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { [weak self] in
            self?.completePlaybackWork()
        }
    }

    /// Sends the queued playback events and starts the new tiles' streams.
    public func completePlaybackWork() {
        guard !isFreed else { return }
        let (events, starts) =
            (try? playbackWorld.locked { w -> ([String], [UInt64]) in
                defer {
                    w.events.removeAll()
                    w.starts.removeAll()
                }
                return (w.events, w.starts)
            }) ?? ([], [])
        for e in events { emit(event: e) }
        for t in starts { startPlaybackTile(t) }
    }
}
