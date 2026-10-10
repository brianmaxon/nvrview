import CoreML
import CoreVideo
import Foundation
import Vision

/// The classes the comparison with device detections cares about: the
/// device AI types `people`, `vehicle` and `dog_cat` (docs/protocol.md).
public enum ObjectClass: String, CaseIterable, Codable, Sendable {
    case person
    case vehicle
    case animal
}

/// One result: a label from the model, the class it maps to (if any), a
/// confidence, and a normalized box (x, y, width, height; origin lower left,
/// as Vision reports it).
public struct Detection: Codable, Sendable {
    public var label: String
    public var cls: ObjectClass?
    public var confidence: Float
    public var box: [Float]

    public init(label: String, cls: ObjectClass?, confidence: Float, box: [Float]) {
        self.label = label
        self.cls = cls
        self.confidence = confidence
        self.box = box
    }
}

/// Where Vision runs its built-in requests' main stage.
public enum VisionDevice: String, Sendable {
    case auto, cpu, gpu, ane
}

/// Core ML compute units, as the command line names them.
public enum ComputeUnits: String, Sendable {
    case all, cpu, cpuGPU = "cpu-gpu", cpuANE = "cpu-ane"

    public var ml: MLComputeUnits {
        switch self {
        case .all: return .all
        case .cpu: return .cpuOnly
        case .cpuGPU: return .cpuAndGPU
        case .cpuANE: return .cpuAndNeuralEngine
        }
    }
}

/// A detector runs on one decoded picture at a time.
public protocol Detector: AnyObject {
    var name: String { get }
    func detect(_ handler: VNImageRequestHandler) throws -> [Detection]
}

/// Maps COCO-style and Vision labels to the three classes.
public func classify(label: String) -> ObjectClass? {
    switch label.lowercased() {
    case "person", "people", "human", "adult", "child":
        return .person
    case "car", "truck", "bus", "motorbike", "motorcycle", "automobile", "vehicle", "van", "suv",
        "pickup_truck", "pickup truck", "jeep":
        return .vehicle
    case "dog", "cat", "bird", "horse", "sheep", "cow", "bear", "animal", "canine", "feline", "pet",
        "mammal", "raccoon", "deer", "fox", "squirrel", "rabbit":
        return .animal
    default:
        return nil
    }
}

private func applyDevice(_ device: VisionDevice, to request: VNRequest) throws {
    guard device != .auto else { return }
    let supported = try request.supportedComputeStageDevices
    guard let devices = supported[.main] else { return }
    let wanted = devices.first { d in
        switch (device, d) {
        case (.cpu, .cpu): return true
        case (.gpu, .gpu): return true
        case (.ane, .neuralEngine): return true
        default: return false
        }
    }
    if let wanted {
        request.setComputeDevice(wanted, for: .main)
    } else {
        throw LabError.message("\(type(of: request)) cannot run its main stage on \(device.rawValue)")
    }
}

private func box(_ r: CGRect) -> [Float] {
    [Float(r.origin.x), Float(r.origin.y), Float(r.width), Float(r.height)]
}

public enum LabError: Error, CustomStringConvertible {
    case message(String)
    public var description: String {
        switch self {
        case .message(let m): return m
        }
    }
}

/// VNDetectHumanRectanglesRequest: whole bodies, or upper bodies only.
public final class HumanDetector: Detector {
    public let name: String
    private let request: VNDetectHumanRectanglesRequest

    public init(upperBodyOnly: Bool, device: VisionDevice) throws {
        name = upperBodyOnly ? "human-upper" : "human"
        request = VNDetectHumanRectanglesRequest()
        request.upperBodyOnly = upperBodyOnly
        try applyDevice(device, to: request)
    }

    public func detect(_ handler: VNImageRequestHandler) throws -> [Detection] {
        try handler.perform([request])
        return (request.results ?? []).map {
            Detection(label: "person", cls: .person, confidence: $0.confidence, box: box($0.boundingBox))
        }
    }
}

/// VNRecognizeAnimalsRequest: cats and dogs.
public final class AnimalDetector: Detector {
    public let name = "animal"
    private let request = VNRecognizeAnimalsRequest()

    public init(device: VisionDevice) throws {
        try applyDevice(device, to: request)
    }

    public func detect(_ handler: VNImageRequestHandler) throws -> [Detection] {
        try handler.perform([request])
        return (request.results ?? []).compactMap { obs in
            guard let top = obs.labels.first else { return nil }
            return Detection(
                label: top.identifier.lowercased(), cls: .animal, confidence: top.confidence,
                box: box(obs.boundingBox))
        }
    }
}

