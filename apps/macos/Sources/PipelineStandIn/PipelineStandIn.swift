// TEMPORARY: a stand-in for the shared Rust `video-pipeline` crate, used only
// by the tests and NVRViewDemo until the C interface lands in M2. The app must
// never ship it, and it must not grow: real policy (gating details,
// backpressure, recovery escalation, VUI parsing, layout modes, stats) belongs
// in the Rust pipeline (docs/RENDERING.md). Delete this target when the
// pipeline drives `TileDecoder` and `GridRenderer` through the C interface.

import CoreGraphics
import CoreMedia
import Foundation
import Synchronization
import VideoKit

/// The color part of a Configure action.
public struct StandInColor: Sendable, Equatable {
    public var matrix: YCbCrToRGB
    public var fullRange: Bool

    public static let bt601VideoRange = standard(kr: 0.299, kb: 0.114, fullRange: false)
    public static let bt601FullRange = standard(kr: 0.299, kb: 0.114, fullRange: true)
    public static let bt709VideoRange = standard(kr: 0.2126, kb: 0.0722, fullRange: false)
    public static let bt709FullRange = standard(kr: 0.2126, kb: 0.0722, fullRange: true)

    /// RENDERING.md's default without a VUI color description: BT.709 at 720
    /// lines and above, BT.601 below, video range.
    public static func defaultFor(height: Int) -> StandInColor {
        height >= 720 ? bt709VideoRange : bt601VideoRange
    }

    /// The YCbCr-to-RGB matrix for luma coefficients Kr and Kb, taking
    /// normalized 8-bit samples (code / 255) as `YCbCrToRGB` defines them.
    static func standard(kr: Float, kb: Float, fullRange: Bool) -> StandInColor {
        let kg = 1 - kr - kb
        // E'y = ys * y + yo; E'cb = cs * cb + co (same for cr), on code / 255.
        let ys: Float = fullRange ? 1 : 255 / 219
        let yo: Float = fullRange ? 0 : -16 / 219
        let cs: Float = fullRange ? 1 : 255 / 224
        let co: Float = fullRange ? -128 / 255 : -128 / 224
        let crR = 2 * (1 - kr)
        let cbB = 2 * (1 - kb)
        let cbG = -2 * kb * (1 - kb) / kg
        let crG = -2 * kr * (1 - kr) / kg
        func row(_ cbK: Float, _ crK: Float) -> SIMD4<Float> {
            SIMD4(ys, cbK * cs, crK * cs, yo + (cbK + crK) * co)
        }
        return StandInColor(
            matrix: YCbCrToRGB(r: row(0, crR), g: row(cbG, crG), b: row(cbB, 0)),
            fullRange: fullRange)
    }
}

public enum StandInAction: Sendable, Equatable {
    case configure(codec: VideoCodec, parameterSets: [Data], color: StandInColor)
    case decode(annexB: Data, keyframe: Bool, ptsMicros: Int64)
    case flush
    case reset
    case clear
}

/// Turns access units into RENDERING.md actions: Configure on the first
/// keyframe, Reset + Configure when a keyframe brings new parameter sets,
/// Decode while not gated, and Flush plus gating to the next keyframe after a
/// decode failure. That is all.
public struct StandInPipeline: Sendable {
    public let codec: VideoCodec
    /// Fixtures with a known color description set this; otherwise the
    /// height rule applies.
    public var colorOverride: StandInColor?
    public private(set) var gated = 0
    private var parameterSets: [Data] = []
    private var configured = false
    private var waitingForKeyframe = true

    public init(codec: VideoCodec, colorOverride: StandInColor? = nil) {
        self.codec = codec
        self.colorOverride = colorOverride
    }

    public mutating func push(_ annexB: Data, ptsMicros: Int64) -> [StandInAction] {
        let nals = AnnexB.split(annexB, codec: codec)
        let base = annexB.startIndex
        let sets = nals.filter { NALType.isParameterSet($0.type, codec: codec) }
            .map { annexB.subdata(in: (base + $0.range.lowerBound)..<(base + $0.range.upperBound)) }
        let keyframe = nals.contains { NALType.isKeyframe($0.type, codec: codec) }
        var actions: [StandInAction] = []
        if keyframe, !sets.isEmpty, sets != parameterSets {
            if configured { actions.append(.reset) }
            actions.append(.configure(codec: codec, parameterSets: sets, color: color(for: sets)))
            parameterSets = sets
            configured = true
            waitingForKeyframe = false
        } else if waitingForKeyframe {
            guard keyframe, configured else {
                gated += 1
                return []
            }
            waitingForKeyframe = false
        }
        actions.append(.decode(annexB: annexB, keyframe: keyframe, ptsMicros: ptsMicros))
        return actions
    }

