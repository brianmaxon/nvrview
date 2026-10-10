import CoreMedia
import Foundation

/// Tracks the most recent parameter sets and builds a `CMVideoFormatDescription`
/// from them. Detects changes by comparing bytes, so repeated identical
/// parameter sets in front of every keyframe cost nothing downstream.
public struct ParameterSetTracker: Sendable {
    public let codec: VideoCodec
    private var vps: Data?
    private var sps: Data?
    private var pps: Data?
    public private(set) var formatDescription: CMVideoFormatDescription?

    public init(codec: VideoCodec) {
        self.codec = codec
    }

    /// Forgets everything (used on an explicit format change).
    public mutating func reset() {
        vps = nil
        sps = nil
        pps = nil
        formatDescription = nil
    }

    /// Records one parameter set NAL unit (no start code). Returns true if it
    /// differs from what was held before. Non-parameter-set types are ignored.
    @discardableResult
    public mutating func record(type: UInt8, nal: Data) -> Bool {
        switch (codec, type) {
        case (.h264, 7), (.h265, 33):
            if sps == nal { return false }
            sps = nal
        case (.h264, 8), (.h265, 34):
            if pps == nal { return false }
            pps = nal
        case (.h265, 32):
            if vps == nal { return false }
            vps = nal
        default:
            return false
        }
        return true
    }

    /// Rebuilds the format description if the sets are complete and something
    /// was recorded since the last build. Returns true when the format
    /// description changed (a new, different description was created).
    public mutating func rebuildIfNeeded(dirty: Bool) -> Bool {
        guard dirty else { return false }
        guard let new = makeFormatDescription() else {
            return false
        }
        if let old = formatDescription, CMFormatDescriptionEqual(old, otherFormatDescription: new) {
            return false
        }
        formatDescription = new
        return true
    }

    private func makeFormatDescription() -> CMVideoFormatDescription? {
        var out: CMVideoFormatDescription?
        let status: OSStatus
        switch codec {
        case .h264:
            guard let sps, let pps else { return nil }
            status = Self.withPointers([sps, pps]) { ptrs, sizes in
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: 2,
                    parameterSetPointers: ptrs, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &out)
            }
        case .h265:
            guard let vps, let sps, let pps else { return nil }
            status = Self.withPointers([vps, sps, pps]) { ptrs, sizes in
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: 3,
                    parameterSetPointers: ptrs, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &out)
            }
        }
        return status == noErr ? out : nil
    }

    private static func withPointers<R>(
        _ sets: [Data],
        _ body: ([UnsafePointer<UInt8>], [Int]) -> R
    ) -> R {
        // Copy into one stable allocation so the pointers stay valid for the call.
        let total = sets.reduce(0) { $0 + $1.count }
        let storage = UnsafeMutablePointer<UInt8>.allocate(capacity: max(total, 1))
        defer { storage.deallocate() }
        var ptrs: [UnsafePointer<UInt8>] = []
        var sizes: [Int] = []
        var offset = 0
        for set in sets {
            _ = set.copyBytes(to: UnsafeMutableBufferPointer(start: storage + offset, count: set.count))
            ptrs.append(UnsafePointer(storage + offset))
            sizes.append(set.count)
            offset += set.count
        }
        return body(ptrs, sizes)
    }
}
