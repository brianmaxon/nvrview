import CoreGraphics
import Foundation

// The follow cut and the 2-up and 4-up grids as the app plays them
// (docs/UX.md "Recordings", "Follow and Grid"): the core's edit list
// (FFI-RECORDINGS.md section 7, `nv_edit_compute`) parsed, and the pure rules
// that turn it into what is on screen at a time: which source, at which file
// position, in which rectangle; where the cuts are; whose sound plays; and a
// clock the transport drives. The renderer (`EditSession`) and export (M4)
// both read these, so the picture on screen and in a file is the same edit.

/// An edit list from `nv_edit_compute`: a follow cut (`segments`) or a grid
/// (`cells`). Times are device-local milliseconds.
public struct EditPlan: Sendable, Equatable, Decodable {
    public enum Kind: String, Sendable, Decodable {
        case follow
        case grid
    }

    /// One camera's finished file.
    public struct Source: Sendable, Equatable, Decodable {
        public var source: Int
        public var camera: RecordingCamera
        public var name: String
        public var file: String
        /// The recorded time of the file's first picture.
        public var fileStartMs: Int64
        /// The user's correction for a clock that disagrees.
        public var offsetMs: Int64
        public var codec: String
        public var width: Int
        public var height: Int

        public init(
            source: Int, camera: RecordingCamera, name: String, file: String, fileStartMs: Int64, offsetMs: Int64 = 0,
            codec: String = "h264", width: Int, height: Int
        ) {
            self.source = source
            self.camera = camera
            self.name = name
            self.file = file
            self.fileStartMs = fileStartMs
            self.offsetMs = offsetMs
            self.codec = codec
            self.width = width
            self.height = height
        }

        private enum CodingKeys: String, CodingKey {
            case source, camera, name, file, codec, width, height
            case fileStartMs = "file_start_ms"
            case offsetMs = "offset_ms"
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            source = try c.decode(Int.self, forKey: .source)
            camera = try c.decode(RecordingCamera.self, forKey: .camera)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            file = try c.decode(String.self, forKey: .file)
            fileStartMs = try c.decode(Int64.self, forKey: .fileStartMs)
            offsetMs = try c.decodeIfPresent(Int64.self, forKey: .offsetMs) ?? 0
            codec = try c.decodeIfPresent(String.self, forKey: .codec) ?? ""
            width = try c.decodeIfPresent(Int.self, forKey: .width) ?? 0
            height = try c.decodeIfPresent(Int.self, forKey: .height) ?? 0
        }
    }

    /// A shot of the follow cut: `source` from `startMs` to `endMs`, starting
    /// `sourceInMs` into its file.
    public struct Segment: Sendable, Equatable, Decodable {
        public var startMs: Int64
        public var endMs: Int64
        public var source: Int
        public var sourceInMs: Int64

        public init(startMs: Int64, endMs: Int64, source: Int, sourceInMs: Int64) {
            self.startMs = startMs
            self.endMs = endMs
            self.source = source
            self.sourceInMs = sourceInMs
        }

        private enum CodingKeys: String, CodingKey {
            case source
            case startMs = "start_ms"
            case endMs = "end_ms"
            case sourceInMs = "source_in_ms"
        }
    }

    /// A rectangle of the canvas in pixels, top-left origin.
    public struct Rect: Sendable, Equatable, Decodable {
        public var x: Int
        public var y: Int
        public var width: Int
        public var height: Int

        public init(x: Int, y: Int, width: Int, height: Int) {
            self.x = x
            self.y = y
            self.width = width
            self.height = height
        }

        public var cgRect: CGRect { CGRect(x: x, y: y, width: width, height: height) }
    }

    public struct Canvas: Sendable, Equatable, Decodable {
        public var width: Int
        public var height: Int

        public init(width: Int, height: Int) {
            self.width = width
            self.height = height
        }
    }

    /// A grid cell: `source` in `rect` for the whole range, black outside
    /// `present`.
    public struct Cell: Sendable, Equatable, Decodable {
        public var rect: Rect
        public var source: Int
        /// `fit` (letterboxed) or `fill` (cropped).
        public var fit: String
        /// `[start, end]` pairs where the source has video.
        public var present: [[Int64]]

        public init(rect: Rect, source: Int, fit: String = "fit", present: [[Int64]]) {
            self.rect = rect
            self.source = source
            self.fit = fit
            self.present = present
        }

