import Accelerate
import CoreImage
import CoreVideo
import Foundation

/// A decoded picture handed to the detector. The detector only reads it.
public struct DetectorPicture: @unchecked Sendable {
    public let buffer: CVPixelBuffer

    public init(_ buffer: CVPixelBuffer) { self.buffer = buffer }

    public var width: Int { CVPixelBufferGetWidth(buffer) }
    public var height: Int { CVPixelBufferGetHeight(buffer) }
}

public enum PreprocessError: Error, Equatable {
    case cannotAllocate
    case cannotRender
}

/// Cuts a crop out of a picture and scales it to the model's square BGRA
/// input. BGRA pictures go through vImage on the CPU; any other format (the
/// decoder's biplanar YCbCr, for one) through Core Image, which converts the
/// color. Use from one queue at a time (it reuses its buffer pool).
public final class CropPreprocessor: @unchecked Sendable {
    public let size: Int
    public let fit: InputFit
    private var pool: CVPixelBufferPool?
    private lazy var context = CIContext(options: [.cacheIntermediates: false])

    public init(size: Int = 640, fit: InputFit = .stretch) {
        self.size = size
        self.fit = fit
    }

    private func makeBuffer() throws -> CVPixelBuffer {
        if pool == nil {
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey: size,
                kCVPixelBufferHeightKey: size,
                kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
            ]
            CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &pool)
        }
        var buffer: CVPixelBuffer?
        guard let pool, CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer) == kCVReturnSuccess, let buffer
        else { throw PreprocessError.cannotAllocate }
        return buffer
    }

    /// The model input for `crop` of `picture`.
    public func input(for crop: NormRect, of picture: DetectorPicture) throws -> CVPixelBuffer {
        let r = CropGeometry.pixelRect(of: crop, pictureWidth: picture.width, pictureHeight: picture.height)
        let content = CropGeometry.contentRect(cropPixelWidth: r.width, cropPixelHeight: r.height, fit: fit)
        let out = try makeBuffer()
        // Destination rectangle inside the square input, in pixels.
        let dx = Int((content.x * Double(size)).rounded()), dy = Int((content.y * Double(size)).rounded())
        let dw = max(1, Int((content.width * Double(size)).rounded())), dh = max(1, Int((content.height * Double(size)).rounded()))

        if CVPixelBufferGetPixelFormatType(picture.buffer) == kCVPixelFormatType_32BGRA {
            try scaleBGRA(picture.buffer, from: r, into: out, at: (dx, dy, dw, dh))
        } else {
            try renderCI(picture.buffer, from: r, into: out, at: (dx, dy, dw, dh))
        }
        return out
    }

    private func scaleBGRA(
        _ source: CVPixelBuffer, from r: (x: Int, y: Int, width: Int, height: Int), into out: CVPixelBuffer,
        at d: (x: Int, y: Int, width: Int, height: Int)
    ) throws {
        CVPixelBufferLockBaseAddress(source, .readOnly)
        CVPixelBufferLockBaseAddress(out, [])
        defer {
            CVPixelBufferUnlockBaseAddress(out, [])
            CVPixelBufferUnlockBaseAddress(source, .readOnly)
        }
        guard let sBase = CVPixelBufferGetBaseAddress(source), let oBase = CVPixelBufferGetBaseAddress(out)
        else { throw PreprocessError.cannotRender }
        let sRow = CVPixelBufferGetBytesPerRow(source), oRow = CVPixelBufferGetBytesPerRow(out)
        if d.width != size || d.height != size { memset(oBase, 0, oRow * size) }
        var src = vImage_Buffer(
            data: sBase + r.y * sRow + r.x * 4, height: vImagePixelCount(r.height),
            width: vImagePixelCount(r.width), rowBytes: sRow)
        var dst = vImage_Buffer(
            data: oBase + d.y * oRow + d.x * 4, height: vImagePixelCount(d.height),
            width: vImagePixelCount(d.width), rowBytes: oRow)
        guard vImageScale_ARGB8888(&src, &dst, nil, vImage_Flags(kvImageHighQualityResampling)) == kvImageNoError
        else { throw PreprocessError.cannotRender }
    }

    private func renderCI(
        _ source: CVPixelBuffer, from r: (x: Int, y: Int, width: Int, height: Int), into out: CVPixelBuffer,
        at d: (x: Int, y: Int, width: Int, height: Int)
    ) throws {
        let height = CVPixelBufferGetHeight(source)
        // Core Image's origin is the bottom left.
        let cropRect = CGRect(x: r.x, y: height - r.y - r.height, width: r.width, height: r.height)
        let image = CIImage(cvPixelBuffer: source).cropped(to: cropRect)
            .transformed(by: CGAffineTransform(translationX: -cropRect.minX, y: -cropRect.minY))
            .transformed(by: CGAffineTransform(
                scaleX: CGFloat(d.width) / CGFloat(r.width), y: CGFloat(d.height) / CGFloat(r.height)))
            .transformed(by: CGAffineTransform(translationX: CGFloat(d.x), y: CGFloat(size - d.y - d.height)))
        let canvas = CGRect(x: 0, y: 0, width: size, height: size)
        let black = CIImage(color: .black).cropped(to: canvas)
        context.render(image.composited(over: black), to: out, bounds: canvas, colorSpace: CGColorSpaceCreateDeviceRGB())
    }
}
