import CoreGraphics
import CoreVideo
import Foundation
import VideoToolbox
import VideoKit

/// A picture decoded for a verification request.
public struct CheckedPicture: @unchecked Sendable {
    // @unchecked: the buffer is never written after the decoder hands it over.
    public let buffer: CVPixelBuffer
    public var width: Int { CVPixelBufferGetWidth(buffer) }
    public var height: Int { CVPixelBufferGetHeight(buffer) }

    /// The picture as an image (for the snapshot).
    public func image() -> CGImage? {
        var image: CGImage?
        VTCreateCGImageFromCVPixelBuffer(buffer, options: nil, imageOut: &image)
        return image
    }
}

/// Decodes the access units of one verification request (a keyframe, with
/// its parameter sets in front, up to the picture to check) with
/// VideoToolbox, and returns the last picture. Synchronous; call it off the
/// main thread.
public enum PictureDecoder {
    public enum Failure: Error, Equatable {
        case noAccessUnits
        case noParameterSets
        case noPicture
    }

    public static func decode(annexB: Data, codec: VideoCodec) throws -> CheckedPicture {
        let units = AccessUnitSplitter.split(annexB, codec: codec)
        guard let first = units.first else { throw Failure.noAccessUnits }
        var sets: [Data] = []
        for nal in AnnexB.split(annexB.subdata(in: first.range), codec: codec)
        where NALType.isParameterSet(nal.type, codec: codec) {
            let base = first.range.lowerBound
            sets.append(annexB.subdata(in: (base + nal.range.lowerBound)..<(base + nal.range.upperBound)))
        }
        guard !sets.isEmpty else { throw Failure.noParameterSets }
        let decoder = TileDecoder(onEvent: { _ in })
        decoder.configure(codec: codec, parameterSets: sets, fullRange: false)
        for (index, unit) in units.enumerated() {
            decoder.decode(
                annexB: annexB.subdata(in: unit.range), keyframe: unit.isKeyframe, ptsMicros: Int64(index) * 1000)
        }
        decoder.waitForPendingFrames()
        guard let picture = decoder.latestPicture() else { throw Failure.noPicture }
        return CheckedPicture(buffer: picture.pixelBuffer)
    }
}
