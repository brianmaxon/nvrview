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

/// A Swift test double of the C core (docs/FFI.md). It plays fixture files and
/// emits actions and events exactly as the real core does: Configure with
/// parameter sets, color and rectangles, Decode with borrowed Annex-B bytes,
/// Flush, Reset, Clear and Status; serialized per tile; events as JSON text.
/// Its "pipeline" is the temporary stand-in (gating, Configure on new
/// parameter sets, Flush after a failure).
///
/// It also checks the app: every Decode not discarded by a later Flush, Reset
/// or Configure must be answered exactly once, nothing may be answered that
/// was not asked, close/remove/free must not be called from inside a callback,
/// and no callback may run after `free`. Problems collect in `violations`.
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
        public var pacing = Pacing.manual
        /// Wrap around at the end of the clip (timestamps keep growing).
        public var loop = true
        /// When set, the device reports a failed login (with this many
        /// attempts remaining) instead of connecting, and its tiles stay silent.
        public var loginFailureRemaining: Int?
        public var channelCount = 4

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
        var tiles: [UInt64]
    }

    private struct World {
        var nextID: UInt64 = 1
        var devices: [UInt64: Device] = [:]
        var tiles: [UInt64: FakeTile] = [:]
        var freed = false
    }

    private let options: Options
    private let sink: any CoreSink
    private let world = Mutex(World())
    private let eventLock = NSLock()
    private let problems = Mutex<[String]>([])
    private let late = Atomic<Int>(0)
    private let afterFree = Atomic<Int>(0)

    public init(options: Options, sink: any CoreSink) {
        self.options = options
        self.sink = sink
    }

    /// Everything the app did wrong so far (empty when all is well).
    public var violations: [String] { problems.withLock { $0 } }
    /// Answers that arrived for a tile already closed (legal, counted).
    public var lateAnswers: Int { late.load(ordering: .relaxed) }
    /// Answers that arrived after `free` (a bridge bug).
    public var answersAfterFree: Int { afterFree.load(ordering: .relaxed) }

    fileprivate func violation(_ text: String) { problems.withLock { $0.append(text) } }

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
        let id: UInt64 = try world.withLock { w throws(CoreStatus) in
            if w.freed { throw .closed }
            let id = w.nextID
            w.nextID += 1
            w.devices[id] = Device(failed: failed, tiles: [])
            return id
        }
        emitEvent(#"{"type":"device_state","device":\#(id),"state":"connecting"}"#)
        if let remaining = options.loginFailureRemaining {
            emitEvent(#"{"type":"device_state","device":\#(id),"state":"login_failed","status":401,"remain_times":\#(remaining)}"#)
        } else {
            emitEvent(#"{"type":"device_state","device":\#(id),"state":"connected","future_field":1}"#)
            emitEvent(#"{"type":"device_info","device":\#(id),"name":"NVR","model":"EXAMPLE-MODEL"}"#)
            let channels = (0..<options.channelCount).map {
                #"{"channel":\#($0),"connected":true,"tiers":["main","sub"],"name":"Channel \#($0 + 1)","model":"EXAMPLE-CAM"}"#
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
        let clip = options.clips[Int(channel) % options.clips.count]
        let (id, tile): (UInt64, FakeTile) = try world.withLock { w throws(CoreStatus) in
            if w.freed { throw .closed }
            guard var d = w.devices[device] else { throw .unknownID }
            let id = w.nextID
            w.nextID += 1
            let tile = FakeTile(id: id, clip: clip, silent: d.failed, owner: self)
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
        guard let t = answerTarget(tile) else { return .unknownID }
        t.answered(.decoded(ptsMicros))
        return .ok
    }

    public func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus {
        guard let t = answerTarget(tile) else { return .unknownID }
        t.answered(.failed(kind))
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

    // MARK: Test hooks

    /// Pushes up to `count` access units of the tile's clip through the
    /// pipeline and emits the resulting actions on the calling thread. Returns
    /// how many were pushed (0 for an unknown tile). Manual pacing only; do not
    /// call it concurrently for one tile.
    @discardableResult
    public func pump(tile: UInt64, count: Int = 1) -> Int {
        guard let t = world.withLock({ $0.tiles[tile] }) else { return 0 }
        return t.pump(count: count)
    }

    /// Emits one hand-made action to the tile, bypassing the pipeline (for
    /// failure injection). The answering check applies to it too.
    public func inject(tile: UInt64, _ action: FakeAction) {
        guard let t = world.withLock({ $0.tiles[tile] }) else { return }
        t.inject(action)
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

    /// The decode answers a tile has received so far.
    public func answers(tile: UInt64) -> FakeAnswers? {
        world.withLock { $0.tiles[tile] }?.answerLog
    }

    /// Access units the stand-in pipeline gated (not decoded) for a tile.
    public func gated(tile: UInt64) -> Int { world.withLock { $0.tiles[tile] }?.gated ?? 0 }

    // MARK: Internals

    private func detach(_ id: UInt64) -> FakeTile? {
        world.withLock { w in
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

    fileprivate func deliver(_ action: CoreAction, tile: UInt64) {
        insideCallback { sink.core(didDeliver: action, tile: tile) }
    }

    fileprivate func event(_ json: String) { emitEvent(json) }
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
}

private enum Answer: Sendable {
    case decoded(Int64)
    case failed(CoreDecodeFailure)
}

// MARK: One tile

private final class FakeTile: Sendable {
    let id: UInt64
    let clip: FakeClip
    let silent: Bool
    private unowned let owner: FakeCore

    private struct State {
        var pipeline: StandInPipeline
        var cursor = 0
        /// Decodes asked and not yet answered or discarded.
        var outstanding = 0
        var submitted = Set<Int64>()
        var configurePending = false
        var answers = FakeAnswers()
        var pendingFailures = 0
        var stopped = false
        var running = false
        var status: TileStatus?
    }

    private let state: Mutex<State>
    /// Serializes actions for this tile (the contract: never concurrent,
    /// never re-entrant).
    private let emitLock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private let cancel = Atomic<Bool>(false)

    init(id: UInt64, clip: FakeClip, silent: Bool, owner: FakeCore) {
        self.id = id
        self.clip = clip
        self.silent = silent
        self.owner = owner
        state = Mutex(State(pipeline: StandInPipeline(codec: clip.codec, colorOverride: clip.colorOverride)))
    }

    var outstanding: Int { state.withLock { $0.outstanding } }
    var answerLog: FakeAnswers { state.withLock { $0.answers } }
    var gated: Int { state.withLock { $0.pipeline.gated } }

    // MARK: Playback

    func start(fps: Double) {
        state.withLock { $0.running = true }
        let thread = Thread { [self] in
            let period = 1.0 / fps
            var next = ContinuousClock.now
            var lastStats = ContinuousClock.now
            while !cancel.load(ordering: .acquiring) {
                _ = pump(count: 1)
                next += .seconds(period)
                if ContinuousClock.now - lastStats > .seconds(1) {
                    lastStats = ContinuousClock.now
                    let a = answerLog
                    owner.event(
                        #"{"type":"tile_stats","tile":\#(id),"received":\#(state.withLock { $0.cursor }),"decoded":\#(a.decodedPTS.count),"gated":\#(gated),"dropped":0,"failures":\#(a.failures.count),"resets":0}"#)
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

    func pump(count: Int) -> Int {
        var pushed = 0
        for _ in 0..<count {
            let step = state.withLock { s -> (Data, Int64, [StandInAction])? in
                guard !silent, !s.stopped, !clip.units.isEmpty else { return nil }
                if s.cursor >= clip.units.count && !(owner.loops) { return nil }
                let unit = clip.units[s.cursor % clip.units.count]
                let pts = Int64(s.cursor) * 50_000
                s.cursor += 1
                var actions: [StandInAction] = []
                if s.pendingFailures > 0 {
                    s.pendingFailures = 0
                    actions += s.pipeline.decodeFailed()
                }
                let au = clip.data.subdata(in: unit.range)
                actions += s.pipeline.push(au, ptsMicros: pts)
                return (au, pts, actions)
            }
            guard let (_, _, actions) = step else { break }
            pushed += 1
            for action in actions { run(action) }
        }
        return pushed
    }

    func inject(_ action: FakeAction) {
        switch action {
        case .configure(let codec, let sets, let color, let size, let visible):
            run(.configure(codec: codec, parameterSets: sets, color: color), size: size, visible: visible)
        case .decode(let data, let keyframe, let pts): run(.decode(annexB: data, keyframe: keyframe, ptsMicros: pts))
        case .flush: run(.flush)
        case .reset: run(.reset)
        case .clear: run(.clear)
        case .status(let s): run(nil, status: s)
        }
    }

    // MARK: Emitting

    /// Emits one action synchronously and does the answering bookkeeping.
    /// Order matters (see the comments): a Decode is counted before the app
    /// sees it, a discard is applied after the app has executed it.
    private func run(
        _ action: StandInAction?, size: PixelSize? = nil, visible: PixelRect? = nil, status: TileStatus? = nil
    ) {
        emitLock.lock()
        defer { emitLock.unlock() }
        if state.withLock({ $0.stopped }) { return }
        if let status {
            owner.deliver(.status(status), tile: id)
            return
        }
        guard let action else { return }
        setStatus(.waiting, onlyIfUnset: true)
        switch action {
        case .decode(let data, let keyframe, let pts):
            state.withLock { s in
                s.outstanding += 1
                s.submitted.insert(pts)
            }
            data.withUnsafeBytes {
                owner.deliver(.decode(CoreDecode(accessUnit: $0, keyframe: keyframe, ptsMicros: pts)), tile: id)
            }
            setStatus(.playing)
        case .configure(let codec, let sets, let color):
            state.withLock { $0.configurePending = true }
            let coded = size ?? Self.dimensions(codec: codec, sets: sets)
            Self.withBorrowed(sets) { borrowed in
                owner.deliver(
                    .configure(
                        CoreConfigure(
                            codec: codec, parameterSets: borrowed, codedSize: coded,
                            visibleRect: visible ?? PixelRect(x: 0, y: 0, width: coded.width, height: coded.height),
                            color: Self.coreColor(color))),
                    tile: id)
            }
            discard(configure: true)
        case .flush:
            setStatus(.recovering)
            owner.deliver(.flush, tile: id)
            discard(configure: false)
        case .reset:
            owner.deliver(.reset, tile: id)
            discard(configure: false)
        case .clear:
            owner.deliver(.clear, tile: id)
        }
    }

    /// Reports a status change (emitLock is held by the caller).
    private func setStatus(_ status: TileStatus, onlyIfUnset: Bool = false) {
        let send = state.withLock { s -> Bool in
            if onlyIfUnset ? s.status != nil : s.status == status { return false }
            s.status = status
            return true
        }
        if send { owner.deliver(.status(status), tile: id) }
    }

    /// Flush, Reset and Configure discard what is in flight (no answers owed).
    /// Applied after the app executed the action: a result reported while the
    /// action ran still counts, and results after it are dropped by the app's
    /// decoder (new epoch).
    private func discard(configure: Bool) {
        state.withLock { s in
            s.outstanding = 0
            s.submitted.removeAll()
            if configure { s.configurePending = false }
        }
    }

    func answered(_ answer: Answer) {
        let problem = state.withLock { s -> String? in
            switch answer {
            case .decoded(let pts):
                s.answers.decodedPTS.append(pts)
                guard s.outstanding > 0 else { return "decoded(\(pts)) with no Decode outstanding (answered twice?)" }
                guard s.submitted.contains(pts) else { return "decoded(\(pts)) for a pts never sent" }
                s.outstanding -= 1
            case .failed(let kind):
                s.answers.failures.append(kind)
                if kind.answersConfigure {
                    guard s.configurePending else { return "\(kind) answered with no Configure pending" }
                    s.configurePending = false
                } else {
                    guard s.outstanding > 0 else { return "\(kind) answered with no Decode outstanding" }
                    s.outstanding -= 1
                    s.pendingFailures += 1
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

extension FakeCore {
    fileprivate var loops: Bool { options.loop }
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
