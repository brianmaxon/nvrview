import AVFoundation
import CoreGraphics
import CoreVideo
import Foundation
import VideoKit

/// Synthetic recordings for checking the follow cut and grids (docs/UX.md
/// "Recordings", "Follow and Grid"): MP4 files written with VideoToolbox
/// (through AVAssetWriter) whose every picture says which camera it is and
/// which frame it is. The top three quarters are one of `palette`'s colors;
/// the bottom quarter is `bits` black or white blocks across the width, the
/// frame number in binary (most significant bit on the left). Made up
/// entirely; nothing comes from a device.
public enum SyntheticRecording {
    /// The cameras' colors (RGB), one per source.
    public static let palette: [SIMD3<Int>] = [
        SIMD3(230, 40, 40), SIMD3(40, 200, 60), SIMD3(40, 70, 230), SIMD3(230, 210, 40),
    ]
    /// Blocks in the frame-number strip.
    public static let bits = 10

    public struct Spec: Sendable, Equatable {
        public var width: Int
        public var height: Int
        public var fps: Int
        public var frames: Int
        /// Frames from one keyframe to the next.
        public var keyframeInterval: Int
        /// Index into `palette`.
        public var color: Int

        public init(width: Int = 640, height: Int = 360, fps: Int = 10, frames: Int, keyframeInterval: Int = 20, color: Int) {
            self.width = width
            self.height = height
            self.fps = fps
            self.frames = frames
            self.keyframeInterval = keyframeInterval
            self.color = color
        }
    }

    public enum WriteError: Error { case setup(String), failed(String) }

