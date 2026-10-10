import Foundation
import Synchronization
import VideoKit

/// What the last Configure and Status said about a tile. `version` changes
/// whenever any of it does, so a view can tell cheaply whether to re-layout.
public struct TileSnapshot: Sendable, Equatable {
    public var version: UInt64 = 0
    public var codec: VideoCodec?
    public var codedSize = PixelSize.zero
    public var visibleRect = PixelRect(x: 0, y: 0, width: 0, height: 0)
    public var color: CoreColor?
    public var status = TileStatus.waiting
    public var configures = 0

    public init() {}
}

/// Where a tile's stream tier stands (ABI 3 `nv_tile_set_tier`).
public struct TileTierState: Sendable, Equatable {
    /// The tier whose pictures are shown.
    public var shown: StreamTier
    /// The generation whose pictures are shown.
    public var shownGeneration: UInt32
    /// The tier asked for last (equal to `shown` when no switch is pending).
    public var requested: StreamTier
    /// The last `tile_tier` state, if a switch has been asked for.
    public var lastState: TierSwitchState?
}

/// One open tile on the app's side: the platform decoder that executes the
/// core's actions and holds the newest picture, plus what the core said about
/// the picture's geometry and color. Created by `CoreBridge.openTile`.
///
/// While a tier switch runs (ABI 3), the core drives two pipelines for the
/// tile, told apart by each action's `generation`. Each generation gets its
/// own decoder; the tile keeps showing the current generation until the core
/// announces the switch (`tile_tier` `active`), then shows the new one and
/// releases the old decoder. As a `TilePictureSource` the tile always hands
/// out the shown generation's picture, so a renderer drawing the tile picks
/// up the switch on its next refresh without being rebuilt.
public final class CoreTile: TilePictureSource {
    public let device: UInt64
    public let channel: UInt8

    private let state: Mutex<State>
    private let tileID = TileIdentity()
    private let routerRef: Weak<CoreBridge.Router>

    /// One pipeline's decoder and what its actions said.
    private struct Generation: @unchecked Sendable {
        let number: UInt32
        let tier: StreamTier
        let decoder: TileDecoder
        let link: TileLink
        var snapshot = TileSnapshot()
    }

    private struct State: @unchecked Sendable {
        var generations: [Generation]
        var shown: UInt32 = 0
        var shownTier: StreamTier
        var requested: StreamTier
        var lastState: TierSwitchState?
        /// Bumped whenever the shown generation changes; folded into the
        /// snapshot version and picture sequence so views notice.
        var switches: UInt64 = 0
        var closed = false

        var current: Generation? { generations.first { $0.number == shown } }
    }

    /// The tile's ID, shared with every generation's result handler.
    final class TileIdentity: Sendable {
        let value = Atomic<UInt64>(0)
    }

    /// Shared with a decoder's result handler, which can run on any thread
    /// and outlive the tile (or its generation) by a moment.
    final class TileLink: Sendable {
        let generation: UInt32
        let closed = Atomic<Bool>(false)
        init(generation: UInt32) { self.generation = generation }
    }

    init(router: CoreBridge.Router, device: UInt64, channel: UInt8, tier: StreamTier) {
        self.device = device
        self.channel = channel
        routerRef = Weak(router)
        state = Mutex(State(generations: [], shownTier: tier, requested: tier))
        let first = makeGeneration(0, tier: tier)
        state.withLock { $0.generations = [first] }
    }

    private func makeGeneration(_ number: UInt32, tier: StreamTier) -> Generation {
        let link = TileLink(generation: number)
        let router = routerRef
        let tileID = self.tileID
        let decoder = TileDecoder { [link] event in
            guard !link.closed.load(ordering: .acquiring), let router = router.value else { return }
            let id = tileID.value.load(ordering: .acquiring)
            switch event {
            case .decoded(let pts):
                router.reportDecoded(link: link, tile: id, ptsMicros: pts)
            case .failed(let failure):
                router.reportFailed(link: link, tile: id, CoreDecodeFailure(failure))
            }
        }
        return Generation(number: number, tier: tier, decoder: decoder, link: link)
    }

    /// The core's ID for this tile.
    public var id: UInt64 { tileID.value.load(ordering: .acquiring) }

    /// The tier the tile was opened with.
    public var openedTier: StreamTier { state.withLock { $0.generations.first?.tier ?? $0.shownTier } }

    /// The tier whose pictures are shown now.
    public var tier: StreamTier { state.withLock { $0.shownTier } }

    public var tierState: TileTierState {
        state.withLock {
            TileTierState(shown: $0.shownTier, shownGeneration: $0.shown, requested: $0.requested, lastState: $0.lastState)
        }
    }

    /// The decoder of the shown generation.
    public var decoder: TileDecoder { state.withLock { s in s.current?.decoder ?? s.generations[0].decoder } }

    /// Every decoder alive (one, or two during a switch), for tests.
    public var decoders: [TileDecoder] { state.withLock { $0.generations.map(\.decoder) } }

    /// The shown generation's snapshot; its version also changes on a switch.
    public var snapshot: TileSnapshot {
        state.withLock { s in
            var snap = s.current?.snapshot ?? TileSnapshot()
            snap.version = snap.version &+ (s.switches << 32)
            return snap
        }
    }

    // MARK: TilePictureSource

    public func latestPicture() -> DecodedPicture? {
        let (decoder, switches) = state.withLock { ($0.current?.decoder, $0.switches) }
        guard let picture = decoder?.latestPicture() else { return nil }
        return DecodedPicture(
            pixelBuffer: picture.pixelBuffer, ptsMicros: picture.ptsMicros,
            sequence: Self.sequence(picture.sequence, switches: switches))
    }

