import CoreML
import CoreVideo
import Foundation

public enum DetectorModelError: Error, Equatable {
    case notFound
    case unexpectedInterface(String)
}

/// Facts about the loaded model.
public struct DetectorModelInfo: Sendable, Equatable {
    public var labels: [String]
    public var source: String
}

/// Where the Core ML package or compiled model is looked for, in order:
/// the `NVRVIEW_DETECTOR_MODEL` environment variable (a path), the app
/// bundle's `Models` resources, the per-user `Models` folder in Application
/// Support. Either a compiled `.mlmodelc` or an `.mlpackage` is accepted.
public enum DetectorModelLocator {
    public static let modelName = "rtdetr_r18vd"
    public static let environmentVariable = "NVRVIEW_DETECTOR_MODEL"

    public static func locate(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        bundle: Bundle = .main, fileManager: FileManager = .default
    ) -> URL? {
        if let path = environment[environmentVariable], !path.isEmpty {
            return fileManager.fileExists(atPath: path) ? URL(fileURLWithPath: path) : nil
        }
        var candidates: [URL] = []
        for ext in ["mlmodelc", "mlpackage"] {
            if let url = bundle.url(forResource: modelName, withExtension: ext, subdirectory: "Models") {
                candidates.append(url)
            }
        }
        if let support = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first {
            for ext in ["mlmodelc", "mlpackage"] {
                candidates.append(support.appendingPathComponent("NVRView/Models/\(modelName).\(ext)"))
            }
        }
        return candidates.first { fileManager.fileExists(atPath: $0.path) }
    }
}

/// RT-DETR on Core ML. Compute units default to the CPU and Neural Engine
/// (docs/VISION.md: the GPU belongs to the renderer).
public final class CoreMLInference: ModelInference, @unchecked Sendable {
    public let inputSize: Int
    public let info: DetectorModelInfo
    public let computeUnits: MLComputeUnits
    private let model: MLModel
    private let inputName: String

    /// Loads a `.mlmodelc`, or compiles a `.mlpackage` first (the compiled
    /// copy is cached under Caches, keyed by name and modification time).
    public static func load(url: URL, computeUnits: MLComputeUnits = .cpuAndNeuralEngine) async throws -> CoreMLInference {
        let compiled = url.pathExtension == "mlmodelc" ? url : try await compiledCopy(of: url)
        let configuration = MLModelConfiguration()
        configuration.computeUnits = computeUnits
        let model = try await MLModel.load(contentsOf: compiled, configuration: configuration)
        return try CoreMLInference(model: model, computeUnits: computeUnits)
    }

    private static func compiledCopy(of url: URL) async throws -> URL {
        let fm = FileManager.default
        let modified = (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)?
            .timeIntervalSince1970 ?? 0
        let caches = fm.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("NVRView/CompiledModels")
        let cached = caches.appendingPathComponent(
            "\(url.deletingPathExtension().lastPathComponent)-\(Int(modified)).mlmodelc")
        if fm.fileExists(atPath: cached.path) { return cached }
        let temporary = try await MLModel.compileModel(at: url)
        try fm.createDirectory(at: caches, withIntermediateDirectories: true)
        try? fm.removeItem(at: cached)
        try fm.moveItem(at: temporary, to: cached)
        return cached
    }

    init(model: MLModel, computeUnits: MLComputeUnits) throws {
        let description = model.modelDescription
        guard let name = description.inputDescriptionsByName.first(where: { $0.value.type == .image })?.key,
            let constraint = description.inputDescriptionsByName[name]?.imageConstraint,
            constraint.pixelsWide == constraint.pixelsHigh
        else { throw DetectorModelError.unexpectedInterface("expected one square image input") }
        for output in ["logits", "pred_boxes"] where description.outputDescriptionsByName[output] == nil {
            throw DetectorModelError.unexpectedInterface("missing output \(output)")
        }
        self.model = model
        self.inputName = name
        self.inputSize = constraint.pixelsWide
        self.computeUnits = computeUnits
        let metadata = description.metadata[.creatorDefinedKey] as? [String: String] ?? [:]
        self.info = DetectorModelInfo(
            labels: (metadata["labels"] ?? "").split(separator: ",").map(String.init),
            source: metadata["source"] ?? "")
    }

    public func infer(_ inputs: [CVPixelBuffer]) throws -> [ModelOutput] {
        let providers = try inputs.map { buffer in
            try MLDictionaryFeatureProvider(dictionary: [inputName: MLFeatureValue(pixelBuffer: buffer)])
        }
        // One batch call: Core ML runs the crops back to back without
        // returning to us in between.
        let results = try model.predictions(fromBatch: MLArrayBatchProvider(array: providers))
        return try (0..<results.count).map { i in
            let features = results.features(at: i)
            guard let logits = features.featureValue(for: "logits")?.multiArrayValue,
                let boxes = features.featureValue(for: "pred_boxes")?.multiArrayValue
            else { throw DetectorModelError.unexpectedInterface("outputs missing") }
            let queries = logits.shape[logits.shape.count - 2].intValue
            let classes = logits.shape[logits.shape.count - 1].intValue
            return ModelOutput(
                queries: queries, classes: classes,
                logits: RTDETRDecoder.floats(logits, rows: queries, columns: classes),
                boxes: RTDETRDecoder.floats(boxes, rows: queries, columns: 4))
        }
    }
}
