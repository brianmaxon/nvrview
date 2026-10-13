import CoreGraphics
import Foundation

// Verification evidence (docs/NOTIFICATIONS.md "Evidence", minor 14): the
// frames a check judged and what each method found in them, kept for a short
// time so a person can review why an alert was or was not raised. The core
// stores and expires the sets (`nv_alert_evidence_*`); the app writes the
// frames and its detector's boxes. Pure: parsing, the overlay mapping, the
// agreement of the methods, and the summary over a window.

/// An alert class for a detector label.
public enum EvidenceClass {
    public static func of(_ label: String) -> String? {
        switch label {
        case "person", "vehicle", "animal": label
        case "car", "truck", "bus", "motorcycle", "bicycle": "vehicle"
        case "dog", "cat", "bird", "horse": "animal"
        default: nil
        }
    }
}

/// One box a method found, normalized to the picture.
public struct EvidenceBox: Sendable, Equatable {
    public var label: String
    public var score: Double
    public var x: Double
    public var y: Double
    public var w: Double
    public var h: Double

    public init(label: String, score: Double, x: Double, y: Double, w: Double, h: Double) {
        self.label = label
        self.score = score
        self.x = x
        self.y = y
        self.w = w
        self.h = h
    }

    public var alertClass: String? { EvidenceClass.of(label) }

    init?(_ j: JSONValue) {
        guard let label = j["label"]?.string, let score = j["score"]?.double else { return nil }
        let b = j["box"] ?? j
        guard let x = b["x"]?.double, let y = b["y"]?.double, let w = b["w"]?.double, let h = b["h"]?.double else {
            return nil
        }
        self.init(label: label, score: score, x: x, y: y, w: w, h: h)
    }
}

/// One method's claim and boxes: the camera's AI, our detector, or another
/// verifier that ran.
public struct EvidenceMethod: Sendable, Equatable, Identifiable {
    /// `camera_ai`, `detector`, or another key.
    public var name: String
    /// The class it claimed (the camera AI), if any.
    public var claimed: String?
    /// The boxes by frame index.
    public var boxes: [Int: [EvidenceBox]]

    public var id: String { name }

    public init(name: String, claimed: String? = nil, boxes: [Int: [EvidenceBox]] = [:]) {
        self.name = name
        self.claimed = claimed
        self.boxes = boxes
    }

