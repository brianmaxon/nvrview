import CoreGraphics
import CoreMedia
import Foundation
import NVRCore
import Synchronization
import VideoKit

/// A fixture file as a stream of access units.
public struct FakeClip: Sendable {
    public let name: String
    public let codec: VideoCodec
    public let data: Data
    public let units: [AccessUnit]
    /// Fixtures with a known VUI color description set this; otherwise the
    /// stand-in's height rule applies.
    public var colorOverride: StandInColor?

    public init(name: String, codec: VideoCodec, data: Data, colorOverride: StandInColor? = nil) {
        self.name = name
        self.codec = codec
        self.data = data
        self.colorOverride = colorOverride
        units = AccessUnitSplitter.split(data, codec: codec)
    }

    /// Reads an Annex-B file; `.h265` and `.hevc` are H.265, anything else H.264.
    public static func load(_ url: URL, colorOverride: StandInColor? = nil) throws -> FakeClip {
        let ext = url.pathExtension.lowercased()
        return FakeClip(
            name: url.lastPathComponent, codec: ext == "h265" || ext == "hevc" ? .h265 : .h264,
            data: try Data(contentsOf: url), colorOverride: colorOverride)
    }
}

/// What a fake device reports about itself, chosen by the host it is added
/// with (`FakeCore.Options.devices`).
public struct FakeDeviceSpec: Sendable {
    public var name: String
    public var model: String
    public var channelCount: Int
    /// Channels whose camera detects objects itself (`detects` in the
    /// `channels` event); the others report only motion.
    public var detectingChannels: Set<Int>
    public var detects: [String]
    /// Channels whose camera has two lenses (`lenses` 2 in `channels`); the
    /// second lens is served on the same channel's `extern` and `mobile`.
    public var multiLensChannels: Set<Int>
    /// Channels whose camera sends no audio (their level reads 0).
    public var channelsWithoutAudio: Set<Int>

    public init(
        name: String = "NVR", model: String = "EXAMPLE-MODEL", channelCount: Int = 4,
        detectingChannels: Set<Int>? = nil, detects: [String] = ["people", "vehicle", "dog_cat"],
        multiLensChannels: Set<Int> = [], channelsWithoutAudio: Set<Int> = []
    ) {
        self.multiLensChannels = multiLensChannels
        self.channelsWithoutAudio = channelsWithoutAudio
        self.name = name
        self.model = model
        self.channelCount = channelCount
        self.detectingChannels = detectingChannels ?? Set((0..<channelCount).filter { $0 % 2 == 0 })
        self.detects = detects
    }
}

/// A Swift test double of the C core (docs/FFI.md, docs/FFI-ABI3.md). It
/// plays fixture files and emits actions and events exactly as the real core
/// does: Configure with parameter sets, color and rectangles, Decode with
/// borrowed Annex-B bytes, Flush, Reset, Clear and Status; serialized per
/// tile; events as JSON text. Its "pipeline" is the temporary stand-in
/// (gating, Configure on new parameter sets, Flush after a failure).
///
/// ABI 3, modeled exactly as the contract says:
/// - tier switches: `tileSetTier` starts a second pipeline (a new
///   `generation`, its pictures continuing the camera's timestamps), both
///   pipelines' actions flow until the new one's first Decode is answered,
///   then the old one ends with a final Reset and `tile_tier` `active` and a
///   `tier_switch` timing follow; a generation that never shows ends with
///   its own Reset and `failed` with a reason (superseded, start_rejected,
///   undecodable, timeout); asking for the tier already shown reports
///   `active` again;
/// - detection and motion events (`emitDetection`, and periodically in
///   real-time pacing with `detectionDemo`), `detects` in `channels`;
/// - `tileAudioLevel`; the layouts store (`FakeLayouts`) with
///   `layouts_changed`; the `first_picture` timing; several devices, each
///   described by `Options.devices`.
///
/// What the real core sends from its own threads in response to an answer
/// (final Resets, `tile_tier`, timings) is queued and sent at the start of
/// the tile's next pump, never from inside `tileDecoded`.
///
/// It also checks the app: every Decode not discarded by a later Flush, Reset
/// or Configure must be answered exactly once (per generation), nothing may
/// be answered that was not asked, close/remove/free must not be called from
/// inside a callback, and no callback may run after `free`. Problems collect
/// in `violations`.
///
/// Create one with `FakeCore.factory(options:onCreate:)` to hand to
/// `CoreBridge`, or directly with `init(options:sink:)`.
public final class FakeCore: CoreAPI {
    public enum Pacing: Sendable, Equatable {
        /// Nothing plays until `pump` is called; fully deterministic.
        case manual
        /// A thread per tile pushes one access unit every 1/fps seconds.
        case realtime(fps: Double)
    }

    public struct Options: Sendable {
        public var clips: [FakeClip]
        /// Clips for a tier other than the one a tile opened with (for
        /// example sharper ones for `main`); by default the tier switch
        /// plays the same clips.
        public var tierClips: [StreamTier: [FakeClip]] = [:]
        public var pacing = Pacing.manual
        /// Wrap around at the end of the clip (timestamps keep growing).
        public var loop = true
        /// When set, the device reports a failed login (with this many
        /// attempts remaining) instead of connecting, and its tiles stay silent.
        public var loginFailureRemaining: Int?
        /// Channels of a device whose host is not in `devices`.
        public var channelCount = 4
        /// What a device added with a given host reports.
        public var devices: [String: FakeDeviceSpec] = [:]
        /// Tier switches fail (`tile_tier` `failed`, reason `start_rejected`;
        /// the old tier keeps showing).
        public var tierSwitchFails = false
        /// A new tier that has decoded nothing after this long fails
        /// (reason `timeout`), as in the core.
        public var tierSwitchTimeout: Duration = .seconds(15)
        /// In real-time pacing, each tile's channel reports a detection (or,
        /// without camera AI, motion) every this many seconds, for 3 s.
        public var detectionDemo: Double?
        /// Where the layouts store keeps its file (nil: memory only).
        public var layoutsFile: URL?
        /// At most this many layouts (`nv_layout_put` then fails with limit).
        public var maxLayouts = 500

        public init(clips: [FakeClip]) { self.clips = clips }
    }

