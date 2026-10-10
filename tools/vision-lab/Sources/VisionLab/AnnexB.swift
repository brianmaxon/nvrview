import Foundation

/// The two codecs the sub-streams and main streams use.
public enum Codec: String, Sendable {
    case h264
    case h265

    /// Guesses the codec from a file extension (`.h264`/`.264`, `.h265`/`.265`/`.hevc`).
    public static func from(path: String) -> Codec? {
        switch (path as NSString).pathExtension.lowercased() {
        case "h264", "264": return .h264
        case "h265", "265", "hevc": return .h265
        default: return nil
        }
    }
}

/// One NAL unit without its start code.
public struct NALUnit: Sendable {
    public let bytes: Data

    public init(bytes: Data) { self.bytes = bytes }

    /// The NAL unit type for the given codec.
    public func type(_ codec: Codec) -> Int {
        guard let first = bytes.first else { return -1 }
        switch codec {
        case .h264: return Int(first & 0x1F)
        case .h265: return Int((first >> 1) & 0x3F)
        }
    }

    /// True for slice data (a coded picture or part of one).
    public func isVCL(_ codec: Codec) -> Bool {
        let t = type(codec)
        switch codec {
        case .h264: return t >= 1 && t <= 5
        case .h265: return t >= 0 && t <= 31
        }
    }

    /// True for an IDR (H.264) or IRAP (H.265) slice.
    public func isKeyframe(_ codec: Codec) -> Bool {
        let t = type(codec)
        switch codec {
        case .h264: return t == 5
        case .h265: return t >= 16 && t <= 23
        }
    }

    /// True when this slice starts a new picture: `first_mb_in_slice == 0`
    /// (H.264, the first Exp-Golomb bit is 1) or
    /// `first_slice_segment_in_pic_flag` (H.265).
    public func startsPicture(_ codec: Codec) -> Bool {
        let headerLength = codec == .h264 ? 1 : 2
        guard isVCL(codec), bytes.count > headerLength else { return false }
        return bytes[bytes.startIndex + headerLength] & 0x80 != 0
    }
}

/// Splits an Annex-B byte stream into NAL units (3- or 4-byte start codes).
public func splitAnnexB(_ data: Data) -> [NALUnit] {
    var units: [NALUnit] = []
    data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
        let p = raw.bindMemory(to: UInt8.self)
        let n = p.count
        var starts: [(payload: Int, code: Int)] = []
        var i = 0
        while i + 2 < n {
            if p[i] == 0 && p[i + 1] == 0 && p[i + 2] == 1 {
                let codeStart = (i > 0 && p[i - 1] == 0) ? i - 1 : i
                starts.append((i + 3, codeStart))
                i += 3
            } else {
                i += 1
            }
        }
        for (k, s) in starts.enumerated() {
            var end = k + 1 < starts.count ? starts[k + 1].code : n
            // Trailing zero bytes belong to the next start code or padding.
            while end > s.payload && p[end - 1] == 0 { end -= 1 }
            if end > s.payload {
                units.append(NALUnit(bytes: Data(raw[s.payload..<end])))
            }
        }
    }
    return units
}

/// A coded picture and the parameter sets in force for it.
public struct AccessUnit: Sendable {
    /// Index of this picture in the stream (0-based, decode order).
    public let index: Int
    public let isKeyframe: Bool
    /// VPS (H.265 only), SPS and PPS, in that order.
    public let parameterSets: [Data]
    public let slices: [NALUnit]
}

/// Groups NAL units into pictures and tracks the latest parameter sets.
public func accessUnits(_ nals: [NALUnit], codec: Codec) -> [AccessUnit] {
    var vps: Data?
    var sps: Data?
    var pps: Data?
    var out: [AccessUnit] = []
    var current: [NALUnit] = []

    func params() -> [Data] {
        switch codec {
        case .h264: return [sps, pps].compactMap { $0 }
        case .h265: return [vps, sps, pps].compactMap { $0 }
        }
    }
    func flush() {
        guard !current.isEmpty else { return }
        let key = current.contains { $0.isKeyframe(codec) }
        out.append(AccessUnit(index: out.count, isKeyframe: key, parameterSets: params(), slices: current))
        current = []
    }

    for nal in nals {
        let t = nal.type(codec)
        switch (codec, t) {
        case (.h264, 7), (.h265, 33):
            flush()
            sps = nal.bytes
        case (.h264, 8), (.h265, 34):
            flush()
            pps = nal.bytes
        case (.h265, 32):
            flush()
            vps = nal.bytes
        default:
            if nal.isVCL(codec) {
                if nal.startsPicture(codec) { flush() }
                current.append(nal)
            }
        // SEI, AUD and the rest are not needed to decode.
        }
    }
    flush()
    return out
}