    public var title: String {
        switch name {
        case "camera_ai": "Camera AI"
        case "detector": "Our detector"
        default: name.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    /// The classes found at or above `minScore`, over every frame.
    public func foundClasses(minScore: Double) -> Set<String> {
        var out: Set<String> = []
        for list in boxes.values {
            for b in list where b.score >= minScore {
                if let c = b.alertClass { out.insert(c) }
            }
        }
        return out
    }

    /// The best score for a class over every frame.
    public func bestScore(for cls: String) -> Double? {
        boxes.values.flatMap { $0 }.filter { $0.alertClass == cls }.map(\.score).max()
    }

    public init?(name: String, _ j: JSONValue) {
        self.name = name
        claimed = j["claimed"]?.string
        var map: [Int: [EvidenceBox]] = [:]
        // Boxes by frame index (an object, so each frame's put adds to it).
        if case .object(let frames)? = j["frames"] {
            for (key, f) in frames {
                guard let index = Int(key) else { continue }
                map[index, default: []] += (f["boxes"]?.array ?? []).compactMap(EvidenceBox.init)
            }
        }
        boxes = map
    }
}

/// How the camera's AI and our detector compare for one set.
public enum EvidenceAgreement: Sendable, Equatable {
    /// Both found the class.
    case agree
    /// One found something the other did not.
    case disagree
    /// Only one method ran (a sensor trigger, or no detector).
    case notApplicable
}

/// A kept set (`nv_alert_evidence_get` with a trigger).
public struct AlertEvidence: Sendable, Equatable, Identifiable {
    public struct Frame: Sendable, Equatable {
        public var index: Int
        public var atMs: Int64
        public var bytes: Int64
    }

    public var triggerID: UInt64
    public var createdMs: Int64
    public var expiresMs: Int64?
    public var pinned: Bool
    public var frames: [Frame]
    public var methods: [EvidenceMethod]
    /// What triggered the check: `device`, `external`, ...
    public var triggerSource: String?
    public var triggerClass: String?
    public var minScore: Double
    public var isTest: Bool
    public var decision: String?
    public var alertID: UInt64?

    public var id: UInt64 { triggerID }

    public init?(_ j: JSONValue) {
        guard let t = j["trigger_id"]?.int64 else { return nil }
        triggerID = UInt64(max(t, 0))
        createdMs = j["created_ms"]?.int64 ?? 0
        expiresMs = j["expires_ms"]?.int64
        pinned = j["pinned"]?.bool ?? false
        frames = (j["frames"]?.array ?? []).compactMap { f in
            guard let i = f["index"]?.int64 else { return nil }
            return Frame(index: Int(i), atMs: f["at_ms"]?.int64 ?? 0, bytes: f["bytes"]?.int64 ?? 0)
        }.sorted { $0.index < $1.index }
        var list: [EvidenceMethod] = []
        if case .object(let o)? = j["methods"] {
            for name in o.keys.sorted() {
                if let m = EvidenceMethod(name: name, o[name] ?? .null) { list.append(m) }
            }
        }
        // The camera AI first, then ours, then the others.
        let order = ["camera_ai": 0, "detector": 1]
        methods = list.sorted { (order[$0.name] ?? 2, $0.name) < (order[$1.name] ?? 2, $1.name) }
        triggerSource = j["trigger"]?["source"]?.string
        triggerClass = j["trigger"]?["class"]?.string
        minScore = j["min_score"]?.double ?? 0.5
        isTest = j["test"]?.bool ?? false
        decision = j["decision"]?["decision"]?.string
        alertID = j["decision"]?["alert_id"]?.int64.map { UInt64(max($0, 0)) }
    }

    public func method(_ name: String) -> EvidenceMethod? { methods.first { $0.name == name } }

    /// The class the camera's AI claimed: its own entry, else the trigger's
    /// class when a camera (not a sensor) raised it.
    public var cameraClaim: String? {
        if let c = method("camera_ai")?.claimed { return c }
        if triggerSource == "device" || triggerSource == "nvr_input" { return triggerClass }
        return nil
    }

    public var detectorClasses: Set<String> { method("detector")?.foundClasses(minScore: minScore) ?? [] }

    public var agreement: EvidenceAgreement {
        guard method("detector") != nil, let claim = cameraClaim, ["person", "vehicle", "animal"].contains(claim) else {
            return .notApplicable
        }
        return detectorClasses.contains(claim) ? .agree : .disagree
    }

    /// Shown as the "methods disagree" badge.
    public var methodsDisagree: Bool { agreement == .disagree }
}

/// Where boxes land in a view that shows a picture aspect-fit.
public enum EvidenceOverlay {
    /// The picture's rectangle inside `view` (aspect-fit, centered).
    public static func pictureRect(picture: CGSize, in view: CGSize) -> CGRect {
        guard picture.width > 0, picture.height > 0, view.width > 0, view.height > 0 else { return .zero }
        let scale = min(view.width / picture.width, view.height / picture.height)
        let size = CGSize(width: picture.width * scale, height: picture.height * scale)
        return CGRect(
            x: (view.width - size.width) / 2, y: (view.height - size.height) / 2, width: size.width, height: size.height)
    }

    /// A normalized box in view coordinates (clamped to the picture).
    public static func rect(for box: EvidenceBox, picture: CGSize, in view: CGSize) -> CGRect {
        let p = pictureRect(picture: picture, in: view)
        let x0 = min(max(box.x, 0), 1)
        let y0 = min(max(box.y, 0), 1)
        let x1 = min(max(box.x + box.w, 0), 1)
        let y1 = min(max(box.y + box.h, 0), 1)
        return CGRect(
            x: p.minX + x0 * p.width, y: p.minY + y0 * p.height, width: (x1 - x0) * p.width, height: (y1 - y0) * p.height)
    }

    /// One drawn box.
    public struct Drawn: Equatable {
        public var method: String
        /// Position of the method in the set (the view's color).
        public var colorIndex: Int
        public var rect: CGRect
        public var caption: String
    }

    /// The boxes of frame `frame` for the methods that are on.
    public static func layout(
        _ evidence: AlertEvidence, frame: Int, visible: Set<String>, picture: CGSize, in view: CGSize
    ) -> [Drawn] {
        var out: [Drawn] = []
        for (i, m) in evidence.methods.enumerated() where visible.contains(m.name) {
            for b in m.boxes[frame] ?? [] {
                out.append(
                    Drawn(
                        method: m.name, colorIndex: i, rect: rect(for: b, picture: picture, in: view),
                        caption: "\(b.label) \(Int((b.score * 100).rounded()))%"))
            }
        }
        return out
    }
}

/// How often the camera's AI and our detector agreed over a window of kept
/// sets, per class.
public struct EvidenceComparison: Sendable, Equatable {
    public struct Row: Sendable, Equatable {
        public var cls: String
        /// Both found it.
        public var agreed = 0
        /// The camera's AI claimed it, our detector did not find it.
        public var cameraOnly = 0
        /// Our detector found it, the camera's AI did not claim it.
        public var detectorOnly = 0
        public var total: Int { agreed + cameraOnly + detectorOnly }
    }

    public var rows: [Row] = []
    /// Sets that had both methods.
    public var compared = 0

    public init(_ sets: [AlertEvidence]) {
        var map: [String: Row] = [:]
        for s in sets where s.agreement != .notApplicable {
            compared += 1
            let claim = s.cameraClaim
            let found = s.detectorClasses
            for cls in found.union(claim.map { [$0] } ?? []) where ["person", "vehicle", "animal"].contains(cls) {
                var r = map[cls] ?? Row(cls: cls)
                switch (claim == cls, found.contains(cls)) {
                case (true, true): r.agreed += 1
                case (true, false): r.cameraOnly += 1
                default: r.detectorOnly += 1
                }
                map[cls] = r
            }
        }
        rows = map.values.sorted { $0.cls < $1.cls }
    }

    /// "Person: 5 agreed, 1 camera only, 0 detector only".
    public func line(_ row: Row) -> String {
        "\(row.cls.capitalized): \(row.agreed) agreed, \(row.cameraOnly) camera only, \(row.detectorOnly) detector only"
    }
}

/// The evidence list answer (`nv_alert_evidence_get`).
public struct EvidenceList: Sendable, Equatable {
    public var sets: [AlertEvidence]
    public var retentionMinutes: Int

    public static func parse(_ text: String) -> EvidenceList? {
        guard let v = JSONValue.parse(text), let list = v["sets"]?.array else { return nil }
        return EvidenceList(
            sets: list.compactMap(AlertEvidence.init), retentionMinutes: Int(v["retention_min"]?.int64 ?? 120))
    }
}

/// What the app puts with `nv_alert_evidence_put`.
public enum EvidenceDocument {
    /// `{"trigger_id","frame","at_ms","doc"}` for one picture and the boxes
    /// each method found in it.
    public static func request(
        triggerID: UInt64, frame: Int, atMs: Int64, methods: [String: (claimed: String?, boxes: [EvidenceBox])]
    ) -> String {
        var m: [String: Any] = [:]
        for (name, v) in methods {
            var entry: [String: Any] = [
                "frames": [
                    String(frame): [
                        "boxes": v.boxes.map { b -> [String: Any] in
                            [
                                "label": b.label, "score": b.score,
                                "box": ["x": b.x, "y": b.y, "w": b.w, "h": b.h],
                            ]
                        }
                    ]
                ]
            ]
            if let c = v.claimed { entry["claimed"] = c }
            m[name] = entry
        }
        let o: [String: Any] = [
            "trigger_id": triggerID, "frame": frame, "at_ms": atMs, "doc": ["methods": m],
        ]
        let data = (try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}