/// VNDetectFaceRectanglesRequest: faces (a cost reference; faces are not one
/// of the three classes).
public final class FaceDetector: Detector {
    public let name = "face"
    private let request = VNDetectFaceRectanglesRequest()

    public init(device: VisionDevice) throws {
        try applyDevice(device, to: request)
    }

    public func detect(_ handler: VNImageRequestHandler) throws -> [Detection] {
        try handler.perform([request])
        return (request.results ?? []).map {
            Detection(label: "face", cls: nil, confidence: $0.confidence, box: box($0.boundingBox))
        }
    }
}

/// VNClassifyImageRequest: whole-picture labels from Apple's built-in
/// taxonomy. Gives presence, not boxes; one result per class (the best
/// matching label), box = whole picture.
public final class SceneClassifier: Detector {
    public let name = "classify"
    private let request = VNClassifyImageRequest()

    public init(device: VisionDevice) throws {
        try applyDevice(device, to: request)
    }

    /// The taxonomy labels mapped to each class.
    public static func labels(for cls: ObjectClass) -> Set<String> {
        switch cls {
        case .person: return ["people", "adult", "child", "baby", "crowd"]
        case .vehicle:
            return ["car", "automobile", "vehicle", "truck", "bus", "motorcycle", "van", "suv", "jeep",
                    "pickup_truck", "sportscar", "convertible", "atv"]
        case .animal:
            return ["animal", "dog", "cat", "canine", "feline", "mammal", "bird", "deer", "raccoon",
                    "squirrel", "fox", "rabbit", "horse", "pet"]
        }
    }

    public func supportedLabels() throws -> [String] {
        try request.supportedIdentifiers()
    }

    public func detect(_ handler: VNImageRequestHandler) throws -> [Detection] {
        try handler.perform([request])
        var best: [ObjectClass: (String, Float)] = [:]
        for obs in request.results ?? [] {
            for cls in ObjectClass.allCases where Self.labels(for: cls).contains(obs.identifier) {
                if obs.confidence > (best[cls]?.1 ?? 0) { best[cls] = (obs.identifier, obs.confidence) }
            }
        }
        return best.map { Detection(label: $0.value.0, cls: $0.key, confidence: $0.value.1, box: [0, 0, 1, 1]) }
    }
}

/// A Core ML object detector through VNCoreMLRequest. Expects a model whose
/// output Vision turns into VNRecognizedObjectObservation (a pipeline that
/// ends in non-maximum suppression, as Apple's converted YOLOv3 models do).
public final class CoreMLDetector: Detector {
    public let name: String
    public let model: MLModel
    private let request: VNCoreMLRequest

    public init(modelURL: URL, units: ComputeUnits, name: String? = nil, scaleFit: Bool = false) throws {
        let compiled: URL
        if modelURL.pathExtension == "mlmodelc" {
            compiled = modelURL
        } else {
            compiled = try compileOnce(modelURL)
        }
        let config = MLModelConfiguration()
        config.computeUnits = units.ml
        model = try MLModel(contentsOf: compiled, configuration: config)
        let vnModel = try VNCoreMLModel(for: model)
        request = VNCoreMLRequest(model: vnModel)
        request.imageCropAndScaleOption = scaleFit ? .scaleFit : .scaleFill
        self.name = name ?? modelURL.deletingPathExtension().lastPathComponent
    }

    public func detect(_ handler: VNImageRequestHandler) throws -> [Detection] {
        try handler.perform([request])
        let results = request.results ?? []
        let features = results.compactMap { $0 as? VNCoreMLFeatureValueObservation }
        if !features.isEmpty {
            return try decodeDETR(features)
        }
        return results.compactMap { r in
            guard let obs = r as? VNRecognizedObjectObservation, let top = obs.labels.first else { return nil }
            return Detection(
                label: top.identifier, cls: classify(label: top.identifier), confidence: top.confidence,
                box: box(obs.boundingBox))
        }
    }

    /// Lowest score kept from raw DETR outputs; scoring applies its own
    /// thresholds above this.
    public static let detrFloor: Float = 0.2

    /// Class names from the model's metadata (`labels`, comma-separated), as
    /// scripts/convert-rtdetr.py writes them.
    private lazy var detrLabels: [String] = {
        let meta = model.modelDescription.metadata[.creatorDefinedKey] as? [String: String]
        return meta?["labels"]?.split(separator: ",").map(String.init) ?? []
    }()