        private enum CodingKeys: String, CodingKey { case rect, source, fit, present }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            rect = try c.decode(Rect.self, forKey: .rect)
            source = try c.decode(Int.self, forKey: .source)
            fit = try c.decodeIfPresent(String.self, forKey: .fit) ?? "fit"
            present = try c.decodeIfPresent([[Int64]].self, forKey: .present) ?? []
        }
    }

    public struct Label: Sendable, Equatable, Decodable {
        public var source: Int
        public var name: String?
        public var time: Bool

        private enum CodingKeys: String, CodingKey { case source, name, time }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            source = try c.decode(Int.self, forKey: .source)
            name = try c.decodeIfPresent(String.self, forKey: .name)
            time = try c.decodeIfPresent(Bool.self, forKey: .time) ?? false
        }
    }

    public var kind: Kind
    public var download: String
    public var startMs: Int64
    public var endMs: Int64
    public var suggestedFile: String
    public var copyPossible: Bool
    public var sources: [Source]
    /// Follow only.
    public var segments: [Segment]
    public var labels: [Label]
    /// Grid only.
    public var layout: String?
    public var canvas: Canvas?
    public var cells: [Cell]

    public init(
        kind: Kind, download: String = "", startMs: Int64, endMs: Int64, sources: [Source], segments: [Segment] = [],
        layout: String? = nil, canvas: Canvas? = nil, cells: [Cell] = []
    ) {
        self.kind = kind
        self.download = download
        self.startMs = startMs
        self.endMs = endMs
        suggestedFile = ""
        copyPossible = false
        self.sources = sources
        self.segments = segments
        labels = []
        self.layout = layout
        self.canvas = canvas
        self.cells = cells
    }

    private enum CodingKeys: String, CodingKey {
        case kind, download, sources, segments, labels, layout, canvas, cells
        case startMs = "start_ms"
        case endMs = "end_ms"
        case suggestedFile = "suggested_file"
        case copyPossible = "copy_possible"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = try c.decode(Kind.self, forKey: .kind)
        download = try c.decodeIfPresent(String.self, forKey: .download) ?? ""
        startMs = try c.decode(Int64.self, forKey: .startMs)
        endMs = try c.decode(Int64.self, forKey: .endMs)
        suggestedFile = try c.decodeIfPresent(String.self, forKey: .suggestedFile) ?? ""
        copyPossible = try c.decodeIfPresent(Bool.self, forKey: .copyPossible) ?? false
        sources = try c.decode([Source].self, forKey: .sources)
        segments = try c.decodeIfPresent([Segment].self, forKey: .segments) ?? []
        labels = try c.decodeIfPresent([Label].self, forKey: .labels) ?? []
        layout = try c.decodeIfPresent(String.self, forKey: .layout)
        canvas = try c.decodeIfPresent(Canvas.self, forKey: .canvas)
        cells = try c.decodeIfPresent([Cell].self, forKey: .cells) ?? []
    }

    /// The plan from `nv_edit_compute`'s answer; nil when it is not one, or
    /// names a source it does not list.
    public static func parse(_ json: String) -> EditPlan? {
        guard let plan = try? JSONDecoder().decode(EditPlan.self, from: Data(json.utf8)), plan.endMs >= plan.startMs
        else { return nil }
        let known = Set(plan.sources.map(\.source))
        guard plan.segments.allSatisfy({ known.contains($0.source) }), plan.cells.allSatisfy({ known.contains($0.source) })
        else { return nil }
        return plan
    }

    /// The source with this number.
    public func source(_ n: Int) -> Source? { sources.first { $0.source == n } }
}

/// What a plan shows at a time.
public enum EditTimeline {
    /// The follow cut's shot at `t`.
    public struct Shot: Sendable, Equatable {
        public var segment: Int
        public var source: Int
        /// Where in the source's file (milliseconds from its first picture).
        public var filePositionMs: Int64

        public init(segment: Int, source: Int, filePositionMs: Int64) {
            self.segment = segment
            self.source = source
            self.filePositionMs = filePositionMs
        }
    }

