import Foundation

// The Recordings window's Export tab, the decisions (docs/UX.md "Recordings",
// "Export tab"): which kinds of export apply to what is open, the default,
// which options show for a kind, the summary line, and what stops an export
// before it starts. Pure, so both apps share the vectors
// (`fixtures/synthetic/export-panel.json`); the same type as the Windows
// app's ExportPanel (NvrView.Shell), with the same tests. The panel draws
// these answers and runs the commands the Export model already has.

/// What the Export button writes.
public enum ExportKind: String, CaseIterable, Sendable {
    /// The range cut from the keyframe before it (a copy, instant).
    case trimCopy = "trim-copy"
    /// The range from the exact picture (re-encoded).
    case trimExact = "trim-exact"
    case followCut = "follow-cut"
    case grid2 = "grid-2"
    case grid4 = "grid-4"
    /// The device's files for the range, as they are.
    case download
    case stillCurrent = "still-current"
    case stillEach = "still-each"

    public var title: String {
        switch self {
        case .trimCopy: "Trim (Keyframe Copy)"
        case .trimExact: "Trim (Exact)"
        case .followCut: "Follow Cut"
        case .grid2: "2-Up"
        case .grid4: "4-Up"
        case .download: "Download Only"
        case .stillCurrent: "Current Picture"
        case .stillEach: "Picture of Each Camera"
        }
    }

    /// The Export button's title for this kind.
    public var buttonTitle: String {
        switch self {
        case .trimCopy, .trimExact: "Trim"
        case .followCut, .grid2, .grid4: "Export"
        case .download: "Download"
        case .stillCurrent: "Save Picture"
        case .stillEach: "Save Pictures"
        }
    }

    public var isTrim: Bool { self == .trimCopy || self == .trimExact }
    public var isEdit: Bool { self == .followCut || self == .grid2 || self == .grid4 }
    public var isStill: Bool { self == .stillCurrent || self == .stillEach }
}

/// Which stream a download (or the download an export makes first) reads.
public enum ExportQuality: String, CaseIterable, Sendable {
    case main
    case sub

    /// The request's `tier`.
    public var tier: String { rawValue }

    public var title: String { self == .main ? "Main Stream" : "Sub-stream" }

    public var detail: String {
        self == .main ? "Full quality; the files are larger" : "Smaller files, lower quality"
    }
}

/// What is open when the panel decides.
public struct ExportContext: Sendable, Equatable {
    /// An incident or a camera is playing, so there is a range to export.
    public var hasRange: Bool
    /// A device playback or the follow cut or grid is open, so there are pictures.
    public var hasPictures: Bool
    /// The incident is downloaded, so a follow cut and grids can be made of its files.
    public var editable: Bool
    /// The app can write a follow cut or grid.
    public var canWriteEdit: Bool
    /// The cameras the range or the incident has.
    public var sourceCameras: Int
    /// The cameras checked in the list.
    public var checkedCameras: Int

    public init(
        hasRange: Bool, hasPictures: Bool, editable: Bool, canWriteEdit: Bool = true, sourceCameras: Int, checkedCameras: Int
    ) {
        self.hasRange = hasRange
        self.hasPictures = hasPictures
        self.editable = editable
        self.canWriteEdit = canWriteEdit
        self.sourceCameras = sourceCameras
        self.checkedCameras = checkedCameras
    }
}

public enum ExportPanelRules {
    /// The shortest range an export may cover (the points' own minimum).
    public static let minimumMs = InOutPoints.minimumMs

    /// The kinds that apply, in the order the panel lists them; the others are hidden.
    public static func kinds(_ c: ExportContext) -> [ExportKind] {
        var list: [ExportKind] = []
        if c.hasRange && c.sourceCameras >= 1 {
            list += [.trimCopy, .trimExact]
        }
        if c.editable && c.canWriteEdit {
            list.append(.followCut)
            if c.sourceCameras >= 2 { list += [.grid2, .grid4] }
        }
        if c.hasRange && c.sourceCameras >= 1 { list.append(.download) }
        if c.hasPictures {
            list.append(.stillCurrent)
            if c.sourceCameras >= 2 { list.append(.stillEach) }
        }
        return list
    }

    /// The kind to show: the one chosen before when it still applies, else the first that does.
    public static func defaultKind(_ available: [ExportKind], keeping current: ExportKind?) -> ExportKind? {
        if let current, available.contains(current) { return current }
        return available.first
    }

    /// Whether the camera checklist applies (the follow cut and grids use the incident's files, and the current picture is one).
    public static func usesCameras(_ kind: ExportKind) -> Bool {
        kind.isTrim || kind == .download || kind == .stillEach
    }

    /// Whether the sound switch applies.
    public static func usesSound(_ kind: ExportKind) -> Bool { kind.isEdit }