    public static func factory(options: Options, onCreate: @escaping @Sendable (FakeCore) -> Void = { _ in })
        -> CoreFactory
    {
        { sink in
            let core = FakeCore(options: options, sink: sink)
            onCreate(core)
            return core
        }
    }

    // MARK: State

    private struct Device {
        var failed: Bool
        var spec: FakeDeviceSpec
        var key: String
        var tiles: [UInt64]
    }

    private struct World {
        var nextID: UInt64 = 1
        var devices: [UInt64: Device] = [:]
        var tiles: [UInt64: FakeTile] = [:]
        var freed = false
    }

    fileprivate let options: Options
    private let sink: any CoreSink
    private let world = Mutex(World())
    private let eventLock = NSLock()
    private let problems = Mutex<[String]>([])
    private let late = Atomic<Int>(0)
    private let afterFree = Atomic<Int>(0)
    /// The fake's audio renderer (see `FakeAudio`).
    public let audio = FakeAudio()
    private let audioTiles = Mutex<[UInt64: FakeTileAudio]>([:])
    private let levels = Mutex<[UInt64: AudioLevel]>([:])
    private let actionLog = Mutex<[UInt64: [String]]>([:])
    private let tapLock = NSLock()
    private let output = Mutex<(sampleRate: UInt32, channels: UInt32)?>(nil)
    private let layouts: FakeLayouts
    fileprivate let createdAt = HostClock.nowNanos()
    private let firstPicture = Atomic<Bool>(false)

    public init(options: Options, sink: any CoreSink) {
        self.options = options
        self.sink = sink
        layouts = FakeLayouts(file: options.layoutsFile, limit: options.maxLayouts)
    }

    /// Everything the app did wrong so far (empty when all is well).
    public var violations: [String] { problems.withLock { $0 } }
    /// Answers that arrived for a tile (or generation) already closed (legal, counted).
    public var lateAnswers: Int { late.load(ordering: .relaxed) }
    /// Answers that arrived after `free` (a bridge bug).
    public var answersAfterFree: Int { afterFree.load(ordering: .relaxed) }

    fileprivate func violation(_ text: String) { problems.withLock { $0.append(text) } }
    fileprivate func countLate() { late.add(1, ordering: .relaxed) }

    // MARK: Callback bookkeeping

    private static let inCallbackKey = "nvrview.fakecore.in-callback"

    private static var inCallback: Bool {
        Thread.current.threadDictionary[inCallbackKey] != nil
    }

    private func insideCallback<R>(_ body: () -> R) -> R {
        let dict = Thread.current.threadDictionary
        let outer = dict[Self.inCallbackKey]
        dict[Self.inCallbackKey] = true
        defer { dict[Self.inCallbackKey] = outer }
        return body()
    }

    private func requireOutsideCallback(_ what: String) {
        if Self.inCallback { violation("\(what) called from inside a callback") }
    }

    private func emitEvent(_ json: String) {
        if world.withLock({ $0.freed }) {
            violation("event emitted after free")
            return
        }
        eventLock.lock()
        defer { eventLock.unlock() }
        var text = json
        text.withUTF8 { bytes in
            insideCallback { sink.core(didEmit: UnsafeRawBufferPointer(bytes)) }
        }
    }

    // MARK: CoreAPI

