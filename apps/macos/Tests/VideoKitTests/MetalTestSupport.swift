import CoreGraphics
import CoreVideo
import Darwin
import Foundation
import PipelineStandIn
import Synchronization
import VideoKit

/// Collects decoder events from any thread.
final class EventLog: Sendable {
    private let events = Mutex<[TileDecoderEvent]>([])
    func append(_ event: TileDecoderEvent) { events.withLock { $0.append(event) } }
    var all: [TileDecoderEvent] { events.withLock { $0 } }
    var decodedPTS: [Int64] {
        all.compactMap { if case .decoded(let pts) = $0 { pts } else { nil } }
    }
    var failures: [DecodeFailure] {
        all.compactMap { if case .failed(let kind) = $0 { kind } else { nil } }
    }
}

/// A fixture played through the stand-in pipeline into a real `TileDecoder`,
/// one access unit at a time, waiting for each to decode so results are
/// deterministic.
struct FixturePlayback {
    let clip: Fixtures.Clip
    let log = EventLog()
    let tile: StandInTile
    private(set) var next = 0

    init(_ name: String, color: StandInColor? = nil) throws {
        clip = try Fixtures.load(name)
        let log = self.log
        tile = StandInTile(codec: clip.codec, colorOverride: color) { log.append($0) }
    }

    var frameCount: Int { clip.units.count }

    /// Pushes access units up to and including `index`.
    mutating func play(through index: Int) {
        while next <= index && next < clip.units.count {
            let unit = clip.units[next]
            tile.push(clip.data.subdata(in: unit.range), ptsMicros: Int64(next) * 50_000)
            tile.decoder.waitForPendingFrames()
            next += 1
        }
    }

    mutating func playAll() { play(through: clip.units.count - 1) }
}

/// A fixed NV12 picture, for renderer tests that need exact input.
final class StillPicture: TilePictureSource {
    let picture: DecodedPicture
    init(_ picture: DecodedPicture) { self.picture = picture }
    func latestPicture() -> DecodedPicture? { picture }
    var pictureSequence: UInt64 { picture.sequence }

    /// A video-range NV12 picture of one flat color.
    static func flat(width: Int, height: Int, y: UInt8, cb: UInt8, cr: UInt8) -> StillPicture {
        StillPicture(DecodedPicture.nv12(width: width, height: height) { _, _ in (y, cb, cr) })
    }
}

extension DecodedPicture {
    /// An IOSurface-backed, Metal-compatible, video-range NV12 buffer filled
    /// per pixel by `value(x, y)` (chroma taken at even coordinates).
    static func nv12(
        width: Int, height: Int, value: (Int, Int) -> (y: UInt8, cb: UInt8, cr: UInt8)
    ) -> DecodedPicture {
        let attributes: [CFString: Any] = [
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
        ]
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(
            kCFAllocatorDefault, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            attributes as CFDictionary, &buffer)
        let pb = buffer!
        CVPixelBufferLockBaseAddress(pb, [])
        let luma = CVPixelBufferGetBaseAddressOfPlane(pb, 0)!.assumingMemoryBound(to: UInt8.self)
        let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
        let chroma = CVPixelBufferGetBaseAddressOfPlane(pb, 1)!.assumingMemoryBound(to: UInt8.self)
        let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
        for y in 0..<height {
            for x in 0..<width {
                let v = value(x, y)
                luma[y * lumaStride + x] = v.y
                if x % 2 == 0 && y % 2 == 0 {
                    chroma[(y / 2) * chromaStride + x] = v.cb
                    chroma[(y / 2) * chromaStride + x + 1] = v.cr
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, [])
        return DecodedPicture(pixelBuffer: pb, ptsMicros: 0, sequence: 1)
    }
}

extension RenderedImage {
    /// Mean RGB over a rectangle.
    func mean(_ r: CGRect) -> SIMD3<Double> {
        var sum = SIMD3<Double>(repeating: 0)
        var n = 0.0
        for y in Int(r.minY)..<Int(r.maxY) {
            for x in Int(r.minX)..<Int(r.maxX) {
                let p = rgb(x: x, y: y)
                sum += SIMD3(Double(p.x), Double(p.y), Double(p.z))
                n += 1
            }
        }
        return sum / max(n, 1)
    }

    /// Standard deviation of luma-like brightness (R+G+B)/3 over a rectangle.
    func spread(_ r: CGRect) -> Double {
        var values: [Double] = []
        for y in Int(r.minY)..<Int(r.maxY) {
            for x in Int(r.minX)..<Int(r.maxX) {
                let p = rgb(x: x, y: y)
                values.append(Double(p.x + p.y + p.z) / 3)
            }
        }
        let mean = values.reduce(0, +) / Double(max(values.count, 1))
        return (values.map { ($0 - mean) * ($0 - mean) }.reduce(0, +) / Double(max(values.count, 1))).squareRoot()
    }

    var bounds: CGRect { CGRect(x: 0, y: 0, width: width, height: height) }
}

/// Physical memory footprint of this process, in bytes.
func physicalFootprint() -> Int {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
    let status = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    return status == KERN_SUCCESS ? Int(info.phys_footprint) : 0
}

/// Largest per-component absolute difference.
func maxDifference<V: SIMD>(_ a: V, _ b: V) -> V.Scalar where V.Scalar: FloatingPoint {
    maxMagnitude(a - b)
}

/// Largest per-component magnitude.
func maxMagnitude<V: SIMD>(_ v: V) -> V.Scalar where V.Scalar: FloatingPoint {
    v.indices.map { Swift.abs(v[$0]) }.max() ?? 0
}
