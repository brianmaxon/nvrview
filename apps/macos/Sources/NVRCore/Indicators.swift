import Foundation

// Corner indicators (docs/UX.md "Corner indicators"): small symbols for what
// is being detected now, always visible. Pure pieces, tested without a window.

/// Which corner of a tile holds the indicators (a setting; top right by
/// default, so they can avoid timestamps burned into the picture).
public enum ChromeCorner: String, Sendable, CaseIterable, Equatable {
    case topLeft, topRight, bottomLeft, bottomRight

    public static let defaultsKey = "indicatorCorner"
    public static let fallback: ChromeCorner = .topRight

    public var title: String {
        switch self {
        case .topLeft: "Top Left"
        case .topRight: "Top Right"
        case .bottomLeft: "Bottom Left"
        case .bottomRight: "Bottom Right"
        }
    }

    /// The remembered corner; anything missing or unknown is the default.
    public static func load(from defaults: UserDefaults = .standard) -> ChromeCorner {
        defaults.string(forKey: defaultsKey).flatMap(ChromeCorner.init(rawValue:)) ?? fallback
    }

    public func save(to defaults: UserDefaults = .standard) {
        defaults.set(rawValue, forKey: Self.defaultsKey)
    }

    /// Where a row of `count` symbols of `symbol` points (with `gap` between
    /// and `inset` from the edges) starts, in a top-left-origin tile of
    /// `size`. The row grows away from the corner's edge.
    public func rowOrigin(count: Int, symbol: Double, gap: Double, inset: Double, in size: (width: Double, height: Double))
        -> (x: Double, y: Double)
    {
        let rowWidth = Double(count) * symbol + Double(max(count - 1, 0)) * gap
        let x = (self == .topLeft || self == .bottomLeft) ? inset : size.width - inset - rowWidth
        let y = (self == .topLeft || self == .topRight) ? inset : size.height - inset - symbol
        return (x, y)
    }
}

/// What an indicator stands for.
public enum IndicatorKind: Hashable, Sendable, CaseIterable {
    case motion, person, vehicle, animal, face, package, visitor, smokeAlarm, coAlarm, other

    public init(detectionKind: String) {
        switch detectionKind {
        case DetectionEvent.motionKind: self = .motion
        case "people", "person": self = .person
        case "vehicle": self = .vehicle
        case "dog_cat", "animal": self = .animal
        case "face": self = .face
        case "package": self = .package
        case "visitor": self = .visitor
        case "smoke_alarm": self = .smokeAlarm
        case "co_alarm": self = .coAlarm
        default: self = .other
        }
    }

    public var label: String {
        switch self {
        case .motion: "Motion"
        case .person: "Person"
        case .vehicle: "Vehicle"
        case .animal: "Animal"
        case .face: "Face"
        case .package: "Package"
        case .visitor: "Visitor"
        case .smokeAlarm: "Smoke alarm"
        case .coAlarm: "CO alarm"
        case .other: "Detection"
        }
    }

    /// An SF Symbol name.
    public var symbolName: String {
        switch self {
        case .motion: "dot.radiowaves.left.and.right"
        case .person: "person.fill"
        case .vehicle: "car.fill"
        case .animal: "pawprint.fill"
        case .face: "face.smiling"
        case .package: "shippingbox.fill"
        case .visitor: "bell.fill"
        case .smokeAlarm: "smoke.fill"
        case .coAlarm: "carbon.monoxide.cloud.fill"
        case .other: "circle.dotted"
        }
    }

    /// Sound alarms get the restrained accent; everything else is monochrome.
    public var isSound: Bool { self == .smokeAlarm || self == .coAlarm }
}

/// One symbol to show: a kind and where it was detected, in words.
public struct IndicatorItem: Sendable, Equatable {
    public var kind: IndicatorKind
    /// "camera AI", "this Mac", "sound"; several joined with " and ".
    public var source: String

    public init(kind: IndicatorKind, source: String) {
        self.kind = kind
        self.source = source
    }

    /// The tooltip and accessibility label, such as "Person (camera AI)".
    public var description: String { "\(kind.label) (\(source))" }

    /// The kinds active for one camera, one item per kind in a fixed order.
    /// Empty means nothing is detected now, never "all clear".
    public static func items(from active: [DetectionTracker.Active]) -> [IndicatorItem] {
        var sources: [IndicatorKind: [String]] = [:]
        for a in active {
            let kind = IndicatorKind(detectionKind: a.kind)
            let s = DetectionTracker.sourceLabel(a.source)
            if !(sources[kind] ?? []).contains(s) { sources[kind, default: []].append(s) }
        }
        return IndicatorKind.allCases.compactMap { k in
            sources[k].map { IndicatorItem(kind: k, source: $0.joined(separator: " and ")) }
        }
    }
}