    public func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64 {
        requireOutsideCallback("nv_device_add")
        guard !host.isEmpty, !user.isEmpty else { throw .invalidArgument }
        let failed = options.loginFailureRemaining != nil
        let spec = options.devices[host] ?? FakeDeviceSpec(channelCount: options.channelCount)
        let id: UInt64 = try world.withLock { w throws(CoreStatus) in
            if w.freed { throw .closed }
            let id = w.nextID
            w.nextID += 1
            w.devices[id] = Device(failed: failed, spec: spec, key: Self.key(host: host, port: port), tiles: [])
            return id
        }
        emitEvent(#"{"type":"device_state","device":\#(id),"state":"connecting"}"#)
        if let remaining = options.loginFailureRemaining {
            emitEvent(#"{"type":"device_state","device":\#(id),"state":"login_failed","status":401,"remain_times":\#(remaining)}"#)
        } else {
            emitEvent(#"{"type":"device_state","device":\#(id),"state":"connected","future_field":1}"#)
            emitEvent(
                #"{"type":"device_info","device":\#(id),"name":"\#(spec.name)","model":"\#(spec.model)","key":"\#(Self.key(host: host, port: port))"}"#)
            let channels = (0..<spec.channelCount).map { c in
                let detects = spec.detectingChannels.contains(c) ? spec.detects.map { "\"\($0)\"" }.joined(separator: ",") : ""
                // Like the devices observed: a multi-lens camera advertises
                // only main and sub, and says how many lenses it has.
                let lenses = spec.multiLensChannels.contains(c) ? #","lenses":2"# : ""
                return #"{"channel":\#(c),"connected":true,"tiers":["main","sub"],"name":"Channel \#(c + 1)","model":"EXAMPLE-CAM","detects":[\#(detects)]\#(lenses)}"#
            }.joined(separator: ",")
            emitEvent(#"{"type":"channels","device":\#(id),"channels":[\#(channels)]}"#)
        }
        return id
    }

    public func removeDevice(_ device: UInt64) throws(CoreStatus) {
        requireOutsideCallback("nv_device_remove")
        let tiles: [UInt64] = try world.withLock { w throws(CoreStatus) in
            guard let d = w.devices.removeValue(forKey: device) else { throw .unknownID }
            return d.tiles
        }
        for tile in tiles {
            if let t = detach(tile) {
                t.stop()
                emitEvent(#"{"type":"tile_closed","tile":\#(tile),"reason":"device_removed"}"#)
            }
        }
        emitEvent(#"{"type":"device_state","device":\#(device),"state":"closed"}"#)
    }

    public func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> UInt64 {
        requireOutsideCallback("nv_tile_open")
        guard !options.clips.isEmpty else { throw .invalidArgument }
        let (id, tile): (UInt64, FakeTile) = try world.withLock { w throws(CoreStatus) in
            if w.freed { throw .closed }
            guard var d = w.devices[device] else { throw .unknownID }
            let id = w.nextID
            w.nextID += 1
            let tile = FakeTile(
                id: id, device: device, channel: channel, tier: tier, detects: d.spec.detectingChannels.contains(Int(channel))
                    ? d.spec.detects : [], silent: d.failed,
                hasAudio: !d.failed && !d.spec.channelsWithoutAudio.contains(Int(channel)), owner: self)
            d.tiles.append(id)
            w.devices[device] = d
            w.tiles[id] = tile
            return (id, tile)
        }
        if case .realtime(let fps) = options.pacing, !tile.silent { tile.start(fps: fps) }
        return id
    }

    public func closeTile(_ tile: UInt64) throws(CoreStatus) {
        requireOutsideCallback("nv_tile_close")
        guard let t = detach(tile) else { throw .unknownID }
        // Joins the playback thread and waits out any pump: no action for the
        // tile runs after this returns.
        t.stop()
    }

    public func tileDecoded(_ tile: UInt64, ptsMicros: Int64) -> CoreStatus {
        tileDecoded(tile, generation: 0, ptsMicros: ptsMicros)
    }

    public func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus {
        tileDecodeFailed(tile, generation: 0, kind: kind)
    }

    public func tileDecoded(_ tile: UInt64, generation: UInt32, ptsMicros: Int64) -> CoreStatus {
        guard let t = answerTarget(tile) else { return .unknownID }
        t.answered(.decoded(ptsMicros), generation: generation)
        return .ok
    }

    public func tileDecodeFailed(_ tile: UInt64, generation: UInt32, kind: CoreDecodeFailure) -> CoreStatus {
        guard let t = answerTarget(tile) else { return .unknownID }
        t.answered(.failed(kind), generation: generation)
        return .ok
    }

    public func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
        throws(CoreStatus) -> [TileLayout]
    {
        FakeLayout.grid(viewport: viewport, gap: gap, pictureSizes: pictureSizes, fill: fill)
    }

    public func free() {
        requireOutsideCallback("nv_core_free")
        let tiles = world.withLock { w -> [FakeTile] in
            if w.freed { return [] }
            w.freed = true
            defer {
                w.tiles.removeAll()
                w.devices.removeAll()
            }
            return Array(w.tiles.values)
        }
        for t in tiles { t.stop() }
    }

    // MARK: ABI 3: device keys

    /// The stable key the core gives a device address (case-insensitive
    /// host, and port): `d-` and 16 hex digits, the same in every run.
    public static func key(host: String, port: UInt16) -> String {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325  // FNV-1a
        for byte in "\(host.lowercased()):\(port)".utf8 {
            hash ^= UInt64(byte)
            hash &*= 0x0000_0100_0000_01b3
        }
        let hex = String(hash, radix: 16)
        return "d-" + String(repeating: "0", count: 16 - hex.count) + hex
    }

    public func deviceKey(_ device: UInt64) throws(CoreStatus) -> String {
        guard let d = world.withLock({ $0.devices[device] }) else { throw .unknownID }
        return d.key
    }

    // MARK: ABI 3: tiers

    public func tileSetTier(_ tile: UInt64, tier: StreamTier) throws(CoreStatus) {
        requireOutsideCallback("nv_tile_set_tier")
        guard let t = world.withLock({ $0.tiles[tile] }) else { throw .unknownID }
        for json in t.setTier(tier) { emitEvent(json) }
    }

    /// The clip a channel plays at a tier.
    fileprivate func clip(channel: UInt8, tier: StreamTier, opened: StreamTier) -> FakeClip {
        let list = tier == opened ? options.clips : (options.tierClips[tier] ?? options.clips)
        return list[Int(channel) % list.count]
    }

    /// Whether this is the core's first decoded picture (for `first_picture`).
    fileprivate func claimFirstPicture() -> Bool {
        !firstPicture.exchange(true, ordering: .acquiringAndReleasing)
    }

    // MARK: ABI 3: detections

    /// Emits a `detection` event (or `motion`, for `kind` "motion") as the
    /// core does on a device's state push.
    /// With `alarm`, an `alarm` event (such as `visitor`, a doorbell press);
    /// `lens` 1 marks the second lens of a multi-lens camera.
    public func emitDetection(
        device: UInt64, channel: Int, kind: String, start: Bool, source: String = "device", lens: Int? = nil,
        alarm: Bool = false
    ) {
        let at = HostClock.nowNanos()
        let state = start ? "start" : "stop"
        let lensField = lens.map { #","lens":\#($0)"# } ?? ""
        if kind == DetectionEvent.motionKind {
            emitEvent(
                #"{"type":"motion","device":\#(device),"channel":\#(channel)\#(lensField),"state":"\#(state)","at_ns":\#(at)}"#)
        } else {
            let type = alarm ? "alarm" : "detection"
            emitEvent(
                #"{"type":"\#(type)","device":\#(device),"channel":\#(channel)\#(lensField),"kind":"\#(kind)","state":"\#(state)","source":"\#(source)","at_ns":\#(at)}"#)
        }
    }

    fileprivate func event(_ json: String) { emitEvent(json) }

    // MARK: ABI 3: layouts

    public func layoutsGet() throws(CoreStatus) -> String {
        if world.withLock({ $0.freed }) { throw .closed }
        return layouts.all()
    }

    public func layoutPut(json: String) throws(CoreStatus) {
        requireOutsideCallback("nv_layout_put")
        if world.withLock({ $0.freed }) { throw .closed }
        let revision = try layouts.put(json)
        emitEvent(#"{"type":"layouts_changed","revision":\#(revision)}"#)
    }

    public func layoutDelete(id: String) throws(CoreStatus) {
        requireOutsideCallback("nv_layout_delete")
        if world.withLock({ $0.freed }) { throw .closed }
        let revision = try layouts.delete(id)
        emitEvent(#"{"type":"layouts_changed","revision":\#(revision)}"#)
    }

    // MARK: Audio

    public func audioSetOutput(sampleRate: UInt32, channels: UInt32) throws(CoreStatus) {
        requireOutsideCallback("nv_audio_set_output")
        guard (8_000...384_000).contains(sampleRate), (1...2).contains(channels) else { throw .invalidArgument }
        output.withLock { $0 = (sampleRate, channels) }
        audio.channels.store(channels, ordering: .releasing)
        audio.sampleRate.store(sampleRate, ordering: .releasing)
    }

    public func tileAudioSet(_ tile: UInt64, audible: Bool, volume: Float) throws(CoreStatus) {
        requireOutsideCallback("nv_tile_audio_set")
        guard volume.isFinite, volume >= 0 else { throw .invalidArgument }
        guard world.withLock({ $0.tiles[tile] }) != nil else { throw .unknownID }
        let gain = audioTiles.withLock { t -> Float in
            t[tile, default: FakeTileAudio()].audible = audible
            t[tile, default: FakeTileAudio()].volume = volume
            return t.values.filter(\.audible).map(\.volume).max() ?? 0
        }
        audio.gainBits.store(gain.bitPattern, ordering: .relaxed)
    }

    public func tileAudioTap(_ tile: UInt64, enabled: Bool) throws(CoreStatus) {
        requireOutsideCallback("nv_tile_audio_tap")
        guard world.withLock({ $0.tiles[tile] }) != nil else { throw .unknownID }
        // Like the real core: no block for the tile after this returns.
        tapLock.withLock {
            audioTiles.withLock { $0[tile, default: FakeTileAudio()].tapped = enabled }
        }
    }

    /// ABI 3 `nv_tile_audio_level`: the level of the tile's decoded audio,
    /// before volume, whether the tile is audible or not (the core measures
    /// muted tiles as their audio is decoded, audible ones as they are
    /// mixed); 0 for a tile with no audio or not known (never an error). It
    /// reads a level set with `setAudioLevel`, or else a slowly varying
    /// synthetic one.
    public func tileAudioLevel(_ tile: UInt64) throws(CoreStatus) -> AudioLevel {
        guard let known = world.withLock({ $0.tiles[tile] }), known.hasAudio else { return .silent }
        if let level = levels.withLock({ $0[tile] }) { return level }
        let t = Double(HostClock.nowNanos()) / 1e9
        let peak = Float(0.35 + 0.3 * sin(t * 5.3) * sin(t * 1.7))
        return AudioLevel(peak: max(peak, 0.02), rms: max(peak, 0.02) * 0.6)
    }

    /// Fixes the level `tileAudioLevel` reports for a tile with audio.
    public func setAudioLevel(tile: UInt64, _ level: AudioLevel?) {
        levels.withLock { $0[tile] = level }
    }

    public var audioRenderHook: AudioRenderHook { audio.hook }

    /// The format the app last set with `audioSetOutput`.
    public var audioOutput: (sampleRate: UInt32, channels: UInt32)? { output.withLock { $0 } }

    /// Emits `count` tap blocks (1024 samples of a 16 kHz tone, camera time
    /// following the sample count) to a tapped tile, on the calling thread.
    /// Returns how many were emitted (0 when the tile is not tapped).
    @discardableResult
    public func pumpAudio(tile: UInt64, count: Int = 1) -> Int {
        var emitted = 0
        var samples = [Float](repeating: 0, count: 1024)
        for _ in 0..<count {
            let sent = tapLock.withLock { () -> Bool in
                guard let first = audioTiles.withLock({ t -> UInt64? in
                    guard t[tile]?.tapped == true else { return nil }
                    defer { t[tile]!.blocks += 1 }
                    return t[tile]!.blocks * 1024
                }) else { return false }
                for i in 0..<samples.count {
                    samples[i] = 0.25 * Float(sin(Double(first + UInt64(i)) * 2 * Double.pi * 440 / 16_000))
                }
                let wall = Int64(Date().timeIntervalSince1970 * 1e6)
                samples.withUnsafeBufferPointer { buf in
                    let block = CoreAudioBlock(
                        samples: buf, epoch: 0, firstSample: first, cameraTimeMicros: Int64(first) * 1_000_000 / 16_000,
                        wallClockMicros: wall, discontinuity: first == 0)
                    insideCallback { sink.core(didTap: block, tile: tile) }
                }
                return true
            }
            if !sent { break }
            emitted += 1
        }
        return emitted
    }

    /// The `audio_stats` event a real core would send for this tile, if it
    /// has audio settings.
    fileprivate func audioStatsJSON(tile: UInt64) -> String? {
        guard let a = audioTiles.withLock({ $0[tile] }) else { return nil }
        return #"{"type":"audio_stats","tile":\#(tile),"records":\#(a.blocks),"decoded":\#(a.blocks),"audible":\#(a.audible),"volume":\#(a.volume),"tapped":\#(a.tapped),"codec":"aac","playout_delay_ms":100,"future_field":1}"#
    }

    // MARK: Test hooks

    /// Pushes up to `count` access units of the tile's clip through each of
    /// its pipelines (two while a tier switch runs) and emits the resulting
    /// actions on the calling thread, after any queued events. Returns how
    /// many steps were pushed (0 for an unknown tile). Manual pacing only;
    /// do not call it concurrently for one tile.
    @discardableResult
    public func pump(tile: UInt64, count: Int = 1) -> Int {
        guard let t = world.withLock({ $0.tiles[tile] }) else { return 0 }
        return t.pump(count: count)
    }

    /// Sends the events queued for every tile (normally sent by its next
    /// pump) now, on the calling thread.
    public func flushEvents() {
        for t in world.withLock({ Array($0.tiles.values) }) { t.flushEvents() }
    }

    /// Emits one hand-made action to the tile, bypassing the pipeline (for
    /// failure injection), for the shown generation unless one is given. The
    /// answering check applies to it too.
    public func inject(tile: UInt64, _ action: FakeAction, generation: UInt32? = nil) {
        guard let t = world.withLock({ $0.tiles[tile] }) else { return }
        t.inject(action, generation: generation)
    }

    /// Emits a raw event string to the sink.
    public func emit(event json: String) { emitEvent(json) }

    /// Waits until every Decode on every tile has been answered. False on timeout.
    public func waitUntilAnswered(timeout: Duration = .seconds(10)) async -> Bool {
        let clock = ContinuousClock()
        let deadline = clock.now + timeout
        while clock.now < deadline {
            let tiles = world.withLock { Array($0.tiles.values) }
            if tiles.allSatisfy({ $0.outstanding == 0 }) { return true }
            try? await clock.sleep(for: .milliseconds(5))
        }
        return false
    }

    /// The decode answers a tile has received so far (every generation).
    public func answers(tile: UInt64) -> FakeAnswers? {
        world.withLock { $0.tiles[tile] }?.answerLog
    }

    /// Access units the shown pipeline gated (not decoded) for a tile.
    public func gated(tile: UInt64) -> Int { world.withLock { $0.tiles[tile] }?.gated ?? 0 }

    /// The generations a tile's pipelines run now (shown first), and the shown one.
    public func generations(tile: UInt64) -> (shown: UInt32, running: [UInt32])? {
        world.withLock { $0.tiles[tile] }?.generations
    }

    /// The last actions sent to a tile (up to 256), as `kind@generation`.
    public func lastActions(tile: UInt64) -> [String] { actionLog.withLock { $0[tile] ?? [] } }

    /// Sends a tile's queued final Resets but not its queued events (to see
    /// the action path decide a switch before the event confirms it).
    public func deliverFinalResetsOnly(tile: UInt64) {
        world.withLock { $0.tiles[tile] }?.deliverFinalResets()
    }

    /// The tier a tile shows now.
    public func shownTier(tile: UInt64) -> StreamTier? { world.withLock { $0.tiles[tile] }?.shownTier }

    // MARK: Internals

    private func detach(_ id: UInt64) -> FakeTile? {
        tapLock.withLock { _ = audioTiles.withLock { $0.removeValue(forKey: id) } }
        audio.gainBits.store(
            audioTiles.withLock { $0.values.filter(\.audible).map(\.volume).max() ?? 0 }.bitPattern,
            ordering: .relaxed)
        return world.withLock { w in
            for (device, var d) in w.devices where d.tiles.contains(id) {
                d.tiles.removeAll { $0 == id }
                w.devices[device] = d
            }
            return w.tiles.removeValue(forKey: id)
        }
    }

    private func answerTarget(_ id: UInt64) -> FakeTile? {
        let (tile, freed) = world.withLock { ($0.tiles[id], $0.freed) }
        if freed {
            afterFree.add(1, ordering: .relaxed)
            return nil
        }
        if tile == nil { late.add(1, ordering: .relaxed) }
        return tile
    }

    fileprivate func deliver(_ action: CoreAction, tile: UInt64, generation: UInt32) {
        let name: String
        switch action {
        case .configure: name = "configure"
        case .decode: name = "decode"
        case .flush: name = "flush"
        case .reset: name = "reset"
        case .clear: name = "clear"
        case .status: name = "status"
        }
        actionLog.withLock { log in
            log[tile, default: []].append("\(name)@\(generation)")
            if log[tile]!.count > 256 { log[tile]!.removeFirst(log[tile]!.count - 256) }
        }
        insideCallback { sink.core(didDeliver: action, tile: tile, generation: generation) }
    }
}

/// A hand-made action for `FakeCore.inject`.
public enum FakeAction: Sendable {
    /// `size` is the coded size (nil: from the parameter sets); `visible` the
    /// visible rectangle (nil: the whole picture).
    case configure(
        codec: VideoCodec, parameterSets: [Data], color: StandInColor, size: PixelSize? = nil,
        visible: PixelRect? = nil)
    case decode(Data, keyframe: Bool, ptsMicros: Int64)
    case flush
    case reset
    case clear
    case status(TileStatus)
}

/// What the app answered for a tile.
public struct FakeAnswers: Sendable, Equatable {
    public var decodedPTS: [Int64] = []
    public var failures: [CoreDecodeFailure] = []
    /// The generation of each answer, in order (decoded and failed together).
    public var generations: [UInt32] = []
}

private enum Answer: Sendable {
    case decoded(Int64)
    case failed(CoreDecodeFailure)
}

// MARK: One tile

private final class FakeTile: Sendable {
    let id: UInt64
    let device: UInt64
    let channel: UInt8
    let openedTier: StreamTier
    let detects: [String]
    let silent: Bool
    /// The camera sends audio.
    let hasAudio: Bool
    private unowned let owner: FakeCore

    /// One pipeline: the tile has one, and two while a tier switch runs.
    private struct Lane {
        let generation: UInt32
        let tier: StreamTier
        let clip: FakeClip
        var pipeline: StandInPipeline
        var cursor: Int
        /// Decodes asked and not yet answered or discarded.
        var outstanding = 0
        var submitted = Set<Int64>()
        var configurePending = false
        var pendingFailures = 0
        var status: TileStatus?
        var answeredDecode = false
        var startedAt = HostClock.nowNanos()

        init(generation: UInt32, tier: StreamTier, clip: FakeClip, cursor: Int) {
            self.generation = generation
            self.tier = tier
            self.clip = clip
            self.cursor = cursor
            pipeline = StandInPipeline(codec: clip.codec, colorOverride: clip.colorOverride)
        }
    }

    private struct State {
        var lanes: [Lane]
        var shown: UInt32 = 0
        var nextGeneration: UInt32 = 1
        /// Generations that ran and were stopped (answers for them are late).
        var retired = Set<UInt32>()
        var answers = FakeAnswers()
        var stopped = false
        var running = false
        /// Events to send at the next pump (the core's own thread would
        /// send them; never from inside an answer).
        var queued: [String] = []
        /// Generations that have ended and still get their final Reset, sent
        /// at the next pump before anything else.
        var finalResets: [UInt32] = []
        var switchStartedAt: UInt64 = 0
    }

    private let state: Mutex<State>
    /// Serializes actions for this tile (the contract: never concurrent,
    /// never re-entrant), and changes to its pipelines.
    private let emitLock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private let cancel = Atomic<Bool>(false)

    init(
        id: UInt64, device: UInt64, channel: UInt8, tier: StreamTier, detects: [String], silent: Bool,
        hasAudio: Bool, owner: FakeCore
    ) {
        self.hasAudio = hasAudio
        self.id = id
        self.device = device
        self.channel = channel
        self.openedTier = tier
        self.detects = detects
        self.silent = silent
        self.owner = owner
        let clip = owner.clip(channel: channel, tier: tier, opened: tier)
        state = Mutex(State(lanes: [Lane(generation: 0, tier: tier, clip: clip, cursor: 0)]))
    }

    var outstanding: Int { state.withLock { $0.lanes.reduce(0) { $0 + $1.outstanding } } }
    var answerLog: FakeAnswers { state.withLock { $0.answers } }
    var gated: Int { state.withLock { s in s.lanes.first { $0.generation == s.shown }?.pipeline.gated ?? 0 } }
    var generations: (shown: UInt32, running: [UInt32]) {
        state.withLock { s in (s.shown, s.lanes.map(\.generation)) }
    }
    var shownTier: StreamTier {
        state.withLock { s in s.lanes.first { $0.generation == s.shown }?.tier ?? openedTier }
    }

    // MARK: Playback

    func start(fps: Double) {
        state.withLock { $0.running = true }
        let thread = Thread { [self] in
            let period = 1.0 / fps
            var next = ContinuousClock.now
            var lastStats = ContinuousClock.now
            let audioStart = ContinuousClock.now
            var audioBlocks = 0
            var detectionCycle = 0
            var detectionActive: String?
            let detectionStart = ContinuousClock.now
            while !cancel.load(ordering: .acquiring) {
                _ = pump(count: 1)
                // Tap blocks at the device's rate: one per 64 ms.
                let due = Int((ContinuousClock.now - audioStart) / .milliseconds(64))
                if due > audioBlocks {
                    owner.pumpAudio(tile: id, count: due - audioBlocks)
                    audioBlocks = due
                }
                if let every = owner.options.detectionDemo, every > 0 {
                    let elapsed = (ContinuousClock.now - detectionStart) / .seconds(1)
                    let phase = elapsed.truncatingRemainder(dividingBy: every)
                    // Channels are staggered so not every tile lights up at once.
                    let offset = Double(channel % 4) * every / 4
                    let inWindow = (phase - offset + every).truncatingRemainder(dividingBy: every) < 3
                    if inWindow, detectionActive == nil {
                        let kind = detects.isEmpty ? DetectionEvent.motionKind : detects[detectionCycle % detects.count]
                        detectionCycle += 1
                        detectionActive = kind
                        owner.emitDetection(device: device, channel: Int(channel), kind: kind, start: true)
                    } else if !inWindow, let kind = detectionActive {
                        detectionActive = nil
                        owner.emitDetection(device: device, channel: Int(channel), kind: kind, start: false)
                    }
                }
                next += .seconds(period)
                if ContinuousClock.now - lastStats > .seconds(1) {
                    lastStats = ContinuousClock.now
                    let a = answerLog
                    let received = state.withLock { s in s.lanes.first { $0.generation == s.shown }?.cursor ?? 0 }
                    owner.event(
                        #"{"type":"tile_stats","tile":\#(id),"received":\#(received),"decoded":\#(a.decodedPTS.count),"gated":\#(gated),"dropped":0,"failures":\#(a.failures.count),"resets":0}"#)
                    if let json = owner.audioStatsJSON(tile: id) { owner.event(json) }
                }
                let wait = next - ContinuousClock.now
                if wait > .zero {
                    Thread.sleep(forTimeInterval: Double(wait.components.seconds) + Double(wait.components.attoseconds) / 1e18)
                }
            }
            done.signal()
        }
        thread.name = "fakecore.tile.\(id)"
        thread.start()
    }

    /// Stops playback and waits until no action is running.
    func stop() {
        cancel.store(true, ordering: .releasing)
        let wasRunning = state.withLock { s -> Bool in
            s.stopped = true
            defer { s.running = false }
            return s.running
        }
        if wasRunning { done.wait() }
        emitLock.lock()  // an action in progress (manual pump) finishes first
        emitLock.unlock()
    }

    /// Sends each ended generation's final Reset (docs/FFI.md: the old
    /// generation's last action is a Reset, in order with the new one's).
    func deliverFinalResets() {
        emitLock.lock()
        defer { emitLock.unlock() }
        let resets = state.withLock { s -> [UInt32] in
            defer { s.finalResets.removeAll() }
            return s.stopped ? [] : s.finalResets
        }
        for g in resets { owner.deliver(.reset, tile: id, generation: g) }
    }

    func flushEvents() {
        deliverFinalResets()
        let events = state.withLock { s -> [String] in
            defer { s.queued.removeAll() }
            return s.stopped ? [] : s.queued
        }
        for json in events { owner.event(json) }
    }

    func pump(count: Int) -> Int {
        var pushed = 0
        for _ in 0..<count {
            flushEvents()
            emitLock.lock()
            defer { emitLock.unlock() }
            var timedOut = false
            let steps = state.withLock { s -> [(UInt32, [StandInAction])]? in
                guard !silent, !s.stopped else { return nil }
                // A new tier that never decodes gives up, as in the core.
                let limit = UInt64(owner.options.tierSwitchTimeout / .nanoseconds(1))
                let now = HostClock.nowNanos()
                for lane in s.lanes where lane.generation > s.shown && !lane.answeredDecode && now &- lane.startedAt > limit {
                    s.lanes.removeAll { $0.generation == lane.generation }
                    s.retired.insert(lane.generation)
                    s.finalResets.append(lane.generation)
                    s.queued.append(
                        #"{"type":"tile_tier","tile":\#(id),"tier":"\#(lane.tier.name)","state":"failed","generation":\#(lane.generation),"reason":"timeout"}"#)
                    timedOut = true
                }
                var steps: [(UInt32, [StandInAction])] = []
                for i in s.lanes.indices {
                    let clip = s.lanes[i].clip
                    guard !clip.units.isEmpty else { continue }
                    if s.lanes[i].cursor >= clip.units.count && !owner.options.loop { continue }
                    let unit = clip.units[s.lanes[i].cursor % clip.units.count]
                    // Every tier of a camera carries the same camera time.
                    let pts = Int64(s.lanes[i].cursor) * 50_000
                    s.lanes[i].cursor += 1
                    var actions: [StandInAction] = []
                    if s.lanes[i].pendingFailures > 0 {
                        s.lanes[i].pendingFailures = 0
                        actions += s.lanes[i].pipeline.decodeFailed()
                    }
                    actions += s.lanes[i].pipeline.push(clip.data.subdata(in: unit.range), ptsMicros: pts)
                    steps.append((s.lanes[i].generation, actions))
                }
                return steps.isEmpty ? nil : steps
            }
            if timedOut {
                emitLock.unlock()
                flushEvents()
                emitLock.lock()
            }
            guard let steps else { break }
            pushed += 1
            for (generation, actions) in steps {
                for action in actions { run(action, generation: generation) }
            }
        }
        return pushed
    }

    func inject(_ action: FakeAction, generation: UInt32?) {
        emitLock.lock()
        defer { emitLock.unlock() }
        let g = generation ?? state.withLock { $0.shown }
        switch action {
        case .configure(let codec, let sets, let color, let size, let visible):
            run(.configure(codec: codec, parameterSets: sets, color: color), generation: g, size: size, visible: visible)
        case .decode(let data, let keyframe, let pts):
            run(.decode(annexB: data, keyframe: keyframe, ptsMicros: pts), generation: g)
        case .flush: run(.flush, generation: g)
        case .reset: run(.reset, generation: g)
        case .clear: run(.clear, generation: g)
        case .status(let s): run(nil, generation: g, status: s)
        }
    }

    // MARK: Tier switch

    /// Starts (or cancels) a switch; returns the events to send now.
    func setTier(_ tier: StreamTier) -> [String] {
        emitLock.lock()
        defer { emitLock.unlock() }
        let now = HostClock.nowNanos()
        return state.withLock { s -> [String] in
            guard !s.stopped, !silent else { return [] }
            guard let shownLane = s.lanes.first(where: { $0.generation == s.shown }) else { return [] }
            let pending = s.lanes.first { $0.generation > s.shown }
            if let pending, pending.tier == tier { return [] }
            if pending == nil, shownLane.tier == tier {
                // The tier already requested: nothing starts; reported again.
                return [
                    #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"active","generation":\#(s.shown)}"#
                ]
            }
            var events: [String] = []
            if let pending {
                // Superseded: its pipeline stops with a final Reset, nothing
                // more is owed for it.
                s.lanes.removeAll { $0.generation == pending.generation }
                s.retired.insert(pending.generation)
                s.finalResets.append(pending.generation)
                events.append(
                    #"{"type":"tile_tier","tile":\#(id),"tier":"\#(pending.tier.name)","state":"failed","generation":\#(pending.generation),"reason":"superseded"}"#)
            }
            if tier == shownLane.tier {
                // Back to the tier shown: nothing starts; reported as active.
                events.append(
                    #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"active","generation":\#(s.shown)}"#)
                return events
            }
            let generation = s.nextGeneration
            s.nextGeneration += 1
            s.switchStartedAt = now
            events.append(
                #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"starting","generation":\#(generation)}"#)
            if owner.options.tierSwitchFails {
                s.retired.insert(generation)
                s.finalResets.append(generation)
                s.queued.append(
                    #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"failed","generation":\#(generation),"reason":"start_rejected"}"#)
                return events
            }
            let clip = owner.clip(channel: channel, tier: tier, opened: openedTier)
            s.lanes.append(Lane(generation: generation, tier: tier, clip: clip, cursor: shownLane.cursor))
            return events
        }
    }

    // MARK: Emitting

    /// Emits one action synchronously and does the answering bookkeeping
    /// (emitLock is held by the caller). Order matters (see the comments): a
    /// Decode is counted before the app sees it, a discard is applied after
    /// the app has executed it.
    private func run(
        _ action: StandInAction?, generation: UInt32, size: PixelSize? = nil, visible: PixelRect? = nil,
        status: TileStatus? = nil
    ) {
        let live = state.withLock { s in !s.stopped && s.lanes.contains { $0.generation == generation } }
        guard live else { return }
        if let status {
            owner.deliver(.status(status), tile: id, generation: generation)
            return
        }
        guard let action else { return }
        setStatus(.waiting, generation: generation, onlyIfUnset: true)
        switch action {
        case .decode(let data, let keyframe, let pts):
            lane(generation) { l in
                l.outstanding += 1
                l.submitted.insert(pts)
            }
            data.withUnsafeBytes {
                owner.deliver(
                    .decode(CoreDecode(accessUnit: $0, keyframe: keyframe, ptsMicros: pts)), tile: id,
                    generation: generation)
            }
            setStatus(.playing, generation: generation)
        case .configure(let codec, let sets, let color):
            lane(generation) { $0.configurePending = true }
            let coded = size ?? Self.dimensions(codec: codec, sets: sets)
            Self.withBorrowed(sets) { borrowed in
                owner.deliver(
                    .configure(
                        CoreConfigure(
                            codec: codec, parameterSets: borrowed, codedSize: coded,
                            visibleRect: visible ?? PixelRect(x: 0, y: 0, width: coded.width, height: coded.height),
                            color: Self.coreColor(color))),
                    tile: id, generation: generation)
            }
            discard(generation, configure: true)
        case .flush:
            setStatus(.recovering, generation: generation)
            owner.deliver(.flush, tile: id, generation: generation)
            discard(generation, configure: false)
        case .reset:
            owner.deliver(.reset, tile: id, generation: generation)
            discard(generation, configure: false)
        case .clear:
            owner.deliver(.clear, tile: id, generation: generation)
        }
    }

    private func lane(_ generation: UInt32, _ body: (inout Lane) -> Void) {
        state.withLock { s in
            guard let i = s.lanes.firstIndex(where: { $0.generation == generation }) else { return }
            body(&s.lanes[i])
        }
    }

    /// Reports a status change (emitLock is held by the caller).
    private func setStatus(_ status: TileStatus, generation: UInt32, onlyIfUnset: Bool = false) {
        var send = false
        lane(generation) { l in
            if onlyIfUnset ? l.status != nil : l.status == status { return }
            l.status = status
            send = true
        }
        if send { owner.deliver(.status(status), tile: id, generation: generation) }
    }

    /// Flush, Reset and Configure discard what is in flight (no answers owed).
    /// Applied after the app executed the action: a result reported while the
    /// action ran still counts, and results after it are dropped by the app's
    /// decoder (new epoch).
    private func discard(_ generation: UInt32, configure: Bool) {
        lane(generation) { l in
            l.outstanding = 0
            l.submitted.removeAll()
            if configure { l.configurePending = false }
        }
    }

    func answered(_ answer: Answer, generation: UInt32) {
        let now = HostClock.nowNanos()
        let problem = state.withLock { s -> String? in
            guard let i = s.lanes.firstIndex(where: { $0.generation == generation }) else {
                if s.retired.contains(generation) || generation < s.shown {
                    owner.countLate()
                    return nil
                }
                return "answer for generation \(generation), which never ran"
            }
            s.answers.generations.append(generation)
            switch answer {
            case .decoded(let pts):
                s.answers.decodedPTS.append(pts)
                guard s.lanes[i].outstanding > 0 else {
                    return "decoded(\(pts)) with no Decode outstanding (answered twice?)"
                }
                guard s.lanes[i].submitted.contains(pts) else { return "decoded(\(pts)) for a pts never sent" }
                s.lanes[i].outstanding -= 1
                let first = !s.lanes[i].answeredDecode
                s.lanes[i].answeredDecode = true
                if first, generation == 0 || generation == s.shown, owner.claimFirstPicture() {
                    s.queued.append(
                        #"{"type":"timing","what":"first_picture","tile":\#(id),"ns":\#(now &- owner.createdAt)}"#)
                }
                if first, generation > s.shown {
                    // The switch: the old pipeline stops here, nothing more
                    // is owed for it, and the new one is the only one left.
                    let tier = s.lanes[i].tier
                    for old in s.lanes where old.generation < generation {
                        s.retired.insert(old.generation)
                        s.finalResets.append(old.generation)
                    }
                    s.lanes.removeAll { $0.generation < generation }
                    s.shown = generation
                    s.queued.append(
                        #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"active","generation":\#(generation)}"#)
                    s.queued.append(
                        #"{"type":"timing","what":"tier_switch","tile":\#(id),"ns":\#(now &- s.switchStartedAt)}"#)
                }
            case .failed(let kind):
                s.answers.failures.append(kind)
                if kind.answersConfigure {
                    guard s.lanes[i].configurePending else { return "\(kind) answered with no Configure pending" }
                    s.lanes[i].configurePending = false
                    if generation > s.shown {
                        // The new tier cannot be shown: drop it, keep the old.
                        let tier = s.lanes[i].tier
                        s.retired.insert(generation)
                        s.lanes.remove(at: i)
                        s.finalResets.append(generation)
                        s.queued.append(
                            #"{"type":"tile_tier","tile":\#(id),"tier":"\#(tier.name)","state":"failed","generation":\#(generation),"reason":"undecodable"}"#)
                    }
                } else {
                    guard s.lanes[i].outstanding > 0 else { return "\(kind) answered with no Decode outstanding" }
                    s.lanes[i].outstanding -= 1
                    s.lanes[i].pendingFailures += 1
                }
            }
            return nil
        }
        if let problem { owner.violation("tile \(id): \(problem)") }
    }

    // MARK: Helpers

    private static func dimensions(codec: VideoCodec, sets: [Data]) -> PixelSize {
        var tracker = ParameterSetTracker(codec: codec)
        var dirty = false
        for nal in sets where !nal.isEmpty {
            let h = nal[nal.startIndex]
            if tracker.record(type: codec == .h264 ? h & 0x1F : (h >> 1) & 0x3F, nal: nal) { dirty = true }
        }
        _ = tracker.rebuildIfNeeded(dirty: dirty)
        guard let f = tracker.formatDescription else { return .zero }
        let d = CMVideoFormatDescriptionGetDimensions(f)
        return PixelSize(width: UInt32(d.width), height: UInt32(d.height))
    }

    private static func coreColor(_ c: StandInColor) -> CoreColor {
        let m = c.matrix
        // Distinguish the two standards by the Cr -> R coefficient: 1.402 or 1.5748.
        let matrix: ColorMatrix = abs(m.r.z / (c.fullRange ? 1 : 255 / 224) - 1.402) < 0.01 ? .bt601 : .bt709
        return CoreColor(
            matrix: matrix, fullRange: c.fullRange,
            ycbcrToRGB: [m.r.x, m.r.y, m.r.z, m.r.w, m.g.x, m.g.y, m.g.z, m.g.w, m.b.x, m.b.y, m.b.z, m.b.w])
    }

    /// Calls `body` with the sets copied into one buffer, as separate slices.
    private static func withBorrowed<R>(_ sets: [Data], _ body: ([UnsafeRawBufferPointer]) -> R) -> R {
        let total = sets.reduce(0) { $0 + $1.count }
        let storage = UnsafeMutableRawPointer.allocate(byteCount: max(total, 1), alignment: 1)
        defer { storage.deallocate() }
        var offset = 0
        var slices: [UnsafeRawBufferPointer] = []
        for set in sets {
            set.copyBytes(to: UnsafeMutableRawBufferPointer(start: storage + offset, count: set.count))
            slices.append(UnsafeRawBufferPointer(start: storage + offset, count: set.count))
            offset += set.count
        }
        return body(slices)
    }
}

/// The layout the real core computes, in Swift: ceil(sqrt(n)) columns,
/// `gap` between cells, each picture fit (or, with `fill`, cropped) in its
/// cell, whole pixels.
enum FakeLayout {
    static func grid(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool) -> [TileLayout] {
        let n = pictureSizes.count
        guard n > 0 else { return [] }
        let columns = Int(Double(n).squareRoot().rounded(.up))
        let rows = (n + columns - 1) / columns
        let g = Int(gap)
        let cellW = max((Int(viewport.width) - g * (columns - 1)) / columns, 0)
        let cellH = max((Int(viewport.height) - g * (rows - 1)) / rows, 0)
        return pictureSizes.enumerated().map { i, p in
            let cx = (i % columns) * (cellW + g)
            let cy = (i / columns) * (cellH + g)
            func rect(_ x: Int, _ y: Int, _ w: Int, _ h: Int) -> PixelRect {
                PixelRect(x: UInt32(x), y: UInt32(y), width: UInt32(w), height: UInt32(h))
            }
            let cell = rect(cx, cy, cellW, cellH)
            guard p.width > 0, p.height > 0, cellW > 0, cellH > 0 else {
                return TileLayout(cell: cell, dest: cell, source: rect(0, 0, 0, 0))
            }
            let pw = Int(p.width), ph = Int(p.height)
            if fill {
                // Crop the picture to the cell's aspect ratio, centered.
                var sw = pw, sh = ph
                if pw * cellH > ph * cellW { sw = ph * cellW / cellH } else { sh = pw * cellH / cellW }
                return TileLayout(cell: cell, dest: cell, source: rect((pw - sw) / 2, (ph - sh) / 2, sw, sh))
            }
            var w = cellW, h = ph * cellW / pw
            if h > cellH {
                h = cellH
                w = pw * cellH / ph
            }
            return TileLayout(
                cell: cell, dest: rect(cx + (cellW - w) / 2, cy + (cellH - h) / 2, w, h),
                source: rect(0, 0, pw, ph))
        }
    }
}
