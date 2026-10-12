import CoreGraphics
import CoreMedia
import Foundation
import ImageIO
import VideoToolbox

/// Turns one keyframe (Annex-B with its parameter sets, as a recording's
/// cover thumbnail arrives from the core) into a small JPEG: decoded with
/// VideoToolbox, shrunk, encoded with ImageIO. Synchronous; call it off the
/// main thread. No state is kept between calls.
public enum KeyframeImage {
    /// A small image and its size.
    public struct Small: Sendable, Equatable {
        public var jpeg: Data
        public var width: Int
        public var height: Int
    }

    /// Decodes the keyframe into a picture (nil when it does not decode).
    public static func decode(annexB: Data, codec: VideoCodec) -> CGImage? {
        annexB.withUnsafeBytes { raw -> CGImage? in
            let nals = AnnexB.split(raw, codec: codec)
            var tracker = ParameterSetTracker(codec: codec)
            var dirty = false
            for nal in nals where NALType.isParameterSet(nal.type, codec: codec) {
                let bytes = Data(raw[nal.range])
                if tracker.record(type: nal.type, nal: bytes) { dirty = true }
            }
            _ = tracker.rebuildIfNeeded(dirty: dirty)
            let slices = nals.filter { NALType.isVCL($0.type, codec: codec) }
            guard let format = tracker.formatDescription,
                slices.contains(where: { NALType.isKeyframe($0.type, codec: codec) }),
                let sample = SampleBuilder.makeSample(
                    nals: slices, from: raw, format: format, ptsMicros: 0, isKeyframe: true)
            else { return nil }
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
            ]
            var session: VTDecompressionSession?
            guard VTDecompressionSessionCreate(
                allocator: kCFAllocatorDefault, formatDescription: format, decoderSpecification: nil,
                imageBufferAttributes: attributes as CFDictionary, outputCallback: nil,
                decompressionSessionOut: &session) == noErr, let session
            else { return nil }
            defer { VTDecompressionSessionInvalidate(session) }
            final class Result: @unchecked Sendable { var image: CGImage? }
            let result = Result()
            let status = VTDecompressionSessionDecodeFrame(
                session, sampleBuffer: sample, flags: [], infoFlagsOut: nil
            ) { status, _, buffer, _, _ in
                guard status == noErr, let buffer else { return }
                var image: CGImage?
                VTCreateCGImageFromCVPixelBuffer(buffer, options: nil, imageOut: &image)
                result.image = image
            }
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            return status == noErr ? result.image : nil
        }
    }

    /// Shrinks `image` to fit `maxSide` points on its longer side (never
    /// enlarges) and encodes it as JPEG.
    public static func small(_ image: CGImage, maxSide: Int = 320, quality: Double = 0.7) -> Small? {
        let w = image.width, h = image.height
        guard w > 0, h > 0 else { return nil }
        let scale = min(1, Double(maxSide) / Double(max(w, h)))
        let tw = max(1, Int((Double(w) * scale).rounded())), th = max(1, Int((Double(h) * scale).rounded()))
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
            let ctx = CGContext(
                data: nil, width: tw, height: th, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: tw, height: th))
        guard let scaled = ctx.makeImage() else { return nil }
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, "public.jpeg" as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(dest, scaled, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return Small(jpeg: out as Data, width: tw, height: th)
    }

    /// Decode and shrink in one step.
    public static func thumbnail(annexB: Data, codec: VideoCodec, maxSide: Int = 320) -> Small? {
        decode(annexB: annexB, codec: codec).flatMap { small($0, maxSide: maxSide) }
    }
}
