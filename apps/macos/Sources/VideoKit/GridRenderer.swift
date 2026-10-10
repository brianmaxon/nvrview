import CoreGraphics
import CoreVideo
import Foundation
import Metal
import QuartzCore

public enum GridRendererError: Error, Equatable {
    case noMetalDevice
    case setupFailed(String)
}

/// Draws a grid of tiles in one Metal render pass: each tile's newest NV12
/// picture, sampled as an `r8Unorm` luma and an `rg8Unorm` chroma texture
/// straight from its IOSurface (no CPU copy), converted with the tile's
/// matrix, into the tile's destination rectangle. Everything else is black.
///
/// It decides nothing: rectangles and matrices come from the pipeline. Use
/// it from one thread at a time. Steady state allocates nothing of its own
/// per frame; Metal's command buffer and encoder are the only per-frame
/// objects, and texture wrappers are made only when a tile has a new picture.
public final class GridRenderer {
    public static let pixelFormat = MTLPixelFormat.bgra8Unorm
    /// Frames the CPU may encode ahead of the GPU.
    public static let maxFramesInFlight = 3

    public let device: any MTLDevice

    /// The tiles to draw, in the pipeline's layout.
    public var tiles: [GridTile] = [] {
        didSet { layoutChanged = true }
    }

    // Must match `TileUniforms` in `gridShaderSource`.
    private struct TileUniforms {
        var destination: SIMD4<Float>
        var source: SIMD4<Float>
        var r: SIMD4<Float>
        var g: SIMD4<Float>
        var b: SIMD4<Float>
    }

    private struct TileTextures {
        let luma: CVMetalTexture
        let chroma: CVMetalTexture
        let lumaTexture: any MTLTexture
        let chromaTexture: any MTLTexture
        let width: Int
        let height: Int
    }

    private let queue: any MTLCommandQueue
    private let pipeline: any MTLRenderPipelineState
    /// Fisheye tiles (`GridTile.dewarp`): the same vertices, `dewarp_fragment`.
    private let dewarpPipeline: any MTLRenderPipelineState
    private let textureCache: CVMetalTextureCache
    private let passDescriptor = MTLRenderPassDescriptor()

    /// Per tile: the picture sequence last looked at and its textures.
    private var seen: [UInt64] = []
    private var textures: [TileTextures?] = []
    /// Per tile: host times of new pictures drawn (the rate shown), and the
    /// picture source they were counted for.
    private var shown: [RateMeter] = []
    private var shownSources: [ObjectIdentifier?] = []
    private var layoutChanged = true
    private var lastTargetSize = (width: 0, height: 0)

    /// The textures each frame in flight samples, kept alive (and their
    /// pixel buffers out of the decoder's pool) until that frame completes.
    private var inFlight: [(commandBuffer: (any MTLCommandBuffer)?, textures: [CVMetalTexture])]
    private var frameIndex = 0

    private var offscreen: (any MTLTexture)?
    private var readback: [UInt8] = []

    public init(device: (any MTLDevice)? = nil) throws(GridRendererError) {
        guard let device = device ?? MTLCreateSystemDefaultDevice() else { throw .noMetalDevice }
        guard let queue = device.makeCommandQueue() else { throw .setupFailed("command queue") }
        self.device = device
        self.queue = queue
        queue.label = "nvrview.grid"

        let library: any MTLLibrary
        do {
            library = try device.makeLibrary(source: gridShaderSource, options: nil)
        } catch {
            throw .setupFailed("shader: \(error)")
        }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "grid tile"
        descriptor.vertexFunction = library.makeFunction(name: "tile_vertex")
        descriptor.fragmentFunction = library.makeFunction(name: "tile_fragment")
        descriptor.colorAttachments[0].pixelFormat = Self.pixelFormat
        do {
            pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
            descriptor.label = "grid dewarp tile"
            descriptor.fragmentFunction = library.makeFunction(name: "dewarp_fragment")
            dewarpPipeline = try device.makeRenderPipelineState(descriptor: descriptor)
        } catch {
            throw .setupFailed("pipeline: \(error)")
        }

        var cache: CVMetalTextureCache?
        guard CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &cache) == kCVReturnSuccess,
            let cache
        else { throw .setupFailed("texture cache") }
        textureCache = cache