    /// The follow cut's shot at `t`: the segment with `start <= t < end` (the
    /// plan's last instant shows the last segment's last picture); nil in a
    /// gap or outside the plan (the picture is black).
    public static func shot(_ plan: EditPlan, at t: Int64) -> Shot? {
        guard plan.kind == .follow else { return nil }
        for (i, s) in plan.segments.enumerated() {
            let last = i == plan.segments.count - 1
            if s.startMs <= t, t < s.endMs || (last && t == s.endMs) {
                return Shot(segment: i, source: s.source, filePositionMs: s.sourceInMs + (t - s.startMs))
            }
        }
        return nil
    }

    /// Where the follow cut changes the picture: the start of every segment
    /// after the first whose source differs from the one before it, or that
    /// follows a gap.
    public static func cuts(_ plan: EditPlan) -> [Int64] {
        guard plan.kind == .follow else { return [] }
        var out: [Int64] = []
        for (prev, s) in zip(plan.segments, plan.segments.dropFirst()) where s.source != prev.source || s.startMs != prev.endMs {
            out.append(s.startMs)
        }
        return out
    }

    /// A source's file position at time `t` (its own clock with the user's
    /// offset applied).
    public static func filePosition(_ source: EditPlan.Source, at t: Int64) -> Int64 {
        t - source.fileStartMs - source.offsetMs
    }

    /// Whether a grid cell has video at `t` (its `present` spans, each
    /// including its start and excluding its end, except at the plan's end).
    public static func present(_ cell: EditPlan.Cell, at t: Int64, planEnd: Int64) -> Bool {
        cell.present.contains { span in
            span.count == 2 && span[0] <= t && (t < span[1] || (t == span[1] && t == planEnd))
        }
    }

    /// The grid cell's file position at `t`, nil where the cell is black.
    public static func cellPosition(_ plan: EditPlan, cell i: Int, at t: Int64) -> Int64? {
        guard plan.cells.indices.contains(i), let source = plan.source(plan.cells[i].source),
            present(plan.cells[i], at: t, planEnd: plan.endMs)
        else { return nil }
        return filePosition(source, at: t)
    }

    /// The source whose sound plays at `t`: in the follow cut, the shown
    /// camera's (the sound cuts with the picture); in a grid, the chosen
    /// cell's (the first unless the user picked another) while it has video.
    /// Nothing when muted or at a speed without sound (`PlaybackSpeed`).
    public static func audibleSource(
        _ plan: EditPlan, at t: Int64, rate: Double, muted: Bool, gridCell: Int = 0
    ) -> Int? {
        guard !muted, PlaybackSpeed.hasSound(rate) else { return nil }
        switch plan.kind {
        case .follow: return shot(plan, at: t)?.source
        case .grid:
            guard cellPosition(plan, cell: gridCell, at: t) != nil else { return nil }
            return plan.cells[gridCell].source
        }
    }

    /// The source a follow cut shows next after `t`, with the time it starts
    /// and where in its file, so the renderer can have its picture ready at
    /// the cut. Nil after the last cut.
    public static func nextShot(_ plan: EditPlan, after t: Int64) -> (at: Int64, shot: Shot)? {
        guard plan.kind == .follow, let current = shot(plan, at: t) else {
            guard let i = plan.segments.firstIndex(where: { $0.startMs > t }) else { return nil }
            let s = plan.segments[i]
            return (s.startMs, Shot(segment: i, source: s.source, filePositionMs: s.sourceInMs))
        }
        var i = current.segment + 1
        while i < plan.segments.count {
            let s = plan.segments[i]
            if s.source != current.source { return (s.startMs, Shot(segment: i, source: s.source, filePositionMs: s.sourceInMs)) }
            i += 1
        }
        return nil
    }
}

/// One picture of an edit placed in a view or an output frame.
public struct EditLayer: Sendable, Equatable {
    /// The plan's source number.
    public var source: Int
    /// Where in its file.
    public var filePositionMs: Int64
    /// Where it goes (top-left origin, in the units of the bounds given).
    public var rect: CGRect
    /// Cropped to fill `rect` rather than letterboxed in it.
    public var fill: Bool
    /// For a grid, the cell's index.
    public var cell: Int?

    public init(source: Int, filePositionMs: Int64, rect: CGRect, fill: Bool, cell: Int? = nil) {
        self.source = source
        self.filePositionMs = filePositionMs
        self.rect = rect
        self.fill = fill
        self.cell = cell
    }
}

