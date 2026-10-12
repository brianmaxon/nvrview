import Foundation
import NVRCore

// Capture (minor 9) in the fake core (docs/FFI.md "Capture"), for the
// capture UI (M5) before it meets the real core. Nothing is written to disk:
// a recording is its events and its file name. The settings, the count
// limits (live, calls, two main-stream recorders per device), the disk
// guard at the start (`setCaptureFreeBytes`), the encoder's calls (`open`
// per encoded track and one frame each at the start, `close` with flush at
// the end, units counted through `recordAudioEncoded`), and the events
// (`waiting_keyframe`, `recording`, then `finalizing` and `finished` on a
// stop) are modeled. Events are queued by the calls and sent shortly after
// (or only on `completeCaptureWork()` with `Options.recordingWorkIsManual`),
// so none arrives before the call that caused it has returned. Not modeled:
// segments, gaps, caps, crash recovery, and ends other than a stop.

/// What the fake core keeps for capture, under one lock.
final class FakeCaptureState: @unchecked Sendable {
    struct Settings {
        var liveDir: String
        var maxLive: Int
        var maxCalls: Int
        var minFreeBytes: UInt64
        var main: Bool
        var shareSafe: Bool
    }

    struct Recording {
        var kind: String
        var device: UInt64
        var channel: UInt8
        var tile: UInt64
        var main: Bool
        var file: String
        var audio: String
        var tracks: [String]
        var encoder: (any CaptureEncoder)?
        /// Tracks opened with the encoder, and the units each got back.
        var encoded: [UInt32: Int]
        var started: Date
        var state: String
    }

    struct World {
        var settings: Settings?
        var encoder: (any CaptureEncoder)?
        var recordings: [UInt64: Recording] = [:]
        var freeBytes: UInt64 = .max
        var events: [String] = []
    }

    private let lock = NSLock()
    private var world = World()

    func locked<R, E: Error>(_ body: (inout World) throws(E) -> R) throws(E) -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&world)
    }
}

