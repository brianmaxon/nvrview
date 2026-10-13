import CoreGraphics
import Foundation

// The kept verification pictures taken out of the Events window (docs/UX.md
// "Events"): file names, the temporary files Open in Preview and a drag
// leave behind, the evidence icon of a list row, the remembered splits, and
// the large viewer's keys. Pure; the app does the drawing and the files.

/// File names for pictures made from a kept frame.
public enum EvidencePictureFiles {
    /// How long a temporary file stays.
    public static let maxAge: TimeInterval = 24 * 3600

    /// The name of a picture of a kept frame: the Save Picture rule
    /// (`YYYY-MM-DD HH.MM.SS.mmm <camera>.png`, `Camera A` when share-safe,
    /// ` (2)` when taken), stamped with the time the frame was taken.
    public static func fileName(
        camera: String, frameMs: Int64, shareSafe: Bool, format: PictureFormat, taken: (String) -> Bool
    ) -> String? {
        let date = Date(timeIntervalSince1970: Double(frameMs) / 1000)
        return PictureNaming.fileName(
            localMillis: PictureNaming.localMillis(date), camera: camera, shareSafe: shareSafe, format: format,
            taken: taken)
    }

    /// The entries at least `maxAge` old at `now`.
    public static func expired(_ files: [(url: URL, modified: Date)], now: Date, maxAge: TimeInterval = maxAge) -> [URL]
    {
        files.filter { now.timeIntervalSince($0.modified) >= maxAge }.map(\.url)
    }

    /// Removes what is at least `maxAge` old directly inside `folder` (files
    /// and the per-drag folders). Returns how many were removed.
    @discardableResult
    public static func removeOld(
        in folder: URL, now: Date, maxAge: TimeInterval = maxAge, manager: FileManager = .default
    ) -> Int {
        guard
            let urls = try? manager.contentsOfDirectory(
                at: folder, includingPropertiesForKeys: [.contentModificationDateKey], options: [])
        else { return 0 }
        let files = urls.map { url -> (url: URL, modified: Date) in
            let date = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            return (url, date ?? .distantPast)
        }
        var removed = 0
        for url in expired(files, now: now, maxAge: maxAge) where (try? manager.removeItem(at: url)) != nil {
            removed += 1
        }
        return removed
    }
}

/// The small picture icon of a list row.
public enum EvidenceBadge {
    public struct Info: Equatable, Sendable {
        public var frames: Int
        public var pinned: Bool
        /// "3 pictures, kept (7 days left)" / "1 picture, deleted in 1 h 20 min".
        public var tooltip: String
    }

    /// Nil when nothing is kept: no frames, or the set has expired.
    public static func info(_ e: AlertEvidence, nowMs: Int64) -> Info? {
        guard !e.frames.isEmpty else { return nil }
        let n = e.frames.count
        let count = "\(n) \(n == 1 ? "picture" : "pictures")"
        let left = e.expiresMs.map { $0 - nowMs }
        if let left, left <= 0 { return nil }
        let text: String
        if e.pinned {
            text = "\(count), kept" + (left.map { " (\(remaining($0)) left)" } ?? "")
        } else {
            text = "\(count), deleted" + (left.map { " in \(remaining($0))" } ?? " soon")
        }
        return Info(frames: n, pinned: e.pinned, tooltip: text)
    }

    /// "6 days", "1 h 20 min", "5 min" (at least 1 min).
    public static func remaining(_ ms: Int64) -> String {
        let minutes = max(Int((ms + 59_999) / 60_000), 1)
        if minutes >= 2 * 1440 {
            return "\((minutes + 1439) / 1440) days"
        }
        if minutes >= 1440 { return "1 day" }
        if minutes >= 60 {
            let h = minutes / 60, m = minutes % 60
            return m == 0 ? "\(h) h" : "\(h) h \(m) min"
        }
        return "\(minutes) min"
    }
}

/// A divider position kept as a fraction of the space (remembered across
/// launches by the app).
public enum SplitLayout {
    /// `fraction` of `total` for the first pane, kept so both panes keep
    /// their minimum size (the first wins when they cannot both fit).
    public static func clamped(fraction: Double, total: CGFloat, minFirst: CGFloat, minSecond: CGFloat) -> Double {
        guard total > 0, fraction.isFinite else { return 0.5 }
        let lo = Double(min(minFirst, total) / total)
        let hi = max(Double(1 - min(minSecond, total) / total), lo)
        return min(max(fraction, lo), hi)
    }

    /// The fraction after the divider moved to `position` points from the start.
    public static func fraction(at position: CGFloat, total: CGFloat) -> Double {
        total > 0 ? Double(position / total) : 0.5
    }
}

/// What a key does in the large picture viewer.
public enum EvidenceViewerKey: Equatable, Sendable {
    case previous, next, close

    /// 123 left arrow, 124 right arrow, 53 Escape.
    public init?(keyCode: UInt16) {
        switch keyCode {
        case 123: self = .previous
        case 124: self = .next
        case 53: self = .close
        default: return nil
        }
    }
}

/// Stepping through a set's frames.
public enum EvidenceFrameStep {
    /// The frame index `delta` places from `current` among `indices`, stopping
    /// at the first and last; the first when `current` is not in the list.
    public static func step(from current: Int, by delta: Int, in indices: [Int]) -> Int {
        let sorted = indices.sorted()
        guard let at = sorted.firstIndex(of: current) else { return sorted.first ?? current }
        return sorted[min(max(at + delta, 0), sorted.count - 1)]
    }
}