    /// Whether the stream choice applies: a download, and a trim that has to download its range first.
    public static func usesQuality(_ kind: ExportKind, editable: Bool) -> Bool {
        kind == .download || (kind.isTrim && !editable)
    }

    /// The camera list's default: the incident's cameras (a camera not in `available` is dropped from `chosen`).
    public static func checked<C: Hashable>(chosen: Set<C>?, available: [C]) -> Set<C> {
        guard let chosen else { return Set(available) }
        return chosen.intersection(available)
    }

    /// Why an export cannot start, or nil. `start` and `end` are the range in milliseconds.
    public static func problem(kind: ExportKind, start: Int64, end: Int64, checkedCameras: Int, busy: Bool) -> String? {
        if busy { return "Wait for the export in progress, or cancel it" }
        if usesCameras(kind) && checkedCameras < 1 { return "Choose at least one camera" }
        if !kind.isStill && end - start < minimumMs { return "Set an in point and an out point first (I and O)" }
        return nil
    }

    /// "1:05" or "1:02:05" for a duration.
    public static func duration(ms: Int64) -> String {
        let s = max(0, ms) / 1000
        let h = s / 3600, m = s % 3600 / 60, sec = s % 60
        return h > 0 ? String(format: "%lld:%02lld:%02lld", h, m, sec) : String(format: "%lld:%02lld", m, sec)
    }

    /// "about 25 MB" for a byte count (decimal units, like the Downloads list).
    public static func size(bytes: Int64) -> String {
        let b = Double(bytes)
        if b >= 1e9 { return String(format: "about %.1f GB", b / 1e9) }
        if b >= 1e6 { return String(format: "about %.0f MB", b / 1e6) }
        return String(format: "about %.0f KB", max(1, b / 1e3))
    }

    /// The live line under "What": the cameras, the duration, and the size when the core can tell.
    public static func summary(kind: ExportKind, durationMs: Int64, checkedCameras: Int, estimatedBytes: Int64?) -> String {
        func cameras(_ n: Int) -> String { n == 1 ? "1 camera" : "\(n) cameras" }
        switch kind {
        case .stillCurrent: return "1 picture"
        case .stillEach: return checkedCameras == 1 ? "1 picture" : "\(checkedCameras) pictures"
        case .followCut, .grid2, .grid4: return "One video, \(duration(ms: durationMs))"
        case .trimCopy, .trimExact, .download:
            var s = "\(cameras(checkedCameras)), \(duration(ms: durationMs))"
            if let estimatedBytes, estimatedBytes > 0 { s += ", \(size(bytes: estimatedBytes))" }
            return s
        }
    }

    /// The bytes of the spans' parts inside `start...end` (device-local seconds), each span's bytes shared evenly over its length.
    public static func estimate(spans: [(start: Int64, end: Int64, bytes: Int64)], start: Int64, end: Int64) -> Int64? {
        var total = 0.0
        var known = false
        for s in spans where s.bytes > 0 && s.end > s.start {
            let overlap = min(s.end, end) - max(s.start, start)
            guard overlap > 0 else { continue }
            known = true
            total += Double(s.bytes) * Double(overlap) / Double(s.end - s.start)
        }
        return known ? Int64(total.rounded()) : nil
    }

    // MARK: Times

    private static let dayMs: Int64 = 86_400_000

    /// "07:42:05", with ".250" when the milliseconds are not zero, for a device-local time.
    public static func timeText(ms: Int64) -> String {
        let t = ((ms % dayMs) + dayMs) % dayMs
        let h = t / 3_600_000, m = t / 60_000 % 60, s = t / 1000 % 60, f = t % 1000
        return f == 0 ? String(format: "%02lld:%02lld:%02lld", h, m, s) : String(format: "%02lld:%02lld:%02lld.%03lld", h, m, s, f)
    }

    /// The device-local time that `text` ("7:42", "07:42:05", "07:42:05.25") names on the day starting at `dayStartMs`; nil when it is not a time.
    public static func parseTime(_ text: String, dayStartMs: Int64) -> Int64? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":", omittingEmptySubsequences: false)
        guard (2...3).contains(parts.count) else { return nil }
        var seconds = 0.0
        if parts.count == 3 {
            guard let sec = Double(parts[2]), sec >= 0, sec < 60, parts[2].allSatisfy({ $0.isNumber || $0 == "." }) else { return nil }
            seconds = sec
        }
        guard let h = Int(parts[0]), let m = Int(parts[1]), (0..<24).contains(h), (0..<60).contains(m),
            parts[0].allSatisfy(\.isNumber), parts[1].allSatisfy(\.isNumber)
        else { return nil }
        return dayStartMs + Int64(h) * 3_600_000 + Int64(m) * 60_000 + Int64((seconds * 1000).rounded())
    }
}