/// How an edit fills a frame. The follow cut's picture fills the bounds
/// (letterboxed by the renderer); a grid's canvas is letterboxed into the
/// bounds and its cells scaled with it, so the screen shows what an export
/// of the same canvas would.
public enum EditComposition {
    /// The canvas's place in `bounds` (the whole bounds for a follow cut).
    public static func frame(_ plan: EditPlan, in bounds: CGRect) -> CGRect {
        guard plan.kind == .grid, let c = plan.canvas, c.width > 0, c.height > 0 else { return bounds }
        return PictureFit.fit(picture: CGSize(width: c.width, height: c.height), in: bounds, fill: false).destination
    }

    /// Every cell's rectangle in `bounds`, shown or not (a grid; empty for a
    /// follow cut).
    public static func cellRects(_ plan: EditPlan, in bounds: CGRect) -> [CGRect] {
        guard plan.kind == .grid, let c = plan.canvas, c.width > 0, c.height > 0 else { return [] }
        let f = frame(plan, in: bounds)
        let sx = f.width / CGFloat(c.width), sy = f.height / CGFloat(c.height)
        return plan.cells.map { cell in
            let r = cell.rect.cgRect
            return CGRect(x: f.minX + r.minX * sx, y: f.minY + r.minY * sy, width: r.width * sx, height: r.height * sy)
        }
    }

    /// The pictures shown at `t`: the follow cut's one shot, or each grid
    /// cell with video.
    public static func layers(_ plan: EditPlan, at t: Int64, in bounds: CGRect) -> [EditLayer] {
        switch plan.kind {
        case .follow:
            guard let shot = EditTimeline.shot(plan, at: t) else { return [] }
            return [EditLayer(source: shot.source, filePositionMs: shot.filePositionMs, rect: bounds, fill: false)]
        case .grid:
            let rects = cellRects(plan, in: bounds)
            return plan.cells.indices.compactMap { i in
                guard let p = EditTimeline.cellPosition(plan, cell: i, at: t) else { return nil }
                return EditLayer(source: plan.cells[i].source, filePositionMs: p, rect: rects[i], fill: plan.cells[i].fit == "fill", cell: i)
            }
        }
    }
}

/// The edit's own clock: the transport (play, pause, speed, seek) over the
/// plan's range, read against a host time the caller supplies (seconds), so
/// tests and the self-test drive it with a simulated clock. It stops at the
/// end.
public struct EditClock: Sendable, Equatable {
    public private(set) var startMs: Int64
    public private(set) var endMs: Int64
    public private(set) var rate: Double = 1
    public private(set) var playing = false
    /// The position at `anchorHost`.
    private var anchorMs: Int64
    private var anchorHost: Double = 0

    public init(startMs: Int64, endMs: Int64, positionMs: Int64? = nil) {
        self.startMs = startMs
        self.endMs = max(endMs, startMs)
        anchorMs = min(max(positionMs ?? startMs, startMs), max(endMs, startMs))
    }

    /// The position at host time `now`.
    public func position(at now: Double) -> Int64 {
        guard playing else { return anchorMs }
        let moved = Int64(((now - anchorHost) * 1000 * rate).rounded(.down))
        return min(max(anchorMs + max(moved, 0), startMs), endMs)
    }

    /// Whether the clock reached the end.
    public func ended(at now: Double) -> Bool { position(at: now) >= endMs }

    /// The transport's state at `now`.
    public func state(at now: Double) -> PlaybackState {
        if ended(at: now) { return .ended }
        return playing ? .playing : .paused
    }

    /// Plays from where it is (from the start again at the end).
    public mutating func play(at now: Double) {
        if ended(at: now) { anchorMs = startMs } else { anchorMs = position(at: now) }
        anchorHost = now
        playing = true
    }

    public mutating func pause(at now: Double) {
        anchorMs = position(at: now)
        anchorHost = now
        playing = false
    }

    public mutating func seek(to ms: Int64, at now: Double) {
        anchorMs = min(max(ms, startMs), endMs)
        anchorHost = now
    }

    public mutating func setRate(_ r: Double, at now: Double) {
        guard r > 0 else { return }
        anchorMs = position(at: now)
        anchorHost = now
        rate = r
    }

    public func transport(at now: Double) -> TransportState {
        TransportState(startMs: startMs, endMs: endMs, positionMs: position(at: now), state: state(at: now), rate: rate)
    }
}
