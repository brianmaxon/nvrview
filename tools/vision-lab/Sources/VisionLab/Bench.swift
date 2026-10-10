import CoreImage
import CoreVideo
import Foundation
import Vision

/// Which decoded pictures the detectors see.
public enum Sampling: Sendable {
    /// Every decoded picture.
    case all
    /// The first picture of each 1/fps interval of stream time, as a frame
    /// tap sampling at that rate would deliver them.
    case rate(Double)
    /// Exactly these picture indices (for scoring against labels).
    case frames(Set<Int>)

    func selects(index: Int, sourceFPS: Double) -> Bool {
        switch self {
        case .all: return true
        case .frames(let set): return set.contains(index)
        case .rate(let fps):
            let t = Double(index) / sourceFPS
            let prev = Double(index - 1) / sourceFPS
            return index == 0 || floor(t * fps) > floor(prev * fps)
        }
    }
}

public struct DetectorResult: Codable, Sendable {
    public var ms: Double
    public var detections: [Detection]
}

/// One sampled picture's results (one JSON line in the results file).
public struct FrameResult: Codable, Sendable {
    public var frame: Int
    public var time: Double
    public var results: [String: DetectorResult]
}

public struct BenchConfig {
    public var input: URL
    public var codec: Codec
    /// Pictures per second of the source; Annex-B files carry no timestamps.
    public var sourceFPS: Double
    public var sampling: Sampling
    public var detectors: [Detector]
    /// Decode at the source rate (sleeping between pictures) instead of as
    /// fast as possible, to measure steady-state CPU use like a live tile.
    public var realtime: Bool = false
    /// Stop after this much stream time.
    public var maxSeconds: Double?
    /// Write each sampled picture as a JPEG here (local labeling only).
    public var exportDir: URL?
    /// Write one JSON line per sampled picture here.
    public var resultsURL: URL?
    public var requireHardware: Bool = true

    public init(input: URL, codec: Codec, sourceFPS: Double, sampling: Sampling, detectors: [Detector]) {
        self.input = input
        self.codec = codec
        self.sourceFPS = sourceFPS
        self.sampling = sampling
        self.detectors = detectors
    }
}

public struct BenchReport: Codable, Sendable {
    public var input: String
    public var codec: String
    public var hardwareDecode: Bool
    public var width: Int
    public var height: Int
    public var pixelFormat: String
    public var ioSurfaceBacked: Bool
    public var framesDecoded: Int
    public var framesSampled: Int
    public var streamSeconds: Double
    public var wallSeconds: Double
    public var realtime: Bool
    /// Decode time per picture (submission to output, synchronous).
    public var decodeMs: Summary
    /// Per detector: time per sampled picture.
    public var detectorMs: [String: Summary]
    /// All detectors together per sampled picture.
    public var detectMsTotal: Summary
    public var processCPUSeconds: Double
    /// Process CPU time / wall time, in percent of one core.
    public var processCPUPercent: Double
    /// CPU seconds used meanwhile by helper processes (hardware decoder
    /// service, Neural Engine daemon); system-wide, so other users of those
    /// services count too.
    public var helperCPUSeconds: [String: Double]
    public var footprintMB: Double
    public var peakFootprintMB: Double
    public var gpuUtilizationMeanPercent: Double
    public var gpuUtilizationMaxPercent: Int
    public var detectionsPerDetector: [String: Int]
}

