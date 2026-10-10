import NVRViewCore
import VideoKit

// Conversions between the C structs of docs/nvrview.h and the Swift types,
// and the two @convention(c) callbacks the core calls. None of this calls a C
// function, so it builds and tests without the library; `CCore` (target
// NVRCoreC) is the only code that does.

extension CoreStatus {
    public init(_ status: NvStatus) { self.init(status.rawValue) }
}

extension NvDecodeFailure {
    public init(_ failure: CoreDecodeFailure) { self.init(rawValue: UInt32(failure.rawValue)) }
}

extension NvStreamTier {
    public init(_ tier: StreamTier) { self.init(rawValue: tier.rawValue) }
}

extension PixelSize {
    public init(_ s: NvSize) { self.init(width: s.width, height: s.height) }
}

extension NvSize {
    public init(_ s: PixelSize) { self.init(width: s.width, height: s.height) }
}

extension PixelRect {
    public init(_ r: NvRect) { self.init(x: r.x, y: r.y, width: r.width, height: r.height) }
}

extension TileLayout {
    public init(_ l: NvTileLayout) {
        self.init(cell: PixelRect(l.cell), dest: PixelRect(l.dest), source: PixelRect(l.source))
    }
}

extension CoreColor {
    public init?(_ c: NvColor) {
        guard let matrix = ColorMatrix(rawValue: c.matrix.rawValue) else { return nil }
        let m = withUnsafeBytes(of: c.ycbcr_to_rgb) { Array($0.bindMemory(to: Float.self)) }
        self.init(matrix: matrix, fullRange: c.full_range, ycbcrToRGB: m)
    }
}

extension CoreAction {
    /// Reads the member of `NvAction` that matches its kind. The result
    /// borrows `action`'s pointers: use it only inside the callback. Nil for
    /// a kind or value this version does not know (such an action is ignored).
    public init?(_ action: NvAction) {
        switch action.kind {
        case NV_ACTION_CONFIGURE:
            let c = action.configure
            let codec: VideoCodec
            switch c.codec {
            case NV_CODEC_H264: codec = .h264
            case NV_CODEC_H265: codec = .h265
            default: return nil
            }
            guard let color = CoreColor(c.color) else { return nil }
            var sets: [UnsafeRawBufferPointer] = []
            if let base = c.parameter_sets {
                for i in 0..<c.parameter_set_count {
                    sets.append(UnsafeRawBufferPointer(start: base[i].data, count: base[i].len))
                }
            }
            self = .configure(
                CoreConfigure(
                    codec: codec, parameterSets: sets, codedSize: PixelSize(c.coded_size),
                    visibleRect: PixelRect(c.visible_rect), color: color))
        case NV_ACTION_DECODE:
            let d = action.decode
            self = .decode(
                CoreDecode(
                    accessUnit: UnsafeRawBufferPointer(start: d.access_unit.data, count: d.access_unit.len),
                    keyframe: d.keyframe, ptsMicros: d.pts_us))
        case NV_ACTION_FLUSH: self = .flush
        case NV_ACTION_RESET: self = .reset
        case NV_ACTION_CLEAR: self = .clear
        case NV_ACTION_STATUS:
            guard let status = TileStatus(rawValue: action.status.rawValue) else { return nil }
            self = .status(status)
        default:
            return nil
        }
    }
}

/// What the C callbacks' `context` points at.
public final class CallbackContext: Sendable {
    public let sink: any CoreSink
    init(sink: any CoreSink) { self.sink = sink }
}

/// Builds the `NvCallbacks` for a sink. The context is retained by the
/// returned `Unmanaged`; whoever creates the core releases it after
/// `nv_core_free` returns (no callback runs after that).
public enum CCallbacks {
    public static func make(sink: any CoreSink) -> (callbacks: NvCallbacks, context: Unmanaged<CallbackContext>) {
        let context = Unmanaged.passRetained(CallbackContext(sink: sink))
        let callbacks = NvCallbacks(
            context: context.toOpaque(),
            on_action: { context, tile, action in
                guard let context, let action else { return }
                let sink = Unmanaged<CallbackContext>.fromOpaque(context).takeUnretainedValue().sink
                // Borrowed pointers: handled synchronously, never kept.
                guard let converted = CoreAction(action.pointee) else { return }
                sink.core(didDeliver: converted, tile: tile)
            },
            on_event: { context, json, length in
                guard let context, let json, length > 0 else { return }
                let sink = Unmanaged<CallbackContext>.fromOpaque(context).takeUnretainedValue().sink
                sink.core(didEmit: UnsafeRawBufferPointer(start: json, count: length))
            })
        return (callbacks, context)
    }
}