    /// Decodes DETR-style raw outputs: `logits` (1, queries, classes; sigmoid
    /// scores) and `pred_boxes` (1, queries, 4; center x, center y, width,
    /// height, normalized, origin top left). Assumes the picture was
    /// stretched into the input (the default crop-and-scale option).
    private func decodeDETR(_ features: [VNCoreMLFeatureValueObservation]) throws -> [Detection] {
        guard let logits = features.first(where: { $0.featureName == "logits" })?.featureValue.multiArrayValue,
            let boxes = features.first(where: { $0.featureName == "pred_boxes" })?.featureValue.multiArrayValue,
            logits.shape.count == 3, boxes.shape.count == 3
        else { throw LabError.message("\(name): expected 'logits' and 'pred_boxes' outputs") }
        let queries = logits.shape[1].intValue
        let classes = logits.shape[2].intValue
        let l = floats(logits)
        let b = floats(boxes)
        var out: [Detection] = []
        for q in 0..<queries {
            var best = -Float.infinity
            var bestClass = 0
            for c in 0..<classes where l[q * classes + c] > best {
                best = l[q * classes + c]
                bestClass = c
            }
            let score = 1 / (1 + exp(-best))
            guard score >= Self.detrFloor else { continue }
            let cx = b[q * 4], cy = b[q * 4 + 1], w = b[q * 4 + 2], h = b[q * 4 + 3]
            let label = bestClass < detrLabels.count ? detrLabels[bestClass] : "class\(bestClass)"
            out.append(
                Detection(
                    label: label, cls: classify(label: label), confidence: score,
                    box: [cx - w / 2, 1 - (cy + h / 2), w, h]))
        }
        return out
    }
}

/// Copies a multi-array's values (Float16 or Float32, any strides) into a
/// row-major Float array.
func floats(_ a: MLMultiArray) -> [Float] {
    let shape = a.shape.map(\.intValue)
    let strides = a.strides.map(\.intValue)
    let count = shape.reduce(1, *)
    var out = [Float](repeating: 0, count: count)
    func offset(_ flat: Int) -> Int {
        var rest = flat
        var off = 0
        for d in stride(from: shape.count - 1, through: 0, by: -1) {
            off += (rest % shape[d]) * strides[d]
            rest /= shape[d]
        }
        return off
    }
    switch a.dataType {
    case .float16:
        a.withUnsafeBufferPointer(ofType: Float16.self) { p in
            for i in 0..<count { out[i] = Float(p[offset(i)]) }
        }
    case .float32:
        a.withUnsafeBufferPointer(ofType: Float.self) { p in
            for i in 0..<count { out[i] = p[offset(i)] }
        }
    default:
        for i in 0..<count { out[i] = a[i].floatValue }
    }
    return out
}

/// Compiles a .mlmodel/.mlpackage next to itself once (as `<name>.mlmodelc`)
/// and reuses the result.
func compileOnce(_ url: URL) throws -> URL {
    let target = url.deletingPathExtension().appendingPathExtension("mlmodelc")
    let fm = FileManager.default
    if fm.fileExists(atPath: target.path) { return target }
    let temp = try MLModel.compileModel(at: url)
    try fm.moveItem(at: temp, to: target)
    return target
}

/// Where Core ML plans to run each operation of a model (an estimate the
/// framework gives without running it; no privileges needed).
public func computePlanSummary(modelURL: URL, units: ComputeUnits) async throws -> [String: Int] {
    let compiled = modelURL.pathExtension == "mlmodelc" ? modelURL : try compileOnce(modelURL)
    let config = MLModelConfiguration()
    config.computeUnits = units.ml
    let plan = try await MLComputePlan.load(contentsOf: compiled, configuration: config)
    var counts: [String: Int] = [:]
    switch plan.modelStructure {
    case .neuralNetwork(let nn):
        for layer in nn.layers {
            counts[deviceName(plan.deviceUsage(for: layer)?.preferred), default: 0] += 1
        }
    case .program(let program):
        for (_, function) in program.functions {
            for op in function.block.operations {
                counts[deviceName(plan.deviceUsage(for: op)?.preferred), default: 0] += 1
            }
        }
    case .pipeline(let pipeline):
        for sub in pipeline.subModels {
            if case .neuralNetwork(let nn) = sub {
                for layer in nn.layers {
                    counts[deviceName(plan.deviceUsage(for: layer)?.preferred), default: 0] += 1
                }
            } else if case .program(let program) = sub {
                for (_, function) in program.functions {
                    for op in function.block.operations {
                        counts[deviceName(plan.deviceUsage(for: op)?.preferred), default: 0] += 1
                    }
                }
            } else {
                counts["other-model", default: 0] += 1
            }
        }
    default:
        counts["unsupported", default: 0] += 1
    }
    return counts
}

private func deviceName(_ d: MLComputeDevice?) -> String {
    switch d {
    case .cpu: return "cpu"
    case .gpu: return "gpu"
    case .neuralEngine: return "ane"
    case nil: return "none"
    @unknown default: return "unknown"
    }
}
