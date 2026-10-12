import CoreML
import CoreVideo
import Foundation

/// Runs the network on prepared model inputs. The Core ML implementation is
/// `CoreMLInference`; tests use a stub.
public protocol ModelInference: Sendable {
    /// The model's square input side in pixels.
    var inputSize: Int { get }
    /// One output per input, in order. Inputs are BGRA `inputSize` squares.
    func infer(_ inputs: [CVPixelBuffer]) throws -> [ModelOutput]
}

/// Synchronous detection: crops of one picture to raw detections. Blocks the
/// calling thread; `DetectorService` runs it off the main thread.
public protocol CropDetecting: Sendable {
    func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections]
}

/// Crops, preprocessing, inference and decoding, with the model behind a
/// protocol.
public final class CropDetector: CropDetecting, @unchecked Sendable {
    private let inference: any ModelInference
    private let preprocessor: CropPreprocessor
    public let fit: InputFit
    public let scoreFloor: Float

    public init(inference: any ModelInference, fit: InputFit = .stretch, scoreFloor: Float = 0.05) {
        self.inference = inference
        self.fit = fit
        self.scoreFloor = scoreFloor
        self.preprocessor = CropPreprocessor(size: inference.inputSize, fit: fit)
    }

    private let lock = NSLock()

    public func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
        // The preprocessor's pool is single-queue; serialize callers.
        lock.lock()
        defer { lock.unlock() }
        guard !crops.isEmpty else { return [] }
        let inputs = try crops.map { try preprocessor.input(for: $0, of: picture) }
        let outputs = try inference.infer(inputs)
        return zip(crops, outputs).map { crop, output in
            let r = CropGeometry.pixelRect(of: crop, pictureWidth: picture.width, pictureHeight: picture.height)
            let detections = RTDETRDecoder.decode(output, scoreFloor: scoreFloor).map { m in
                RawDetection(
                    classID: m.classID, score: m.score,
                    box: CropGeometry.toPicture(
                        m.box, crop: crop, cropPixelWidth: r.width, cropPixelHeight: r.height, fit: fit))
            }
            return CropDetections(crop: crop, detections: detections)
        }
    }
}