    public var pictureSequence: UInt64 {
        let (decoder, switches) = state.withLock { ($0.current?.decoder, $0.switches) }
        return Self.sequence(decoder?.pictureSequence ?? 0, switches: switches)
    }

    private static func sequence(_ s: UInt64, switches: UInt64) -> UInt64 { s &+ (switches << 40) }

    // MARK: From the bridge

    func assign(id: UInt64) { tileID.value.store(id, ordering: .releasing) }

    /// Answers an action this version could not carry out.
    func reject(_ failure: CoreDecodeFailure, generation: UInt32) {
        let link = state.withLock { s in s.generations.first { $0.number == generation }?.link }
        guard let router = routerRef.value else { return }
        router.reportFailed(link: link ?? TileLink(generation: generation), tile: id, failure)
    }

    /// No more results go to the core (the tile is closed or being closed).
    func markClosed() {
        let links = state.withLock { s -> [TileLink] in
            s.closed = true
            return s.generations.map(\.link)
        }
        for link in links { link.closed.store(true, ordering: .releasing) }
    }

    /// Notes the tier the app asked for (before calling the core).
    func noteRequested(_ tier: StreamTier) { state.withLock { $0.requested = tier } }

    /// Waits until every decoder's submitted frames have been reported.
    public func waitForPendingFrames() {
        for d in decoders { d.waitForPendingFrames() }
    }

    /// Folds in a `tile_tier` event. Called on the core's event thread.
    func applyTier(_ tier: StreamTier?, state newState: TierSwitchState, generation: UInt32) {
        let released = state.withLock { s -> [Generation] in
            s.lastState = newState
            switch newState {
            case .active:
                guard generation >= s.shown else { return [] }
                if generation != s.shown {
                    s.shown = generation
                    s.switches &+= 1
                }
                if let t = s.generations.first(where: { $0.number == generation })?.tier ?? tier {
                    s.shownTier = t
                }
                s.requested = s.shownTier
                let old = s.generations.filter { $0.number < generation }
                s.generations.removeAll { $0.number < generation }
                return old
            case .failed:
                s.requested = s.shownTier
                let failed = s.generations.filter { $0.number == generation && $0.number != s.shown }
                s.generations.removeAll { $0.number == generation && $0.number != s.shown }
                return failed
            case .starting, .other:
                return []
            }
        }
        Self.release(released)
    }

    /// Old generations: their results are no longer wanted, and their
    /// decoders are dropped off this thread (a decoder's deinit waits for
    /// frames in flight, which must not hold up the core's event thread).
    private static func release(_ generations: [Generation]) {
        guard !generations.isEmpty else { return }
        for g in generations { g.link.closed.store(true, ordering: .releasing) }
        let decoders = generations.map(\.decoder)
        DispatchQueue.global(qos: .utility).async { withExtendedLifetime(decoders) {} }
    }

    /// Executes one action synchronously, as the core's callback requires
    /// (borrowed bytes: only what the decoder needs is copied, by the decoder).
    func perform(_ action: CoreAction, generation: UInt32) {
        let (target, closed) = state.withLock { s -> (Generation?, Bool) in
            if s.closed { return (nil, true) }
            if let g = s.generations.first(where: { $0.number == generation }) { return (g, false) }
            // An older generation than the shown one has been switched away
            // from: the core sends it nothing more after the switch.
            guard generation > s.shown, generation > (s.generations.map(\.number).max() ?? 0) else {
                return (nil, false)
            }
            let g = makeGeneration(generation, tier: s.requested)
            s.generations.append(g)
            return (g, false)
        }
        if closed { return }
        guard let target else {
            // Never leave a Decode unanswered, even one for a stale generation.
            if case .decode = action {
                routerRef.value?.reportFailed(link: TileLink(generation: generation), tile: id, .noPicture)
            }
            return
        }
        let decoder = target.decoder
        switch action {
        case .configure(let c):
            update(generation) { snap in
                snap.version &+= 1
                snap.configures += 1
                snap.codec = c.codec
                snap.codedSize = c.codedSize
                snap.visibleRect = c.visibleRect
                snap.color = c.color
            }
            decoder.configure(
                codec: c.codec, parameterSets: c.parameterSets.map { Data($0) },
                fullRange: c.color.fullRange)
        case .decode(let d):
            decoder.decode(annexB: d.accessUnit, keyframe: d.keyframe, ptsMicros: d.ptsMicros)
        case .flush:
            decoder.flush()
        case .reset:
            decoder.reset()
        case .clear:
            decoder.clear()
        case .status(let status):
            update(generation) { snap in
                if snap.status != status {
                    snap.status = status
                    snap.version &+= 1
                }
            }
        }
    }

    private func update(_ generation: UInt32, _ body: (inout TileSnapshot) -> Void) {
        state.withLock { s in
            guard let i = s.generations.firstIndex(where: { $0.number == generation }) else { return }
            body(&s.generations[i].snapshot)
        }
    }
}

/// A weak reference that is `Sendable` (the referent is `Sendable`).
final class Weak<T: AnyObject & Sendable>: Sendable {
    private let box: Mutex<WeakBox<T>>
    init(_ value: T) { box = Mutex(WeakBox(value: value)) }
    var value: T? { box.withLock { $0.value } }
}

private struct WeakBox<T: AnyObject>: @unchecked Sendable {
    weak var value: T?
}