/// Keeps a kind showing for a short while after it stops, so brief gaps do
/// not flicker. Time is passed in (seconds on any steady clock).
public struct IndicatorHold: Sendable, Equatable {
    public static let holdSeconds = 2.0

    private struct Entry: Sendable, Equatable {
        var item: IndicatorItem
        /// Still detected as of the last update.
        var active: Bool
        /// When it stopped (meaningful when not active).
        var stoppedAt: Double
    }
    private var entries: [IndicatorKind: Entry] = [:]

    public init() {}

    /// Folds in what is active at `now`.
    public mutating func update(_ active: [IndicatorItem], now: Double) {
        let live = Set(active.map(\.kind))
        for item in active { entries[item.kind] = Entry(item: item, active: true, stoppedAt: 0) }
        for (kind, var e) in entries where !live.contains(kind) {
            if e.active { e.active = false; e.stoppedAt = now }
            entries[kind] = now - e.stoppedAt >= Self.holdSeconds ? nil : e
        }
    }

    /// What to show at `now`: active kinds and those stopped under the hold.
    public func visible(now: Double) -> [IndicatorItem] {
        IndicatorKind.allCases.compactMap { k in
            guard let e = entries[k], e.active || now - e.stoppedAt < Self.holdSeconds else { return nil }
            return e.item
        }
    }

    /// When the next held kind runs out, so a timer can re-evaluate.
    public func nextExpiry(now: Double) -> Double? {
        entries.values.filter { !$0.active }.map { $0.stoppedAt + Self.holdSeconds }.filter { $0 > now }.min()
    }
}

extension IndicatorItem {
    /// One item per kind across several lists (several lenses or cameras),
    /// sources joined, in the fixed kind order.
    public static func union(_ lists: [[IndicatorItem]]) -> [IndicatorItem] {
        var sources: [IndicatorKind: [String]] = [:]
        for item in lists.joined() {
            for s in item.source.components(separatedBy: " and ") where !(sources[item.kind] ?? []).contains(s) {
                sources[item.kind, default: []].append(s)
            }
        }
        return IndicatorKind.allCases.compactMap { k in
            sources[k].map { IndicatorItem(kind: k, source: $0.joined(separator: " and ")) }
        }
    }
}

extension DetectionTracker {
    /// Every camera with something active, sound included on every lens.
    public var activeCameras: [CameraRef] {
        var out = Set(activeKeys)
        for c in soundKeys { for lens in 0...CameraRef.maxLens { out.insert(CameraRef(device: c.device, channel: c.channel, lens: lens)) } }
        return Array(out)
    }
}

/// The sidebar's view of the detections: per camera, what to show now with
/// the same hold as the tiles. Updated on detection events and a slow tick,
/// never per frame.
public struct IndicatorBoard: Sendable, Equatable {
    private var holds: [CameraRef: IndicatorHold] = [:]
    private var shown: [CameraRef: [IndicatorItem]] = [:]

    public init() {}

    /// Re-reads the tracker at `now`; returns whether anything shown changed.
    @discardableResult
    public mutating func update(_ tracker: DetectionTracker, now: Double) -> Bool {
        let before = shown
        let live = Set(tracker.activeCameras)
        for camera in live.union(holds.keys) {
            var h = holds[camera] ?? IndicatorHold()
            h.update(IndicatorItem.items(from: tracker.active(for: camera)), now: now)
            let v = h.visible(now: now)
            holds[camera] = (v.isEmpty && !live.contains(camera)) ? nil : h
            shown[camera] = v.isEmpty ? nil : v
        }
        return shown != before
    }

    /// One camera (one lens).
    public func items(for camera: CameraRef) -> [IndicatorItem] { shown[camera] ?? [] }

    /// A multi-lens channel's row summarizes all its lenses.
    public func items(device: String, channel: Int) -> [IndicatorItem] {
        IndicatorItem.union((0...CameraRef.maxLens).map { items(for: CameraRef(device: device, channel: channel, lens: $0)) })
    }

    /// A collapsed device row: the union of its cameras' kinds.
    public func items(device: String) -> [IndicatorItem] {
        IndicatorItem.union(shown.filter { $0.key.device == device }.map(\.value))
    }
}