extension FakeCore {
    private static func captureJSON(_ object: [String: Any]) -> String {
        let data =
            (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes]))
            ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private static func recordingEvent(_ id: UInt64, _ r: FakeCaptureState.Recording, reason: String? = nil)
        -> [String: Any]
    {
        let ms = r.state == "waiting_keyframe" ? 0 : UInt64(max(0, Date().timeIntervalSince(r.started) * 1000))
        var o: [String: Any] = [
            "type": "recording", "recording": id, "kind": r.kind, "device": r.device, "channel": r.channel,
            "lens": 0, "tier": r.main ? "main" : "sub", "state": r.state, "file": r.file, "segment": 1,
            "bytes": ms * 100, "duration_ms": ms, "gaps": 0, "audio": r.audio, "tracks": r.tracks,
            "encode_dropped_ms": 0, "dropped_frames": 0,
        ]
        if let reason {
            o["reason"] = reason
            o["fragmented"] = false
        }
        return o
    }

    public func captureConfigure(json: String) throws(CoreStatus) {
        checkOutsideCallback("nv_capture_configure")
        if isFreed { throw .closed }
        guard let o = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any],
            let dir = o["live_dir"] as? String, dir.hasPrefix("/")
        else { throw .invalidArgument }
        func count(_ k: String, _ d: Int) throws(CoreStatus) -> Int {
            guard let v = o[k] else { return d }
            guard let n = v as? Int, n >= 0 else { throw .invalidArgument }
            return n
        }
        let quality = o["quality"] as? String ?? "main"
        guard quality == "main" || quality == "sub" else { throw .invalidArgument }
        let settings = FakeCaptureState.Settings(
            liveDir: dir, maxLive: try count("max_live", 4), maxCalls: try count("max_calls", 2),
            minFreeBytes: UInt64(try count("min_free_bytes", 2_000_000_000)), main: quality == "main",
            shareSafe: o["share_safe"] as? Bool ?? false)
        captureWorld.locked { $0.settings = settings }
    }

    public func captureSetEncoder(_ encoder: (any CaptureEncoder)?) throws(CoreStatus) {
        checkOutsideCallback("nv_capture_set_encoder")
        if isFreed { throw .closed }
        captureWorld.locked { $0.encoder = encoder }
    }

    /// Any thread, also inside the encoder's calls.
    public func recordAudioEncoded(recording: UInt64, track: UInt32, unit: UnsafeRawBufferPointer) -> CoreStatus {
        guard !unit.isEmpty, unit.count <= 8192 else { return .invalidArgument }
        return captureWorld.locked { w -> CoreStatus in
            guard w.recordings[recording]?.encoded[track] != nil else { return .unknownID }
            w.recordings[recording]?.encoded[track]! += 1
            return .ok
        }
    }

    public func recordStart(_ request: RecordRequest) throws(RecordStartError) -> RecordingStarted {
        checkOutsideCallback("nv_record_start")
        if isFreed { throw RecordStartError(.closed) }
        let tile: UInt64
        switch request {
        case .live(let t, let quality, _, _, let offset):
            guard quality == nil || quality == "main" || quality == "sub", abs(offset) <= 18 * 3600 else {
                throw RecordStartError(.invalidArgument)
            }
            tile = t
        case .call(let t, let file):
            guard file.hasPrefix("/"), file.hasSuffix(".mp4") else { throw RecordStartError(.invalidArgument) }
            tile = t
        }
        guard let source = captureSource(tile: tile) else { throw RecordStartError(.unknownID) }
        let id: UInt64
        do throws(CoreStatus) {
            id = try newID()
        } catch {
            throw RecordStartError(error)
        }
        let (r, encoder) = try captureWorld.locked { w throws(RecordStartError) in
            guard let s = w.settings else { throw RecordStartError(.notConfigured) }
            var r = FakeCaptureState.Recording(
                kind: "live", device: source.device, channel: source.channel, tile: tile, main: s.main, file: "",
                audio: source.hasAudio ? "passthrough" : "none", tracks: ["video", "audio"], encoder: nil,
                encoded: [:], started: Date(), state: "waiting_keyframe")
            let running = w.recordings.values
            switch request {
            case .live(_, let quality, let name, _, let offset):
                if running.filter({ $0.kind == "live" }).count >= s.maxLive {
                    throw RecordStartError(.limit, limit: "live")
                }
                if let quality { r.main = quality == "main" }
                let formatter = DateFormatter()
                formatter.locale = Locale(identifier: "en_US_POSIX")
                formatter.timeZone = TimeZone(secondsFromGMT: offset)
                formatter.dateFormat = "yyyy-MM-dd HH.mm.ss"
                r.file = "\(s.liveDir)/\(formatter.string(from: r.started)) \(name ?? "Camera \(source.channel + 1)").mp4"
                if !source.hasAudio { r.tracks = ["video"] }
            case .call(_, let file):
                if running.filter({ $0.kind == "call" }).count >= s.maxCalls
                    || running.contains(where: { $0.kind == "call" && $0.channel == source.channel && $0.device == source.device })
                {
                    throw RecordStartError(.limit, limit: "calls")
                }
                guard let e = w.encoder else { throw RecordStartError(.notConfigured) }
                r.kind = "call"
                r.file = file
                r.encoder = e
                r.tracks = ["video", "Conversation", "Visitor", "You"]
                if !source.hasAudio { r.audio = "encoded" }
            }
            if r.main && running.filter({ $0.device == source.device && $0.main }).count >= 2 {
                throw RecordStartError(.limit, limit: "main_streams")
            }
            if w.freeBytes < s.minFreeBytes { throw RecordStartError(.limit, limit: "disk_low") }
            w.recordings[id] = r
            w.events.append(Self.captureJSON(Self.recordingEvent(id, r)))
            r.state = "recording"
            w.recordings[id] = r
            w.events.append(Self.captureJSON(Self.recordingEvent(id, r)))
            return (r, r.encoder)
        }
        // The encoder is called without the lock (it answers through
        // recordAudioEncoded): the tracks open, and each gets a frame.
        if let encoder {
            let tracks: [UInt32] = r.audio == "passthrough" ? [0, 2] : [0, 1, 2]
            for t in tracks {
                guard
                    encoder.open(recording: id, track: t, sampleRate: 48_000, channels: 1, bitRate: 96_000)
                        != nil
                else { continue }
                captureWorld.locked { $0.recordings[id]?.encoded[t] = 0 }
                let silence = [Float](repeating: 0, count: 1024)
                silence.withUnsafeBufferPointer { encoder.encode(recording: id, track: t, pcm: $0) }
            }
        }
        scheduleCaptureWork()
        return RecordingStarted(recording: id, file: r.file, state: "waiting_keyframe")
    }

    public func recordStop(_ recording: UInt64) throws(CoreStatus) {
        checkOutsideCallback("nv_record_stop")
        if isFreed { throw .closed }
        guard let r = captureWorld.locked({ $0.recordings[recording] }) else { throw .unknownID }
        if let encoder = r.encoder {
            for t in r.encoded.keys.sorted() { encoder.close(recording: recording, track: t, flush: true) }
        }
        captureWorld.locked { w in
            guard var r = w.recordings.removeValue(forKey: recording) else { return }
            r.state = "finalizing"
            w.events.append(Self.captureJSON(Self.recordingEvent(recording, r)))
            r.state = "finished"
            w.events.append(Self.captureJSON(Self.recordingEvent(recording, r, reason: "stopped")))
        }
        scheduleCaptureWork()
    }

    public func recordStatus() throws(CoreStatus) -> String {
        if isFreed { throw .closed }
        return captureWorld.locked { w in
            let list = w.recordings.keys.sorted().map { Self.recordingEvent($0, w.recordings[$0]!) }
            return Self.captureJSON(["recordings": list])
        }
    }

    /// The free space the disk guard sees (default: plenty).
    public func setCaptureFreeBytes(_ bytes: UInt64) {
        captureWorld.locked { $0.freeBytes = bytes }
    }

    /// The units the encoder answered for each opened track of a recording
    /// that is still running (for tests).
    public func encodedUnits(recording: UInt64) -> [UInt32: Int] {
        captureWorld.locked { $0.recordings[recording]?.encoded ?? [:] }
    }

    private func scheduleCaptureWork() {
        guard !options.recordingWorkIsManual else { return }
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { [weak self] in
            self?.completeCaptureWork()
        }
    }

    /// Sends the queued `recording` events.
    public func completeCaptureWork() {
        guard !isFreed else { return }
        let events = captureWorld.locked { w -> [String] in
            defer { w.events.removeAll() }
            return w.events
        }
        for e in events { emit(event: e) }
    }
}
