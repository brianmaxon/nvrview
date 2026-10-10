import Foundation

/// Video codecs the core delivers.
public enum VideoCodec: Sendable, Equatable {
    case h264
    case h265
}

/// One NAL unit located inside a larger Annex-B buffer.
public struct NALUnit: Sendable, Equatable {
    /// Range of the NAL unit bytes (header included, start code excluded).
    public let range: Range<Int>
    /// The codec-specific NAL unit type (H.264: 5 bits, H.265: 6 bits).
    public let type: UInt8

    public init(range: Range<Int>, type: UInt8) {
        self.range = range
        self.type = type
    }
}

/// NAL unit type classification.
public enum NALType {
    public static func isParameterSet(_ type: UInt8, codec: VideoCodec) -> Bool {
        switch codec {
        case .h264: return type == 7 || type == 8
        case .h265: return type >= 32 && type <= 34
        }
    }

    /// A random-access picture: H.264 IDR (5), H.265 IRAP (16...21).
    public static func isKeyframe(_ type: UInt8, codec: VideoCodec) -> Bool {
        switch codec {
        case .h264: return type == 5
        case .h265: return type >= 16 && type <= 21
        }
    }

    /// Slice data (a coded picture or part of one).
    public static func isVCL(_ type: UInt8, codec: VideoCodec) -> Bool {
        switch codec {
        case .h264: return type >= 1 && type <= 5
        case .h265: return type <= 31
        }
    }
}

public enum AnnexB {
    /// Splits an Annex-B buffer (3- or 4-byte start codes) into NAL units.
    /// Trailing zero bytes belonging to the next start code or to
    /// `trailing_zero_8bits` are not part of a NAL unit. Bytes before the first
    /// start code are ignored.
    public static func split(_ bytes: UnsafeRawBufferPointer, codec: VideoCodec) -> [NALUnit] {
        let count = bytes.count
        var result: [NALUnit] = []
        var nalStart: Int?
        var i = 0
        while i + 2 < count {
            if bytes[i + 2] > 1 {
                i += 3
            } else if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
                if let start = nalStart {
                    append(start, i, bytes, codec, &result)
                }
                nalStart = i + 3
                i += 3
            } else {
                i += 1
            }
        }
        if let start = nalStart {
            append(start, count, bytes, codec, &result)
        }
        return result
    }

    public static func split(_ data: Data, codec: VideoCodec) -> [NALUnit] {
        data.withUnsafeBytes { split($0, codec: codec) }
    }

    private static func append(
        _ start: Int, _ rawEnd: Int, _ bytes: UnsafeRawBufferPointer,
        _ codec: VideoCodec, _ out: inout [NALUnit]
    ) {
        var end = rawEnd
        while end > start && bytes[end - 1] == 0 { end -= 1 }
        guard end > start else { return }
        let header = bytes[start]
        let type: UInt8
        switch codec {
        case .h264: type = header & 0x1F
        case .h265: type = (header >> 1) & 0x3F
        }
        out.append(NALUnit(range: start..<end, type: type))
    }
}
