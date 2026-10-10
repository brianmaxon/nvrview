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

/// One open tile on the app's side: the platform decoder that executes the
/// core's actions and holds the newest picture, plus what the core said about
/// the picture's geometry and color. Created by `CoreBridge.openTile`.
public final class CoreTile: Sendable {
    public let device: UInt64
    public let channel: UInt8
    public let tier: StreamTier
    /// Draw from this: it is a `TilePictureSource`.
    public let decoder: TileDecoder

    private let state = Mutex(State())
    private let link: TileLink

    private struct State {
        var id: UInt64 = 0
        var snapshot = TileSnapshot()
    }

    /// Shared with the decoder's result handler, which can run on any thread
    /// and outlive the tile by a moment.
    final class TileLink: Sendable {
        let id = Atomic<UInt64>(0)
        let closed = Atomic<Bool>(false)
        let router: Weak<CoreBridge.Router>
        init(router: CoreBridge.Router) { self.router = Weak(router) }
    }

    init(router: CoreBridge.Router, device: UInt64, channel: UInt8, tier: StreamTier) {
        let link = TileLink(router: router)
        self.link = link
        self.device = device
        self.channel = channel
        self.tier = tier
        decoder = TileDecoder { event in
            guard !link.closed.load(ordering: .acquiring), let router = link.router.value else { return }
            let id = link.id.load(ordering: .acquiring)
            switch event {
            case .decoded(let pts): router.reportDecoded(link: link, tile: id, ptsMicros: pts)
            case .failed(let failure): router.reportFailed(link: link, tile: id, CoreDecodeFailure(failure))
            }
        }
    }

    /// The core's ID for this tile.
    public var id: UInt64 { link.id.load(ordering: .acquiring) }

    public var snapshot: TileSnapshot { state.withLock { $0.snapshot } }

    func assign(id: UInt64) { link.id.store(id, ordering: .releasing) }

    /// Answers an action this version could not carry out.
    func reject(_ failure: CoreDecodeFailure) {
        link.router.value?.reportFailed(link: link, tile: id, failure)
    }

    /// No more results go to the core (the tile is closed or being closed).
    func markClosed() { link.closed.store(true, ordering: .releasing) }

    /// Executes one action synchronously, as the core's callback requires
    /// (borrowed bytes: only what the decoder needs is copied, by the decoder).
    func perform(_ action: CoreAction) {
        switch action {
        case .configure(let c):
            state.withLock { s in
                s.snapshot.version &+= 1
                s.snapshot.configures += 1
                s.snapshot.codec = c.codec
                s.snapshot.codedSize = c.codedSize
                s.snapshot.visibleRect = c.visibleRect
                s.snapshot.color = c.color
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
            state.withLock { s in
                if s.snapshot.status != status {
                    s.snapshot.status = status
                    s.snapshot.version &+= 1
                }
            }
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
