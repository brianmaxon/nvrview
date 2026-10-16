import CoreGraphics
import Foundation

// What the smart-alert filters learned (docs/NOTIFICATIONS.md "Cheap
// filters", FFI minor 17): per camera, a list of generic insight layers the
// Events window's Scene Memory pane draws without knowing the technique
// behind them. A layer is a `grid` (a heatmap), `boxes`, or `scalar` values;
// a kind this app does not know is listed with its description only. Pure:
// parsing, the color scale, and the heatmap raster.

/// `nv_alert_insights_get` with no camera: the cameras with a memory.
public struct InsightCameraList: Sendable, Equatable, Decodable {
    public struct Entry: Sendable, Equatable, Decodable {
        public var camera: AlertCamera
        public var classes: [String]
    }

    public var cameras: [Entry]

    public static func parse(_ text: String) -> InsightCameraList? {
        try? JSONDecoder().decode(InsightCameraList.self, from: Data(text.utf8))
    }
}

/// One camera's layers.
public struct AlertInsights: Sendable, Equatable, Decodable {
    /// The newest kept evidence frame of the camera, to draw the layers on.
    public struct Picture: Sendable, Equatable, Decodable {
        public var triggerID: UInt64
        public var frame: Int
        public var atMs: Int64

        enum CodingKeys: String, CodingKey {
            case frame
            case triggerID = "trigger_id"
            case atMs = "at_ms"
        }
    }

    public var camera: AlertCamera
    public var atMs: Int64
    /// The night thresholds are in force now.
    public var night: Bool
    public var layers: [InsightLayer]
    public var picture: Picture?

    enum CodingKeys: String, CodingKey {
        case camera, night, layers, picture
        case atMs = "at_ms"
    }

    public static func parse(_ text: String) -> AlertInsights? {
        try? JSONDecoder().decode(AlertInsights.self, from: Data(text.utf8))
    }

    /// The heatmap layers (the class picker's choices).
    public var gridLayers: [InsightLayer] {
        layers.filter { if case .grid = $0.data { true } else { false } }
    }

    public var boxLayers: [InsightLayer] {
        layers.filter { if case .boxes = $0.data { true } else { false } }
    }

    public var scalarLayers: [InsightLayer] {
        layers.filter { if case .scalar = $0.data { true } else { false } }
    }
}

public struct InsightGrid: Sendable, Equatable {
    public var rows: Int
    public var cols: Int
    /// `rows * cols` values, row by row from the top left.
    public var values: [Double]
    public var className: String?
    public var threshold: Double?
    public var updatedMs: Int64?

    public init(
        rows: Int, cols: Int, values: [Double], className: String? = nil, threshold: Double? = nil,
        updatedMs: Int64? = nil
    ) {
        self.rows = rows
        self.cols = cols
        self.values = values
        self.className = className
        self.threshold = threshold
        self.updatedMs = updatedMs
    }

    public func value(row: Int, col: Int) -> Double? {
        guard row >= 0, row < rows, col >= 0, col < cols, row * cols + col < values.count else { return nil }
        return values[row * cols + col]
    }

    /// Cells at or above the threshold.
    public var hotCells: Int { threshold.map { t in values.filter { $0 >= t }.count } ?? 0 }
}

public struct InsightBox: Sendable, Equatable {
    public var className: String
    public var score: Double
    public var x: Double
    public var y: Double
    public var w: Double
    public var h: Double
}

public struct InsightScalar: Sendable, Equatable, Identifiable {
    public var id: String
    public var label: String
    public var value: Double
    public var units: String

    /// "0.50 score", "30 min".
    public var text: String {
        let v = value.rounded() == value && abs(value) >= 1 ? String(Int(value)) : String(format: "%.2f", value)
        return units.isEmpty ? v : "\(v) \(units)"
    }
}

public struct InsightLayer: Sendable, Equatable, Decodable, Identifiable {
    public enum Content: Sendable, Equatable {
        case grid(InsightGrid)
        case boxes([InsightBox], atMs: Int64?)
        case scalar([InsightScalar])
        /// A kind this app cannot draw (a newer core).
        case other(kind: String)
    }

    public var id: String
    public var title: String
    public var description: String
    public var provenance: String
    public var units: String
    public var range: ClosedRange<Double>
    public var data: Content

    private struct Box: Decodable {
        struct Rect: Decodable { var x, y, w, h: Double }
        var `class`: String
        var score: Double
        var box: Rect
    }

    private struct Item: Decodable {
        var id: String
        var label: String
        var value: Double
        var units: String
    }