    /// Writes an H.264 MP4 (BT.601, video range, flagged that way).
    public static func write(_ spec: Spec, to url: URL) async throws {
        try? FileManager.default.removeItem(at: url)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        let input = AVAssetWriterInput(
            mediaType: .video,
            outputSettings: [
                AVVideoCodecKey: AVVideoCodecType.h264,
                AVVideoWidthKey: spec.width,
                AVVideoHeightKey: spec.height,
                AVVideoCompressionPropertiesKey: [
                    AVVideoMaxKeyFrameIntervalKey: spec.keyframeInterval,
                    AVVideoAllowFrameReorderingKey: false,
                    AVVideoAverageBitRateKey: spec.width * spec.height * spec.fps,
                    AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                ],
                AVVideoColorPropertiesKey: [
                    AVVideoColorPrimariesKey: AVVideoColorPrimaries_SMPTE_C,
                    AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
                    AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_601_4,
                ],
            ])
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                kCVPixelBufferWidthKey as String: spec.width,
                kCVPixelBufferHeightKey as String: spec.height,
                kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
            ])
        guard writer.canAdd(input) else { throw WriteError.setup("input") }
        writer.add(input)
        guard writer.startWriting() else { throw WriteError.setup(String(describing: writer.error)) }
        writer.startSession(atSourceTime: .zero)
        let job = Job(writer: writer, input: input, adaptor: adaptor, spec: spec)
        try await job.run()
    }

    /// The writer's state, used on its queue only.
    private final class Job: @unchecked Sendable {
        let writer: AVAssetWriter
        let input: AVAssetWriterInput
        let adaptor: AVAssetWriterInputPixelBufferAdaptor
        let spec: Spec
        var frame = 0
        let queue = DispatchQueue(label: "nvrview.synthetic-recording")

        init(writer: AVAssetWriter, input: AVAssetWriterInput, adaptor: AVAssetWriterInputPixelBufferAdaptor, spec: Spec) {
            self.writer = writer
            self.input = input
            self.adaptor = adaptor
            self.spec = spec
        }

        func run() async throws {
            try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, any Error>) in
                input.requestMediaDataWhenReady(on: queue) { [self] in
                    while input.isReadyForMoreMediaData, frame < spec.frames {
                        guard let pool = adaptor.pixelBufferPool, let buffer = SyntheticRecording.picture(spec, frame: frame, pool: pool),
                            adaptor.append(buffer, withPresentationTime: CMTime(value: Int64(frame), timescale: Int32(spec.fps)))
                        else {
                            frame = spec.frames + 1
                            input.markAsFinished()
                            writer.cancelWriting()
                            done.resume(throwing: WriteError.failed(String(describing: writer.error)))
                            return
                        }
                        frame += 1
                    }
                    guard frame == spec.frames else { return }
                    frame += 1
                    input.markAsFinished()
                    writer.finishWriting { [self] in
                        if writer.status == .completed {
                            done.resume()
                        } else {
                            done.resume(throwing: WriteError.failed(String(describing: writer.error)))
                        }
                    }
                }
            }
        }
    }

    /// BT.601 video-range Y, Cb, Cr of an RGB color.
    static func ycbcr(_ c: SIMD3<Int>) -> (y: UInt8, cb: UInt8, cr: UInt8) {
        let r = Double(c.x) / 255, g = Double(c.y) / 255, b = Double(c.z) / 255
        func clamp(_ v: Double) -> UInt8 { UInt8(min(max(v.rounded(), 0), 255)) }
        return (
            clamp(16 + 65.481 * r + 128.553 * g + 24.966 * b),
            clamp(128 - 37.797 * r - 74.203 * g + 112.0 * b),
            clamp(128 + 112.0 * r - 93.786 * g - 18.214 * b)
        )
    }

    static func picture(_ spec: Spec, frame: Int, pool: CVPixelBufferPool) -> CVPixelBuffer? {
        var out: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &out) == kCVReturnSuccess, let buffer = out else { return nil }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let w = spec.width, h = spec.height
        let strip = h * 3 / 4
        let color = ycbcr(palette[spec.color % palette.count])
        let block = w / bits
        func bit(_ x: Int) -> Bool {
            let i = min(x / max(block, 1), bits - 1)
            return (frame >> (bits - 1 - i)) & 1 == 1
        }
        guard let yBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0)?.assumingMemoryBound(to: UInt8.self),
            let cBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1)?.assumingMemoryBound(to: UInt8.self)
        else { return nil }
        let yStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let cStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        for y in 0..<h {
            let row = yBase + y * yStride
            if y < strip {
                memset(row, Int32(color.y), w)
            } else {
                for x in 0..<w { row[x] = bit(x) ? 235 : 16 }
            }
        }
        for y in 0..<(h / 2) {
            let row = cBase + y * cStride
            let top = y * 2 < strip
            for x in 0..<(w / 2) {
                row[x * 2] = top ? color.cb : 128
                row[x * 2 + 1] = top ? color.cr : 128
            }
        }
        return buffer
    }

    /// What a rendered picture of a synthetic recording says, read from
    /// `picture` (where the picture is drawn, in the image's pixels): the
    /// camera's color (an index into `palette`, nil when none is within
    /// `tolerance` per channel) and the frame number.
    public static func read(_ image: RenderedImage, picture: CGRect, tolerance: Int = 40) -> (color: Int?, frame: Int) {
        func px(_ fx: Double, _ fy: Double) -> SIMD3<Int> {
            let x = min(max(Int(picture.minX + picture.width * fx), 0), image.width - 1)
            let y = min(max(Int(picture.minY + picture.height * fy), 0), image.height - 1)
            return image.rgb(x: x, y: y)
        }
        let c = px(0.5, 0.375)
        let color = palette.firstIndex { p in
            abs(p.x - c.x) <= tolerance && abs(p.y - c.y) <= tolerance && abs(p.z - c.z) <= tolerance
        }
        var frame = 0
        for i in 0..<bits {
            let v = px((Double(i) + 0.5) / Double(bits), 0.875)
            frame = frame << 1 | ((v.x + v.y + v.z) / 3 > 128 ? 1 : 0)
        }
        return (color, frame)
    }
}
