import Foundation

/// One coded picture with its parameter sets, as a slice of a file or buffer.
public struct AccessUnit: Sendable, Equatable {
    public let range: Range<Int>
    public let isKeyframe: Bool
}

/// Groups the NAL units of an Annex-B elementary stream into access units.
/// Only used where there is no framing already (files); the core delivers
/// one access unit per frame.
public enum AccessUnitSplitter {
    public static func split(_ data: Data, codec: VideoCodec) -> [AccessUnit] {
        let nals = AnnexB.split(data, codec: codec)
        guard !nals.isEmpty else { return [] }
        var units: [AccessUnit] = []
        var unitStart = 0  // index into `nals`
        var sawVCL = false
        var isKey = false

        func startOffset(of nalIndex: Int) -> Int {
            // Include the start code (3 or 4 bytes) in front of the NAL.
            var s = nals[nalIndex].range.lowerBound - 3
            if s > 0 && data[data.startIndex + s - 1] == 0 { s -= 1 }
            return max(s, 0)
        }
        func close(before nalIndex: Int) {
            units.append(AccessUnit(
                range: startOffset(of: unitStart)..<(nalIndex < nals.count ? startOffset(of: nalIndex) : data.count),
                isKeyframe: isKey))
        }

        for (n, nal) in nals.enumerated() {
            let vcl = NALType.isVCL(nal.type, codec: codec)
            let startsNewUnit: Bool
            if vcl {
                startsNewUnit = sawVCL && firstSliceInPicture(data, nal, codec)
            } else {
                startsNewUnit = sawVCL
            }
            if startsNewUnit {
                close(before: n)
                unitStart = n
                sawVCL = false
                isKey = false
            }
            if vcl {
                sawVCL = true
                if NALType.isKeyframe(nal.type, codec: codec) { isKey = true }
            }
        }
        if sawVCL { close(before: nals.count) }
        return units
    }

    private static func firstSliceInPicture(_ data: Data, _ nal: NALUnit, _ codec: VideoCodec) -> Bool {
        switch codec {
        case .h264:
            // first_mb_in_slice == 0 is ue(v) "1": the top bit of the first payload byte.
            return data[data.startIndex + nal.range.lowerBound + 1] & 0x80 != 0
        case .h265:
            // first_slice_segment_in_pic_flag: top bit after the 2-byte header.
            return data[data.startIndex + nal.range.lowerBound + 2] & 0x80 != 0
        }
    }
}
