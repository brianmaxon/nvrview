import Foundation

/// A rectangle in a picture's normalized coordinates: origin at the top left,
/// both axes 0...1 across the whole picture.
public struct NormRect: Sendable, Equatable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public static let whole = NormRect(x: 0, y: 0, width: 1, height: 1)

    public var maxX: Double { x + width }
    public var maxY: Double { y + height }

    /// The part of the rectangle inside the unit square.
    public func clampedToUnit() -> NormRect {
        let x0 = min(max(x, 0), 1), y0 = min(max(y, 0), 1)
        let x1 = min(max(maxX, 0), 1), y1 = min(max(maxY, 0), 1)
        return NormRect(x: x0, y: y0, width: x1 - x0, height: y1 - y0)
    }
}

/// One box as the model reported it, with no threshold applied beyond the
/// score floor the decoder was given and no suppression of overlaps.
public struct RawDetection: Sendable, Equatable {
    /// The model's class index (COCO order for RT-DETR: 0 person, 1 bicycle,
    /// 2 car, ...). `DetectorModelInfo.labels` names them.
    public var classID: Int
    /// The class probability, 0...1.
    public var score: Float
    /// In the normalized coordinates of the original picture (not of the crop).
    public var box: NormRect

    public init(classID: Int, score: Float, box: NormRect) {
        self.classID = classID
        self.score = score
        self.box = box
    }
}

/// The detections of one crop.
public struct CropDetections: Sendable, Equatable {
    public var crop: NormRect
    public var detections: [RawDetection]

    public init(crop: NormRect, detections: [RawDetection]) {
        self.crop = crop
        self.detections = detections
    }
}

/// How a crop is fitted to the model's square input.
public enum InputFit: Sendable, Equatable {
    /// Stretched to fill the input, as the model's training pipeline does
    /// (docs/VISION.md measured it this way). The default.
    case stretch
    /// Scaled to fit with the aspect ratio kept, centered, padded with black.
    case letterbox
}

public enum CropPlan {
    /// The whole picture plus four overlapping tiles (each 60 % of the picture
    /// across, 20 % overlap), the plan whose recall docs/VISION.md measured.
    public static let wholeFramePlusTiles: [NormRect] = [.whole] + tiles

    public static let tiles: [NormRect] = [
        NormRect(x: 0, y: 0, width: 0.6, height: 0.6),
        NormRect(x: 0.4, y: 0, width: 0.6, height: 0.6),
        NormRect(x: 0, y: 0.4, width: 0.6, height: 0.6),
        NormRect(x: 0.4, y: 0.4, width: 0.6, height: 0.6),
    ]
}

/// The coordinate arithmetic between a model box, its crop and the original
/// picture. Pure, so it is tested without a model.
public enum CropGeometry {
    /// The crop's pixel rectangle in a picture of `width` x `height`, rounded
    /// outward to whole pixels and kept inside the picture, at least 1 x 1.
    public static func pixelRect(of crop: NormRect, pictureWidth: Int, pictureHeight: Int)
        -> (x: Int, y: Int, width: Int, height: Int)
    {
        let c = crop.clampedToUnit()
        let x0 = min(max(Int((c.x * Double(pictureWidth)).rounded(.down)), 0), pictureWidth - 1)
        let y0 = min(max(Int((c.y * Double(pictureHeight)).rounded(.down)), 0), pictureHeight - 1)
        let x1 = min(max(Int((c.maxX * Double(pictureWidth)).rounded(.up)), x0 + 1), pictureWidth)
        let y1 = min(max(Int((c.maxY * Double(pictureHeight)).rounded(.up)), y0 + 1), pictureHeight)
        return (x0, y0, x1 - x0, y1 - y0)
    }

    /// Where the crop's pixels land in the square input, in input-normalized
    /// units: the whole input when stretched, the centered content area when
    /// letterboxed.
    public static func contentRect(cropPixelWidth: Int, cropPixelHeight: Int, fit: InputFit) -> NormRect {
        switch fit {
        case .stretch:
            return .whole
        case .letterbox:
            let w = Double(cropPixelWidth), h = Double(cropPixelHeight)
            let side = max(w, h)
            let cw = w / side, ch = h / side
            return NormRect(x: (1 - cw) / 2, y: (1 - ch) / 2, width: cw, height: ch)
        }
    }

    /// Maps a box in the model's input-normalized coordinates to the original
    /// picture's normalized coordinates. `cropPixelWidth/Height` are the
    /// crop's size in the original picture's pixels (they matter for the
    /// letterbox only). The result is clamped to the picture.
    public static func toPicture(
        _ box: NormRect, crop: NormRect, cropPixelWidth: Int, cropPixelHeight: Int, fit: InputFit
    ) -> NormRect {
        let content = contentRect(cropPixelWidth: cropPixelWidth, cropPixelHeight: cropPixelHeight, fit: fit)
        // Input -> crop-normalized (undo the padding), then crop -> picture.
        let cx = (box.x - content.x) / content.width
        let cy = (box.y - content.y) / content.height
        let cw = box.width / content.width
        let ch = box.height / content.height
        return NormRect(
            x: crop.x + cx * crop.width, y: crop.y + cy * crop.height,
            width: cw * crop.width, height: ch * crop.height
        ).clampedToUnit()
    }
}
