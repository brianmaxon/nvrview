import CoreML
import Foundation

/// The two raw tensors of one RT-DETR pass, flattened row-major:
/// `logits` is queries x classes, `boxes` is queries x 4 (center x, center y,
/// width, height; normalized to the model's input, origin top left).
public struct ModelOutput: Sendable, Equatable {
    public var queries: Int
    public var classes: Int
    public var logits: [Float]
    public var boxes: [Float]

    public init(queries: Int, classes: Int, logits: [Float], boxes: [Float]) {
        precondition(logits.count == queries * classes && boxes.count == queries * 4)
        self.queries = queries
        self.classes = classes
        self.logits = logits
        self.boxes = boxes
    }
}

/// A decoded box in the model's input-normalized coordinates.
public struct ModelBox: Sendable, Equatable {
    public var classID: Int
    public var score: Float
    public var box: NormRect
}

/// Turns RT-DETR's tensors into boxes. The model needs no suppression of
/// overlaps (each query is one object); per query this reports its best class.
public enum RTDETRDecoder {
    /// `scoreFloor` only drops noise (the default 0.05 keeps the output of the
    /// 300 queries small); it is not a decision threshold, the caller's rules
    /// choose those. Pass 0 to get every query.
    public static func decode(_ output: ModelOutput, scoreFloor: Float = 0.05) -> [ModelBox] {
        var result: [ModelBox] = []
        for q in 0..<output.queries {
            let row = q * output.classes
            var best = 0
            var bestLogit = -Float.infinity
            for c in 0..<output.classes where output.logits[row + c] > bestLogit {
                bestLogit = output.logits[row + c]
                best = c
            }
            let score = 1 / (1 + exp(-bestLogit))
            guard score >= scoreFloor else { continue }
            let b = q * 4
            let cx = Double(output.boxes[b]), cy = Double(output.boxes[b + 1])
            let w = Double(output.boxes[b + 2]), h = Double(output.boxes[b + 3])
            result.append(ModelBox(
                classID: best, score: score,
                box: NormRect(x: cx - w / 2, y: cy - h / 2, width: w, height: h)))
        }
        return result
    }

    /// Reads a float16 or float32 tensor of shape (1, rows, columns), strides
    /// honored, into a flat float array.
    static func floats(_ array: MLMultiArray, rows: Int, columns: Int) -> [Float] {
        let rowStride = array.strides[array.strides.count - 2].intValue
        let colStride = array.strides[array.strides.count - 1].intValue
        var out = [Float](repeating: 0, count: rows * columns)
        array.withUnsafeBytes { raw in
            switch array.dataType {
            case .float16:
                let p = raw.bindMemory(to: Float16.self)
                for r in 0..<rows {
                    for c in 0..<columns { out[r * columns + c] = Float(p[r * rowStride + c * colStride]) }
                }
            case .float32:
                let p = raw.bindMemory(to: Float.self)
                for r in 0..<rows {
                    for c in 0..<columns { out[r * columns + c] = p[r * rowStride + c * colStride] }
                }
            default:
                preconditionFailure("unsupported tensor type \(array.dataType)")
            }
        }
        return out
    }
}