    public mutating func decodeFailed() -> [StandInAction] {
        waitingForKeyframe = true
        return [.flush]
    }

    private func color(for sets: [Data]) -> StandInColor {
        if let colorOverride { return colorOverride }
        var tracker = ParameterSetTracker(codec: codec)
        for set in sets {
            let header = set[set.startIndex]
            tracker.record(type: codec == .h264 ? header & 0x1F : (header >> 1) & 0x3F, nal: set)
        }
        _ = tracker.rebuildIfNeeded(dirty: true)
        let height = tracker.formatDescription.map { Int(CMVideoFormatDescriptionGetDimensions($0).height) } ?? 0
        return StandInColor.defaultFor(height: height)
    }
}

/// One tile driven by the stand-in: a pipeline, a `TileDecoder`, and the
/// color of the current configuration. `push` may be called from any thread,
/// one call at a time; decode failures reported by the decoder are applied on
/// the next `push`, which is how the core will serialize them.
public final class StandInTile: Sendable {
    public let decoder: TileDecoder
    private let state: Mutex<(pipeline: StandInPipeline, color: StandInColor?)>
    private let failures: FailureCount

    private final class FailureCount: Sendable {
        let pending = Atomic<Int>(0)
        let total = Atomic<Int>(0)
    }

    /// `onEvent`, if given, also sees every decoder event (tests use it).
    public init(
        codec: VideoCodec, colorOverride: StandInColor? = nil, onEvent: TileDecoder.EventHandler? = nil
    ) {
        let failures = FailureCount()
        self.failures = failures
        decoder = TileDecoder { event in
            if case .failed = event {
                failures.pending.add(1, ordering: .relaxed)
                failures.total.add(1, ordering: .relaxed)
            }
            onEvent?(event)
        }
        state = Mutex((StandInPipeline(codec: codec, colorOverride: colorOverride), nil))
    }

    /// The color of the current configuration (nil before the first).
    public var color: StandInColor? { state.withLock { $0.color } }
    public var gated: Int { state.withLock { $0.pipeline.gated } }
    public var decodeFailures: Int { failures.total.load(ordering: .relaxed) }

    public func push(_ annexB: Data, ptsMicros: Int64) {
        var actions: [StandInAction] = []
        state.withLock { s in
            if failures.pending.exchange(0, ordering: .relaxed) > 0 {
                actions += s.pipeline.decodeFailed()
            }
            actions += s.pipeline.push(annexB, ptsMicros: ptsMicros)
            for case .configure(_, _, let color) in actions { s.color = color }
        }
        for action in actions { execute(action) }
    }

    public func execute(_ action: StandInAction) {
        switch action {
        case .configure(let codec, let sets, let color):
            decoder.configure(codec: codec, parameterSets: sets, fullRange: color.fullRange)
        case .decode(let data, let keyframe, let pts):
            decoder.decode(annexB: data, keyframe: keyframe, ptsMicros: pts)
        case .flush: decoder.flush()
        case .reset: decoder.reset()
        case .clear: decoder.clear()
        }
    }
}

/// A simple grid: ceil(sqrt(n)) columns, cells separated by `gap`, each
/// picture fit into its cell with black bars, in whole pixels.
public enum StandInLayout {
    public static func grid(count: Int, viewport: CGSize, gap: CGFloat, pictureSizes: [CGSize?]) -> [CGRect] {
        guard count > 0 else { return [] }
        let columns = Int(Double(count).squareRoot().rounded(.up))
        let rows = (count + columns - 1) / columns
        let cellW = (viewport.width - gap * CGFloat(columns - 1)) / CGFloat(columns)
        let cellH = (viewport.height - gap * CGFloat(rows - 1)) / CGFloat(rows)
        return (0..<count).map { i in
            let cell = CGRect(
                x: CGFloat(i % columns) * (cellW + gap), y: CGFloat(i / columns) * (cellH + gap),
                width: cellW, height: cellH)
            guard i < pictureSizes.count, let size = pictureSizes[i], size.width > 0, size.height > 0 else {
                return cell.integral
            }
            let scale = min(cellW / size.width, cellH / size.height)
            let w = (size.width * scale).rounded()
            let h = (size.height * scale).rounded()
            return CGRect(
                x: (cell.midX - w / 2).rounded(), y: (cell.midY - h / 2).rounded(), width: w, height: h)
        }
    }
}
