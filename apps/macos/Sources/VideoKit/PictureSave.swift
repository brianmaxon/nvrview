import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// One decoded picture held fixed, for the renderer to draw once.
final class StillPictureSource: TilePictureSource {
    private let picture: DecodedPicture

    init(_ picture: DecodedPicture) { self.picture = picture }

    func latestPicture() -> DecodedPicture? { picture }
    var pictureSequence: UInt64 { picture.sequence }
}

/// What to draw into a saved picture (docs/CAPTURE.md "Save picture").
public struct PictureRender: Sendable {
    public var picture: DecodedPicture
    public var color: YCbCrToRGB
    /// The part of the buffer to draw, in its pixels (the whole visible
    /// picture, or a zoomed region). Not used with `dewarp`.
    public var source: CGRect
    /// A fisheye view: drawn through the dewarp pipeline over the whole
    /// image instead of copying `source`.
    public var dewarp: DewarpConstants?
    /// The image's size in pixels: `source`'s for a copy, the dewarped
    /// view's for a fisheye.
    public var width: Int
    public var height: Int

    public init(
        picture: DecodedPicture, color: YCbCrToRGB, source: CGRect, dewarp: DewarpConstants? = nil, width: Int,
        height: Int
    ) {
        self.picture = picture
        self.color = color
        self.source = source
        self.dewarp = dewarp
        self.width = width
        self.height = height
    }
}

/// The offscreen path of Save Picture: the renderer's shaders and color
/// matrix draw the tile's own pixel buffer (no second decode) into a texture
/// of the saved size, which is read back and encoded with ImageIO. Work runs
/// on its own serial queue, never the main thread.
public final class PictureRenderer: @unchecked Sendable {
    // @unchecked Sendable: the renderer is touched only on `queue`.
    private let queue = DispatchQueue(label: "nvrview.picture-save", qos: .userInitiated)
    private var renderer: GridRenderer?

    public init() {}

    /// Renders and encodes `job`; nil if Metal, the picture or the encoder
    /// failed.
    public func encode(_ job: PictureRender, format: PictureEncoding) async -> Data? {
        await withCheckedContinuation { continuation in
            queue.async { [self] in
                continuation.resume(returning: renderAndEncode(job, format))
            }
        }
    }

    /// The rendered image alone (tests).
    public func render(_ job: PictureRender) async -> RenderedImage? {
        await withCheckedContinuation { continuation in
            queue.async { [self] in
                continuation.resume(returning: renderNow(job))
            }
        }
    }

    private func renderAndEncode(_ job: PictureRender, _ format: PictureEncoding) -> Data? {
        renderNow(job).flatMap { PictureEncoder.encode($0, as: format) }
    }

    private func renderNow(_ job: PictureRender) -> RenderedImage? {
        guard job.width > 0, job.height > 0 else { return nil }
        if renderer == nil { renderer = try? GridRenderer() }
        guard let renderer else { return nil }
        // A new still has its own sequence numbers: forget the last one's.
        renderer.releaseTextures()
        let target = CGRect(x: 0, y: 0, width: job.width, height: job.height)
        renderer.tiles = [
            job.dewarp.map {
                GridTile(picture: StillPictureSource(job.picture), color: job.color, destination: target, dewarp: $0)
            } ?? GridTile(
                picture: StillPictureSource(job.picture), color: job.color, destination: target, source: job.source)
        ]
        return renderer.renderOffscreen(width: job.width, height: job.height)
    }
}

/// The file formats of saved pictures.
public enum PictureEncoding: Sendable {
    case png
    /// Quality 0...1.
    case jpeg(quality: Double)
}

public enum PictureEncoder {
    /// A rendered frame as a `CGImage` (sRGB, no alpha).
    public static func cgImage(_ image: RenderedImage) -> CGImage? {
        guard let provider = CGDataProvider(data: Data(image.bgra) as CFData) else { return nil }
        return CGImage(
            width: image.width, height: image.height, bitsPerComponent: 8, bitsPerPixel: 32,
            bytesPerRow: image.width * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    /// PNG or JPEG bytes with no metadata beyond what the format needs.
    public static func encode(_ image: RenderedImage, as format: PictureEncoding) -> Data? {
        guard let cg = cgImage(image) else { return nil }
        let data = NSMutableData()
        let type: UTType
        var options: [CFString: Any] = [:]
        switch format {
        case .png: type = .png
        case .jpeg(let quality):
            type = .jpeg
            options[kCGImageDestinationLossyCompressionQuality] = quality
        }
        guard let destination = CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, cg, options as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }

    /// An image file's pixels as BGRA (tests and the self-test).
    public static func decode(_ data: Data) -> RenderedImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
            let cg = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { return nil }
        let w = cg.width, h = cg.height
        var bytes = [UInt8](repeating: 0, count: w * h * 4)
        let drawn = bytes.withUnsafeMutableBytes { buffer -> Bool in
            guard
                let context = CGContext(
                    data: buffer.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
            else { return false }
            context.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        return drawn ? RenderedImage(width: w, height: h, bgra: bytes) : nil
    }
}