/// Decodes the whole input, runs the detectors on the sampled pictures and
/// measures the cost.
public func runBench(_ config: BenchConfig, progress: ((Int) -> Void)? = nil) throws -> BenchReport {
    let data = try Data(contentsOf: config.input)
    let units = accessUnits(splitAnnexB(data), codec: config.codec)
    let decoder = Decoder(codec: config.codec, requireHardware: config.requireHardware)

    var resultsHandle: FileHandle?
    if let url = config.resultsURL {
        FileManager.default.createFile(atPath: url.path, contents: nil)
        resultsHandle = try FileHandle(forWritingTo: url)
    }
    defer { try? resultsHandle?.close() }
    if let dir = config.exportDir {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }
    let ciContext = CIContext()
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]

    let helperNames = ["VTDecoderXPCService", "aned", "ANECompilerService"]
    let helpersBefore = otherProcessCPUSeconds(matching: helperNames)
    let gpu = GPUSampler()
    let cpu0 = processCPUSeconds()
    let wall0 = now()

    var decodeMs: [Double] = []
    var perDetector: [String: [Double]] = [:]
    var totals: [Double] = []
    var counts: [String: Int] = [:]
    var decoded = 0
    var sampled = 0
    var width = 0
    var height = 0
    var pixelFormat = ""
    var ioSurface = false

    for unit in units {
        let t = Double(unit.index) / config.sourceFPS
        if let maxSeconds = config.maxSeconds, t >= maxSeconds { break }
        if config.realtime {
            let due = wall0 + t
            let wait = due - now()
            if wait > 0 { Thread.sleep(forTimeInterval: wait) }
        }
        let d0 = now()
        guard let pb = try decoder.decode(unit) else { continue }
        decodeMs.append((now() - d0) * 1000)
        decoded += 1
        if width == 0 {
            width = CVPixelBufferGetWidth(pb)
            height = CVPixelBufferGetHeight(pb)
            pixelFormat = fourCC(CVPixelBufferGetPixelFormatType(pb))
            ioSurface = CVPixelBufferGetIOSurface(pb) != nil
        }
        progress?(unit.index)
        guard config.sampling.selects(index: unit.index, sourceFPS: config.sourceFPS) else { continue }
        sampled += 1

        // One handler per picture, shared by all detectors: Vision reads the
        // decoder's IOSurface-backed buffer directly.
        let handler = VNImageRequestHandler(cvPixelBuffer: pb, orientation: .up, options: [:])
        var frame = FrameResult(frame: unit.index, time: t, results: [:])
        var total = 0.0
        for detector in config.detectors {
            let s = now()
            let detections = try detector.detect(handler)
            let ms = (now() - s) * 1000
            total += ms
            perDetector[detector.name, default: []].append(ms)
            counts[detector.name, default: 0] += detections.count
            frame.results[detector.name] = DetectorResult(ms: ms, detections: detections)
        }
        totals.append(total)
        if let handle = resultsHandle {
            var line = try encoder.encode(frame)
            line.append(0x0A)
            try handle.write(contentsOf: line)
        }
        if let dir = config.exportDir {
            let url = dir.appendingPathComponent(String(format: "f%05d.jpg", unit.index))
            let image = CIImage(cvPixelBuffer: pb)
            try ciContext.writeJPEGRepresentation(
                of: image, to: url, colorSpace: CGColorSpace(name: CGColorSpace.sRGB)!, options: [:])
        }
    }

    let wall = now() - wall0
    let cpu = processCPUSeconds() - cpu0
    let g = gpu.stop()
    let helpersAfter = otherProcessCPUSeconds(matching: helperNames)
    var helpers: [String: Double] = [:]
    for (name, after) in helpersAfter {
        helpers[name] = max(0, after - (helpersBefore[name] ?? 0))
    }
    let mem = memoryFootprint()
    let last = units.last.map { Double($0.index + 1) / config.sourceFPS } ?? 0

    return BenchReport(
        input: config.input.lastPathComponent, codec: config.codec.rawValue,
        hardwareDecode: decoder.usingHardware, width: width, height: height, pixelFormat: pixelFormat,
        ioSurfaceBacked: ioSurface, framesDecoded: decoded, framesSampled: sampled,
        streamSeconds: min(last, config.maxSeconds ?? last), wallSeconds: wall, realtime: config.realtime,
        decodeMs: Summary(decodeMs), detectorMs: perDetector.mapValues { Summary($0) },
        detectMsTotal: Summary(totals), processCPUSeconds: cpu,
        processCPUPercent: wall > 0 ? cpu / wall * 100 : 0, helperCPUSeconds: helpers,
        footprintMB: Double(mem.current) / 1_048_576, peakFootprintMB: Double(mem.peak) / 1_048_576,
        gpuUtilizationMeanPercent: g.mean, gpuUtilizationMaxPercent: g.max, detectionsPerDetector: counts)
}

func fourCC(_ code: OSType) -> String {
    let bytes = [24, 16, 8, 0].map { UInt8((code >> UInt32($0)) & 0xFF) }
    return String(decoding: bytes, as: UTF8.self)
}
