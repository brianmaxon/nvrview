import CoreML
import CoreVideo
import Foundation

/// What the processor ranking (docs/NOTIFICATIONS.md, "Measured detector
/// speed") reads: the time to run the detector on one picture's crops.
public struct DetectorBenchmarkResult: Sendable, Equatable {
    public var computeUnits: String
    /// Crops per picture (the whole picture plus four tiles by default: 5).
    public var cropsPerFrame: Int
    public var frames: Int
    public var modelLoadMilliseconds: Double
    /// Wall time to detect all crops of one picture, in milliseconds.
    public var medianFrameMilliseconds: Double
    public var worstFrameMilliseconds: Double
    /// The process's peak resident size after the run, in megabytes.
    public var peakResidentMegabytes: Double
    /// The process's current physical footprint after the run, in megabytes.
    public var footprintMegabytes: Double
    /// How much the footprint grew from before the model was loaded to after
    /// the run, in megabytes (the detector's own cost inside a larger app).
    public var footprintGrowthMegabytes: Double
    /// The part of `medianFrameMilliseconds` spent cutting and scaling crops
    /// (measured separately, on the same pictures).
    public var medianPreprocessMilliseconds: Double

    public var summary: String {
        String(
            format: "%@: %d crops per frame, median %.1f ms (worst %.1f ms, preprocessing %.1f ms) over %d frames, load %.0f ms, peak resident %.0f MB, footprint %.0f MB (+%.0f MB)",
            computeUnits, cropsPerFrame, medianFrameMilliseconds, worstFrameMilliseconds, medianPreprocessMilliseconds,
            frames, modelLoadMilliseconds, peakResidentMegabytes, footprintMegabytes, footprintGrowthMegabytes)
    }
}

public enum DetectorBenchmark {
    /// A synthetic picture: a gradient background with a few flat-colored
    /// rectangles, deterministic from `seed`. Not meant to contain anything
    /// recognizable; the cost of a pass does not depend on the content.
    public static func syntheticPicture(width: Int = 1280, height: Int = 720, seed: Int = 0) -> DetectorPicture {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &buffer)
        guard let buffer else { preconditionFailure("cannot allocate a benchmark picture") }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let base = CVPixelBufferGetBaseAddress(buffer)!.assumingMemoryBound(to: UInt8.self)
        let row = CVPixelBufferGetBytesPerRow(buffer)
        for y in 0..<height {
            for x in 0..<width {
                let p = base + y * row + x * 4
                p[0] = UInt8((x * 255 / width + seed * 37) & 255)
                p[1] = UInt8((y * 255 / height + seed * 91) & 255)
                p[2] = UInt8(((x + y) * 255 / (width + height)) & 255)
                p[3] = 255
            }
        }
        for k in 0..<4 {
            let w = width / 8, h = height / 4
            let x0 = ((k * 3 + seed) % 6) * width / 8, y0 = ((k + seed) % 3) * height / 4
            for y in y0..<min(y0 + h, height) {
                for x in x0..<min(x0 + w, width) {
                    let p = base + y * row + x * 4
                    p[0] = UInt8(40 + k * 50); p[1] = UInt8(200 - k * 40); p[2] = UInt8(90 + k * 30)
                }
            }
        }
        return DetectorPicture(buffer)
    }

    /// Runs `detector` on `frames` synthetic pictures (after one warm-up) and
    /// reports the time per picture. `modelLoadMilliseconds` is passed through
    /// from whoever loaded the model.
    public static func run(
        detector: any CropDetecting, computeUnits: String, crops: [NormRect] = CropPlan.wholeFramePlusTiles,
        frames: Int = 8, modelLoadMilliseconds: Double = 0, baselineFootprintBytes: Double? = nil
    ) throws -> DetectorBenchmarkResult {
        let baseline = baselineFootprintBytes ?? footprintBytes()
        let pictures = (0..<frames).map { syntheticPicture(seed: $0) }
        _ = try detector.detect(picture: pictures[0], crops: crops)  // warm-up
        var times: [Double] = []
        let clock = ContinuousClock()
        for picture in pictures {
            let elapsed = try clock.measure { _ = try detector.detect(picture: picture, crops: crops) }
            times.append(Double(elapsed.components.seconds) * 1000 + Double(elapsed.components.attoseconds) / 1e15)
        }
        times.sort()
        // Preprocessing alone, same pictures and crops.
        let preprocessor = CropPreprocessor()
        var prep: [Double] = []
        for picture in pictures {
            let elapsed = try clock.measure { for crop in crops { _ = try preprocessor.input(for: crop, of: picture) } }
            prep.append(Double(elapsed.components.seconds) * 1000 + Double(elapsed.components.attoseconds) / 1e15)
        }
        prep.sort()
        return DetectorBenchmarkResult(
            computeUnits: computeUnits, cropsPerFrame: crops.count, frames: frames,
            modelLoadMilliseconds: modelLoadMilliseconds,
            medianFrameMilliseconds: times[times.count / 2], worstFrameMilliseconds: times.last ?? 0,
            peakResidentMegabytes: peakResidentBytes() / 1_048_576, footprintMegabytes: footprintBytes() / 1_048_576,
            footprintGrowthMegabytes: (footprintBytes() - baseline) / 1_048_576,
            medianPreprocessMilliseconds: prep[prep.count / 2])
    }

    /// Loads the model at `url` with `units` and benchmarks it.
    public static func run(
        modelURL: URL, units: MLComputeUnits, frames: Int = 8
    ) async throws -> DetectorBenchmarkResult {
        let clock = ContinuousClock()
        let baseline = footprintBytes()
        let start = clock.now
        let inference = try await CoreMLInference.load(url: modelURL, computeUnits: units)
        let load = clock.now - start
        let loadMs = Double(load.components.seconds) * 1000 + Double(load.components.attoseconds) / 1e15
        return try run(
            detector: CropDetector(inference: inference), computeUnits: name(of: units), frames: frames,
            modelLoadMilliseconds: loadMs, baselineFootprintBytes: baseline)
    }

    public static func name(of units: MLComputeUnits) -> String {
        switch units {
        case .cpuOnly: "cpu"
        case .cpuAndGPU: "cpu+gpu"
        case .cpuAndNeuralEngine: "cpu+neural-engine"
        case .all: "all"
        @unknown default: "unknown"
        }
    }

    static func peakResidentBytes() -> Double {
        var usage = rusage()
        getrusage(RUSAGE_SELF, &usage)
        return Double(usage.ru_maxrss)  // bytes on macOS
    }

    static func footprintBytes() -> Double {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let status = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        return status == KERN_SUCCESS ? Double(info.phys_footprint) : 0
    }
}