    enum CodingKeys: String, CodingKey {
        case id, title, description, provenance, units, range, kind, rows, cols, values, threshold, boxes, items
        case className = "class"
        case updatedMs = "updated_ms"
        case atMs = "at_ms"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? id
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        provenance = try c.decodeIfPresent(String.self, forKey: .provenance) ?? ""
        units = try c.decodeIfPresent(String.self, forKey: .units) ?? ""
        let r = try c.decodeIfPresent([Double].self, forKey: .range) ?? [0, 1]
        let lo = r.first ?? 0
        let hi = r.count > 1 ? r[1] : 1
        range = lo < hi ? lo...hi : 0...1
        let kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? ""
        switch kind {
        case "grid":
            let rows = try c.decode(Int.self, forKey: .rows)
            let cols = try c.decode(Int.self, forKey: .cols)
            let values = try c.decode([Double].self, forKey: .values)
            guard rows > 0, cols > 0, values.count == rows * cols else {
                throw DecodingError.dataCorruptedError(forKey: .values, in: c, debugDescription: "grid size")
            }
            data = .grid(
                InsightGrid(
                    rows: rows, cols: cols, values: values,
                    className: try c.decodeIfPresent(String.self, forKey: .className),
                    threshold: try c.decodeIfPresent(Double.self, forKey: .threshold),
                    updatedMs: try c.decodeIfPresent(Int64.self, forKey: .updatedMs)))
        case "boxes":
            let boxes = try c.decodeIfPresent([Box].self, forKey: .boxes) ?? []
            data = .boxes(
                boxes.map { InsightBox(className: $0.class, score: $0.score, x: $0.box.x, y: $0.box.y, w: $0.box.w, h: $0.box.h) },
                atMs: try c.decodeIfPresent(Int64.self, forKey: .atMs))
        case "scalar":
            let items = try c.decodeIfPresent([Item].self, forKey: .items) ?? []
            data = .scalar(items.map { InsightScalar(id: $0.id, label: $0.label, value: $0.value, units: $0.units) })
        default:
            data = .other(kind: kind)
        }
    }

    public var grid: InsightGrid? { if case .grid(let g) = data { g } else { nil } }
}

/// The heatmap's color scale: dark blue through teal and green to yellow, in
/// equal steps of lightness (a viridis-like ramp, readable for the common
/// color-vision deficiencies).
public enum InsightColorScale {
    /// The stops, from the range's low end to its high end (sRGB, 0...1).
    public static let stops: [(r: Double, g: Double, b: Double)] = [
        (0.267, 0.005, 0.329),
        (0.229, 0.322, 0.546),
        (0.128, 0.567, 0.551),
        (0.369, 0.789, 0.383),
        (0.993, 0.906, 0.144),
    ]

    /// The color of `value` within `range` (clamped).
    public static func color(_ value: Double, in range: ClosedRange<Double>) -> (r: Double, g: Double, b: Double) {
        let span = range.upperBound - range.lowerBound
        let t = span > 0 ? min(max((value - range.lowerBound) / span, 0), 1) : 0
        let x = t * Double(stops.count - 1)
        let i = min(Int(x), stops.count - 2)
        let f = x - Double(i)
        let a = stops[i], b = stops[i + 1]
        return (a.r + (b.r - a.r) * f, a.g + (b.g - a.g) * f, a.b + (b.b - a.b) * f)
    }

    /// The grid as a picture, one pixel per cell (top row first), drawn with
    /// the scale. Cells at the range's low end are transparent, so the
    /// picture under the overlay shows where nothing was learned; the others
    /// are opaque and the overlay's opacity applies on top.
    public static func image(_ grid: InsightGrid, range: ClosedRange<Double>) -> CGImage? {
        guard grid.rows > 0, grid.cols > 0, grid.values.count == grid.rows * grid.cols else { return nil }
        var pixels = [UInt8](repeating: 0, count: grid.rows * grid.cols * 4)
        for (i, v) in grid.values.enumerated() where v > range.lowerBound {
            let c = color(v, in: range)
            pixels[i * 4] = UInt8((c.r * 255).rounded())
            pixels[i * 4 + 1] = UInt8((c.g * 255).rounded())
            pixels[i * 4 + 2] = UInt8((c.b * 255).rounded())
            pixels[i * 4 + 3] = 255
        }
        guard let provider = CGDataProvider(data: Data(pixels) as CFData) else { return nil }
        return CGImage(
            width: grid.cols, height: grid.rows, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: grid.cols * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }
}
