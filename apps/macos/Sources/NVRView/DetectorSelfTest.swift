import CoreML
import DetectorKit
import Foundation

/// Self-test step `alerts-detector-benchmark` (docs/NOTIFICATIONS.md,
/// "Measured detector speed"): when the detector model is present (an
/// `NVRVIEW_DETECTOR_MODEL` path, the app bundle, or the per-user Models
/// folder), the real model runs the benchmark (a few synthetic frames, the
/// whole picture plus four tiles) on the CPU and Neural Engine and on the CPU
/// alone, and one detection goes through the off-main-thread queue. Each time
/// per frame must be positive and finite. Without the model nothing runs and
/// the step says so (the model is a separate download, not part of the build).
/// The work runs in a task; the self-test polls `finished`.
final class DetectorSelfTest: @unchecked Sendable {
    private let lock = NSLock()
    private var _problems: [String] = []
    private var _finished = false

    var finished: Bool { lock.withLock { _finished } }
    var problems: [String] { lock.withLock { _problems } }

    init() {
        Task.detached { [self] in
            await run()
            lock.withLock { _finished = true }
        }
    }

    private func fail(_ message: String) { lock.withLock { _problems.append("detector: \(message)") } }

    private func run() async {
        guard let url = DetectorModelLocator.locate() else {
            print("detector benchmark: no model present, skipped (set \(DetectorModelLocator.environmentVariable))")
            return
        }
        for units in [MLComputeUnits.cpuAndNeuralEngine, .cpuOnly] {
            do {
                let result = try await DetectorBenchmark.run(modelURL: url, units: units)
                print("detector benchmark: \(result.summary)")
                if !(result.medianFrameMilliseconds > 0 && result.medianFrameMilliseconds.isFinite) {
                    fail("\(result.computeUnits): implausible time per frame")
                }
                if result.cropsPerFrame != CropPlan.wholeFramePlusTiles.count { fail("wrong crop count") }
            } catch {
                fail("\(DetectorBenchmark.name(of: units)): \(error)")
            }
        }
        do {
            let inference = try await CoreMLInference.load(url: url)
            let service = DetectorService(detector: CropDetector(inference: inference))
            let results = try await service.detect(
                picture: DetectorBenchmark.syntheticPicture(), crops: CropPlan.wholeFramePlusTiles)
            if results.count != CropPlan.wholeFramePlusTiles.count { fail("the queue returned \(results.count) crops") }
            if inference.info.labels.first != "person" { fail("class names missing from the model's metadata") }
        } catch {
            fail("queue: \(error)")
        }
    }
}
