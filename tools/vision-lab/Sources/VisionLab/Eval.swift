import Foundation

/// Ground truth for one picture: which classes are present.
public struct Label: Sendable {
    public var frame: Int
    public var present: Set<ObjectClass>

    public init(frame: Int, present: Set<ObjectClass>) {
        self.frame = frame
        self.present = present
    }
}

/// Reads a label file: a header `frame,person,vehicle,animal` and one row
/// per labeled picture with 0/1 values. Lines starting with `#` are ignored.
public func parseLabels(_ text: String) throws -> [Label] {
    var labels: [Label] = []
    var columns: [String] = []
    for raw in text.split(whereSeparator: \.isNewline) {
        let line = raw.trimmingCharacters(in: .whitespaces)
        if line.isEmpty || line.hasPrefix("#") { continue }
        let fields = line.split(separator: ",", omittingEmptySubsequences: false).map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        if columns.isEmpty {
            columns = fields.map { $0.lowercased() }
            guard columns.first == "frame" else { throw LabError.message("label header must start with 'frame'") }
            continue
        }
        guard fields.count == columns.count, let frame = Int(fields[0]) else {
            throw LabError.message("bad label row: \(line)")
        }
        var present = Set<ObjectClass>()
        for (name, value) in zip(columns.dropFirst(), fields.dropFirst()) {
            if let cls = ObjectClass(rawValue: name), value == "1" { present.insert(cls) }
        }
        labels.append(Label(frame: frame, present: present))
    }
    return labels
}

/// Reads a results file written by `run --results`.
public func parseResults(_ data: Data) throws -> [FrameResult] {
    let decoder = JSONDecoder()
    return try data.split(separator: 0x0A).filter { !$0.isEmpty }.map {
        try decoder.decode(FrameResult.self, from: Data($0))
    }
}

/// True when `detector` (or any detector, for "any") found `cls` at or above
/// `threshold` in this picture. A detection counts only when it is at least
/// `minArea` of the picture (to drop specks), measured on its box.
public func fires(
    _ frame: FrameResult, detector: String, cls: ObjectClass, threshold: Float, minArea: Float = 0
) -> Bool {
    let lists = detector == "any" ? Array(frame.results.values) : [frame.results[detector]].compactMap { $0 }
    return lists.contains { r in
        r.detections.contains { d in
            d.cls == cls && d.confidence >= threshold && (d.box.count < 4 || d.box[2] * d.box[3] >= minArea)
        }
    }
}

public struct Score: Codable, Sendable {
    public var tp = 0
    public var fp = 0
    public var fn = 0
    public var tn = 0

    public var precision: Double? { tp + fp == 0 ? nil : Double(tp) / Double(tp + fp) }
    public var recall: Double? { tp + fn == 0 ? nil : Double(tp) / Double(tp + fn) }

    public init() {}
}

/// Picture-level presence scores of one detector and class over the labeled
/// pictures that also have results.
public func scoreFrames(
    labels: [Label], results: [FrameResult], detector: String, cls: ObjectClass, threshold: Float
) -> Score {
    let byFrame = Dictionary(results.map { ($0.frame, $0) }, uniquingKeysWith: { a, _ in a })
    var s = Score()
    for label in labels {
        guard let r = byFrame[label.frame] else { continue }
        let truth = label.present.contains(cls)
        let predicted = fires(r, detector: detector, cls: cls, threshold: threshold)
        switch (truth, predicted) {
        case (true, true): s.tp += 1
        case (false, true): s.fp += 1
        case (true, false): s.fn += 1
        case (false, false): s.tn += 1
        }
    }
    return s
}

/// A span of stream time in which a class is present.
public struct Span: Codable, Sendable, Equatable {
    public var start: Double
    public var end: Double

    public init(start: Double, end: Double) {
        self.start = start
        self.end = end
    }
}

/// Merges times at which something is present into spans: points closer than
/// `gap` seconds join one span; each span is widened by `pad` on both sides.
public func spans(_ times: [Double], gap: Double, pad: Double) -> [Span] {
    var out: [Span] = []
    for t in times.sorted() {
        if let last = out.last, t - (last.end - pad) <= gap {
            out[out.count - 1].end = t + pad
        } else {
            out.append(Span(start: t - pad, end: t + pad))
        }
    }
    return out
}

public struct EventScore: Codable, Sendable {
    /// Ground-truth events.
    public var events: Int
    /// Events with at least one firing sampled picture inside them.
    public var detected: Int
    /// Detection episodes (merged like events) that overlap no event.
    public var falseEpisodes: Int
    /// Seconds from an event's start to its first firing picture, for the
    /// detected events.
    public var latencySeconds: [Double]
}

/// Event-level recall: does a detection fire during each real event?
public func scoreEvents(
    truth: [Span], results: [FrameResult], detector: String, cls: ObjectClass, threshold: Float,
    gap: Double, pad: Double
) -> EventScore {
    let firing = results.filter { fires($0, detector: detector, cls: cls, threshold: threshold) }.map(\.time)
    var detected = 0
    var latencies: [Double] = []
    for event in truth {
        if let first = firing.filter({ $0 >= event.start && $0 <= event.end }).min() {
            detected += 1
            latencies.append(max(0, first - event.start))
        }
    }
    let episodes = spans(firing, gap: gap, pad: 0)
    let falseEpisodes = episodes.filter { e in
        !truth.contains { $0.start <= e.end + pad && e.start - pad <= $0.end }
    }.count
    return EventScore(events: truth.count, detected: detected, falseEpisodes: falseEpisodes, latencySeconds: latencies)
}