        passDescriptor.colorAttachments[0].loadAction = .clear
        passDescriptor.colorAttachments[0].storeAction = .store
        passDescriptor.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)

        inFlight = Array(repeating: (nil, []), count: Self.maxFramesInFlight)
        for i in inFlight.indices { inFlight[i].textures.reserveCapacity(16) }
    }

    /// Whether a frame drawn now would differ from the last one: the layout
    /// or target size changed, or some tile has a new picture (or was cleared).
    public func needsDisplay(width: Int, height: Int) -> Bool {
        if layoutChanged || lastTargetSize != (width, height) || seen.count != tiles.count { return true }
        for (i, tile) in tiles.enumerated() where tile.picture.pictureSequence != seen[i] {
            return true
        }
        return false
    }

    /// Distinct new pictures drawn per second for the tile at `index` over
    /// the last two seconds (what the viewer actually sees); nil until two
    /// were drawn, or when none was for two seconds.
    public func shownRate(ofTile index: Int) -> Double? {
        guard shown.indices.contains(index) else { return nil }
        return shown[index].rate(now: hostMicros())
    }

    /// Tiles drawn with the dewarp pipeline in the last frame (self-test).
    public private(set) var dewarpedTilesDrawn = 0

    /// The size of the picture a tile is drawing now, or nil when it draws black.
    public func pictureSize(ofTile index: Int) -> CGSize? {
        guard textures.indices.contains(index), let t = textures[index] else { return nil }
        return CGSize(width: t.width, height: t.height)
    }

    /// Draws one frame into a drawable and presents it. With `capture`, the
    /// same tiles are also drawn into an offscreen texture of the same size
    /// in the same command buffer, which is then read back (this waits for
    /// the GPU).
    @discardableResult
    public func draw(to drawable: any CAMetalDrawable, capture: Bool = false) -> RenderedImage? {
        let target = drawable.texture
        guard let commandBuffer = beginFrame() else { return nil }
        encodePass(commandBuffer, target: target)
        var image: (any MTLTexture)?
        if capture {
            image = offscreenTexture(width: target.width, height: target.height)
            encodePass(commandBuffer, target: image!)
        }
        commandBuffer.present(drawable)
        commandBuffer.commit()
        finishFrame(width: target.width, height: target.height)
        guard let image else { return nil }
        commandBuffer.waitUntilCompleted()
        return read(image)
    }

    /// Draws one frame into an offscreen texture and reads it back. Waits for
    /// the GPU; for tests and self-checks.
    public func renderOffscreen(width: Int, height: Int) -> RenderedImage? {
        guard width > 0, height > 0, let commandBuffer = beginFrame() else { return nil }
        let target = offscreenTexture(width: width, height: height)
        encodePass(commandBuffer, target: target)
        commandBuffer.commit()
        finishFrame(width: width, height: height)
        commandBuffer.waitUntilCompleted()
        return read(target)
    }

    /// Drops every texture this renderer holds (call after the tiles' decoders
    /// go away, or to release memory while hidden).
    public func releaseTextures() {
        for i in inFlight.indices {
            inFlight[i].commandBuffer?.waitUntilCompleted()
            inFlight[i].commandBuffer = nil
            inFlight[i].textures.removeAll(keepingCapacity: true)
        }
        textures = textures.map { _ in nil }
        seen = seen.map { _ in UInt64.max }
        CVMetalTextureCacheFlush(textureCache, 0)
    }

    // MARK: Frame

    /// Waits for the oldest frame slot, refreshes tile textures, and starts a
    /// command buffer.
    private func beginFrame() -> (any MTLCommandBuffer)? {
        let slot = frameIndex % Self.maxFramesInFlight
        // Command buffers on one queue complete in order: once this slot's
        // frame is done, its textures are free.
        if let previous = inFlight[slot].commandBuffer, previous.status.rawValue < MTLCommandBufferStatus.completed.rawValue {
            previous.waitUntilCompleted()
        }
        inFlight[slot].commandBuffer = nil
        inFlight[slot].textures.removeAll(keepingCapacity: true)

        updateTextures()
        for t in textures {
            if let t {
                inFlight[slot].textures.append(t.luma)
                inFlight[slot].textures.append(t.chroma)
            }
        }
        guard let commandBuffer = queue.makeCommandBuffer() else { return nil }
        inFlight[slot].commandBuffer = commandBuffer
        return commandBuffer
    }

    private func finishFrame(width: Int, height: Int) {
        frameIndex &+= 1
        layoutChanged = false
        lastTargetSize = (width, height)
        CVMetalTextureCacheFlush(textureCache, 0)
    }

    private func updateTextures() {
        if seen.count != tiles.count {
            seen = Array(repeating: UInt64.max, count: tiles.count)
            textures = Array(repeating: nil, count: tiles.count)
            shown = Array(repeating: RateMeter(), count: tiles.count)
            shownSources = Array(repeating: nil, count: tiles.count)
        }
        let now = hostMicros()
        for (i, tile) in tiles.enumerated() {
            let source = ObjectIdentifier(tile.picture)
            if shownSources[i] != source {
                shown[i].reset()
                shownSources[i] = source
            }
            let sequence = tile.picture.pictureSequence
            guard sequence != seen[i] else { continue }
            let picture = tile.picture.latestPicture()
            seen[i] = picture?.sequence ?? sequence
            textures[i] = picture.flatMap { makeTextures($0.pixelBuffer) }
            if textures[i] != nil { shown[i].add(now) }
        }
    }

    private func makeTextures(_ buffer: CVPixelBuffer) -> TileTextures? {
        let format = CVPixelBufferGetPixelFormatType(buffer)
        guard format == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            || format == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            CVPixelBufferGetPlaneCount(buffer) == 2,
            let luma = planeTexture(buffer, plane: 0, format: .r8Unorm),
            let chroma = planeTexture(buffer, plane: 1, format: .rg8Unorm)
        else { return nil }
        return TileTextures(
            luma: luma.0, chroma: chroma.0, lumaTexture: luma.1, chromaTexture: chroma.1,
            width: CVPixelBufferGetWidth(buffer), height: CVPixelBufferGetHeight(buffer))
    }

    private func planeTexture(
        _ buffer: CVPixelBuffer, plane: Int, format: MTLPixelFormat
    ) -> (CVMetalTexture, any MTLTexture)? {
        var texture: CVMetalTexture?
        let status = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault, textureCache, buffer, nil, format,
            CVPixelBufferGetWidthOfPlane(buffer, plane), CVPixelBufferGetHeightOfPlane(buffer, plane),
            plane, &texture)
        guard status == kCVReturnSuccess, let texture, let metal = CVMetalTextureGetTexture(texture) else {
            return nil
        }
        return (texture, metal)
    }

    // MARK: Pass

    private func encodePass(_ commandBuffer: any MTLCommandBuffer, target: any MTLTexture) {
        passDescriptor.colorAttachments[0].texture = target
        defer { passDescriptor.colorAttachments[0].texture = nil }
        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: passDescriptor) else { return }
        var current: (any MTLRenderPipelineState)?
        var dewarped = 0
        let width = Float(target.width)
        let height = Float(target.height)
        for (i, tile) in tiles.enumerated() {
            guard i < textures.count, let t = textures[i], !tile.destination.isEmpty else { continue }
            let d = tile.destination
            let s = tile.source ?? CGRect(x: 0, y: 0, width: t.width, height: t.height)
            let pw = Float(t.width)
            let ph = Float(t.height)
            let wanted = tile.dewarp == nil ? pipeline : dewarpPipeline
            if current !== wanted {
                encoder.setRenderPipelineState(wanted)
                current = wanted
            }
            var uniforms = TileUniforms(
                destination: SIMD4(
                    Float(d.minX) / width * 2 - 1, 1 - Float(d.minY) / height * 2,
                    Float(d.maxX) / width * 2 - 1, 1 - Float(d.maxY) / height * 2),
                // A dewarped tile's shader works in tile coordinates.
                source: tile.dewarp == nil
                    ? SIMD4(Float(s.minX) / pw, Float(s.minY) / ph, Float(s.maxX) / pw, Float(s.maxY) / ph)
                    : SIMD4(0, 0, 1, 1),
                r: tile.color.r, g: tile.color.g, b: tile.color.b)
            let size = MemoryLayout<TileUniforms>.stride
            encoder.setVertexBytes(&uniforms, length: size, index: 0)
            encoder.setFragmentBytes(&uniforms, length: size, index: 0)
            if var constants = tile.dewarp {
                encoder.setFragmentBytes(&constants, length: MemoryLayout<DewarpConstants>.stride, index: 1)
                dewarped += 1
            }
            encoder.setFragmentTexture(t.lumaTexture, index: 0)
            encoder.setFragmentTexture(t.chromaTexture, index: 1)
            encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        }
        encoder.endEncoding()
        dewarpedTilesDrawn = dewarped
    }

    // MARK: Offscreen

    private func offscreenTexture(width: Int, height: Int) -> any MTLTexture {
        if let offscreen, offscreen.width == width, offscreen.height == height { return offscreen }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: Self.pixelFormat, width: width, height: height, mipmapped: false)
        descriptor.usage = [.renderTarget]
        descriptor.storageMode = .shared
        let texture = device.makeTexture(descriptor: descriptor)!
        texture.label = "grid offscreen"
        offscreen = texture
        return texture
    }

    private func read(_ texture: any MTLTexture) -> RenderedImage {
        let count = texture.width * texture.height * 4
        if readback.count != count { readback = Array(repeating: 0, count: count) }
        readback.withUnsafeMutableBytes { bytes in
            texture.getBytes(
                bytes.baseAddress!, bytesPerRow: texture.width * 4,
                from: MTLRegionMake2D(0, 0, texture.width, texture.height), mipmapLevel: 0)
        }
        return RenderedImage(width: texture.width, height: texture.height, bgra: readback)
    }
}
