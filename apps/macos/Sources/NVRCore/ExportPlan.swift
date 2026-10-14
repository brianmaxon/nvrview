import CoreGraphics
import Foundation

// Export, the pure part (docs/UX.md "Recordings", "Export"; the core's side is
// FFI-RECORDINGS.md section 7a): the in and out points the user sets, the
// requests sent to the core, the plans that come back (a trim, still images),
// and the rules for an exported follow cut or grid (output size, frame times,
// whose sound is written when). The writers (`EditExporter`, `TrimExporter`,
// `StillExporter`) only carry these out.

/// The in and out points on the scrubber: device-local milliseconds, either
/// or both unset. An unset point means the start or the end of the range
/// the scrubber spans.
public struct InOutPoints: Sendable, Equatable {
    public var inMs: Int64?
    public var outMs: Int64?

    /// The shortest span an export may cover.
    public static let minimumMs: Int64 = 100

    public init(inMs: Int64? = nil, outMs: Int64? = nil) {
        self.inMs = inMs
        self.outMs = outMs
    }

    public var isEmpty: Bool { inMs == nil && outMs == nil }

    /// Sets the in point (kept inside `range`). An in point at or after the
    /// out point clears the out point, so the points are never crossed.
    public mutating func setIn(_ ms: Int64, within range: ClosedRange<Int64>) {
        let at = min(max(ms, range.lowerBound), range.upperBound)
        inMs = at
        if let o = outMs, o < at + Self.minimumMs { outMs = nil }
    }

    /// Sets the out point; an out point at or before the in point clears the in point.
    public mutating func setOut(_ ms: Int64, within range: ClosedRange<Int64>) {
        let at = min(max(ms, range.lowerBound), range.upperBound)
        outMs = at
        if let i = inMs, i > at - Self.minimumMs { inMs = nil }
    }

    public mutating func clear() {
        inMs = nil
        outMs = nil
    }

    /// The points that lie inside `range`, for a range that changed (another
    /// incident, another view): a point outside it is dropped.
    public func kept(within range: ClosedRange<Int64>) -> InOutPoints {
        var p = self
        if let i = p.inMs, !range.contains(i) { p.inMs = nil }
        if let o = p.outMs, !range.contains(o) { p.outMs = nil }
        if let i = p.inMs, let o = p.outMs, o < i + Self.minimumMs { p.outMs = nil }
        return p
    }

    /// The span to export in `range`: the points, with the range's ends for
    /// an unset one; nil when it would be shorter than `minimumMs`.
    public func window(in range: ClosedRange<Int64>) -> (start: Int64, end: Int64)? {
        let start = min(max(inMs ?? range.lowerBound, range.lowerBound), range.upperBound)
        let end = min(max(outMs ?? range.upperBound, range.lowerBound), range.upperBound)
        return end - start >= Self.minimumMs ? (start, end) : nil
    }

    /// Which point a press at `fraction` (0...1 across the range) grabs:
    /// the nearer one within `tolerance`, nil when none is set near it.
    public enum Handle: Sendable, Equatable { case `in`, out }

    public func handle(atFraction f: Double, range: ClosedRange<Int64>, tolerance: Double) -> Handle? {
        let span = Double(range.upperBound - range.lowerBound)
        guard span > 0 else { return nil }
        func distance(_ ms: Int64?) -> Double? { ms.map { abs(Double($0 - range.lowerBound) / span - f) } }
        let di = distance(inMs), dout = distance(outMs)
        switch (di, dout) {
        case (let a?, let b?): return min(a, b) <= tolerance ? (a <= b ? .in : .out) : nil
        case (let a?, nil): return a <= tolerance ? .in : nil
        case (nil, let b?): return b <= tolerance ? .out : nil
        default: return nil
        }
    }

    /// Moves a grabbed point to `ms`.
    public mutating func move(_ handle: Handle, to ms: Int64, within range: ClosedRange<Int64>) {
        switch handle {
        case .in:
            let limit = (outMs ?? range.upperBound) - Self.minimumMs
            inMs = min(max(ms, range.lowerBound), max(limit, range.lowerBound))
        case .out:
            let limit = (inMs ?? range.lowerBound) + Self.minimumMs
            outMs = max(min(ms, range.upperBound), min(limit, range.upperBound))
        }
    }

    /// "07:42:05.250 to 07:42:11.000", with "start" or "end" for an unset point.
    public func text(clock: (Int64) -> String) -> String {
        "\(inMs.map(clock) ?? "start") to \(outMs.map(clock) ?? "end")"
    }
}

/// Everything an export request can carry besides its subject.
public struct ExportOptions: Sendable, Equatable {
    /// An absolute directory the user chose; nil for the core's default.
    public var dest: String?
    /// No metadata of the app's own and neutral camera names ("Camera A").
    public var shareSafe: Bool

    public init(dest: String? = nil, shareSafe: Bool = false) {
        self.dest = dest
        self.shareSafe = shareSafe
    }

    /// The members `dest` and `share_safe` (each only when set), as JSON
    /// object members with a leading comma.
    var members: String {
        var s = ""
        if let dest { s += #","dest":\#(RecordingsRequest.string(dest))"# }
        if shareSafe { s += #","share_safe":true"# }
        return s
    }
}

/// The export requests of FFI-RECORDINGS.md section 7a as JSON.
public enum ExportRequest {
    private static func id(_ s: String) -> String { RecordingsRequest.string(s) }

    /// `nv_edit_compute` of kind `trim`: `cameras` nil for every camera of the download.
    public static func trim(
        download: String, cameras: [RecordingCamera]?, inMs: Int64, outMs: Int64, exact: Bool, options: ExportOptions
    ) -> String {
        let list = cameras.map { #","cameras":\#(RecordingsRequest.cameras($0))"# } ?? ""
        return #"{"kind":"trim","download":\#(id(download))\#(list),"in_ms":\#(inMs),"out_ms":\#(outMs),"cut":"\#(exact ? "exact" : "keyframe")"\#(options.members)}"#
    }

    /// `nv_edit_compute` of kind `follow` or `grid`, cut to a window.
    public static func edit(
        grid layout: String?, download: String, window: (start: Int64, end: Int64)?, options: ExportOptions
    ) -> String {
        var s = #"{"kind":"\#(layout == nil ? "follow" : "grid")","download":\#(id(download))"#
        if let layout { s += #","layout":"\#(layout)","canvas":{"width":1920,"height":1080}"# }
        if let window { s += #","in_ms":\#(window.start),"out_ms":\#(window.end)"# }
        return s + options.members + "}"
    }

    /// `nv_download_range`: whole seconds, the start rounded down and the end up.
    public static func range(
        startMs: Int64, endMs: Int64, cameras: [RecordingCamera], name: String?, options: ExportOptions,
        quality: ExportQuality = .main
    ) -> String {
        let start = Int64((Double(startMs) / 1000).rounded(.down)), end = Int64((Double(endMs) / 1000).rounded(.up))
        let list = cameras.map { #"{"camera":\#($0.json),"tier":"\#(quality.tier)"}"# }.joined(separator: ",")
        let n = name.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }.map { #","name":\#(id($0))"# } ?? ""
        return #"{"start":\#(start),"end":\#(end),"cameras":[\#(list)]\#(n)\#(options.members)}"#
    }

    /// `nv_still_plan` for the pictures a playback's tiles show now.
    public static func stills(
        playback: UInt64, tiles: [UInt64]?, format: PictureFormat, options: ExportOptions
    ) -> String {
        let list = tiles.map { #","tiles":[\#($0.map(String.init).joined(separator: ","))]"# } ?? ""
        return #"{"playback":\#(playback)\#(list),"format":"\#(format.rawValue)"\#(options.members)}"#
    }

    /// `nv_still_plan` for one camera's picture at a time in a downloaded file.
    public static func still(
        download: String, camera: RecordingCamera, atMs: Int64, exact: Bool, format: PictureFormat, options: ExportOptions
    ) -> String {
        #"{"download":\#(id(download)),"camera":\#(camera.json),"at_ms":\#(atMs),"exact":\#(exact),"format":"\#(format.rawValue)"\#(options.members)}"#
    }
}

/// A trim plan (`nv_edit_compute` of kind `trim`).
public struct TrimPlan: Sendable, Equatable, Decodable {
    public struct Output: Sendable, Equatable, Decodable {
        public var source: Int
        public var camera: RecordingCamera
        public var name: String
        /// The downloaded MP4.
        public var file: String
        /// Where the output starts and ends in `file`, in milliseconds from its first picture.
        public var sourceInMs: Int64
        public var sourceOutMs: Int64
        /// Whether the samples can be copied without re-encoding.
        public var copyPossible: Bool
        public var suggestedFile: String

        private enum CodingKeys: String, CodingKey {
            case source, camera, name, file
            case sourceInMs = "source_in_ms"
            case sourceOutMs = "source_out_ms"
            case copyPossible = "copy_possible"
            case suggestedFile = "suggested_file"
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            source = try c.decode(Int.self, forKey: .source)
            camera = try c.decode(RecordingCamera.self, forKey: .camera)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            file = try c.decode(String.self, forKey: .file)
            sourceInMs = try c.decode(Int64.self, forKey: .sourceInMs)
            sourceOutMs = try c.decode(Int64.self, forKey: .sourceOutMs)
            copyPossible = try c.decodeIfPresent(Bool.self, forKey: .copyPossible) ?? false
            suggestedFile = try c.decodeIfPresent(String.self, forKey: .suggestedFile) ?? ""
        }
    }

    public var download: String
    public var startMs: Int64
    public var endMs: Int64
    public var outputs: [Output]
    public var shareSafe: Bool

    private enum CodingKeys: String, CodingKey {
        case kind, download, outputs
        case startMs = "start_ms"
        case endMs = "end_ms"
        case shareSafe = "share_safe"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        guard try c.decode(String.self, forKey: .kind) == "trim" else {
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "not a trim")
        }
        download = try c.decode(String.self, forKey: .download)
        startMs = try c.decode(Int64.self, forKey: .startMs)
        endMs = try c.decode(Int64.self, forKey: .endMs)
        outputs = try c.decode([Output].self, forKey: .outputs)
        shareSafe = try c.decodeIfPresent(Bool.self, forKey: .shareSafe) ?? false
    }

    public static func parse(_ json: String) -> TrimPlan? {
        guard let p = try? JSONDecoder().decode(TrimPlan.self, from: Data(json.utf8)), !p.outputs.isEmpty,
            p.outputs.allSatisfy({ $0.sourceOutMs > $0.sourceInMs })
        else { return nil }
        return p
    }
}

/// The answer of `nv_still_plan`.
public struct StillPlan: Sendable, Equatable, Decodable {
    public struct Still: Sendable, Equatable, Decodable {
        /// From a playback: the tile whose picture to save.
        public var tile: UInt64?
        public var camera: RecordingCamera
        public var name: String
        /// From a playback: the picture's generation and recorded time.
        public var generation: UInt32?
        public var positionMs: Int64?
        /// From a file: the MP4 and the position in it to decode at, and from
        /// which keyframe decoding starts (absent when it is the picture itself).
        public var file: String?
        public var sourceInMs: Int64?
        public var suggestedFile: String

        private enum CodingKeys: String, CodingKey {
            case tile, camera, name, generation, file
            case positionMs = "position_ms"
            case sourceInMs = "source_in_ms"
            case suggestedFile = "suggested_file"
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            tile = try c.decodeIfPresent(UInt64.self, forKey: .tile)
            camera = try c.decode(RecordingCamera.self, forKey: .camera)
            name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
            generation = try c.decodeIfPresent(UInt32.self, forKey: .generation)
            positionMs = try c.decodeIfPresent(Int64.self, forKey: .positionMs)
            file = try c.decodeIfPresent(String.self, forKey: .file)
            sourceInMs = try c.decodeIfPresent(Int64.self, forKey: .sourceInMs)
            suggestedFile = try c.decodeIfPresent(String.self, forKey: .suggestedFile) ?? ""
        }
    }

    public var stills: [Still]

    public static func parse(_ json: String) -> StillPlan? {
        guard let p = try? JSONDecoder().decode(StillPlan.self, from: Data(json.utf8)), !p.stills.isEmpty else { return nil }
        return p
    }
}

/// How an exported follow cut or grid is made.
public enum ExportGeometry {
    /// Frames per second of an exported follow cut or grid.
    public static let framesPerSecond = 30
    /// The largest output.
    public static let maxSize = (width: 1920, height: 1080)

    private static func even(_ v: Double) -> Int { max(2, Int(v.rounded(.down)) / 2 * 2) }

    /// The size scaled down (never up) to fit `maxSize`, with even sides.
    public static func fitted(width: Int, height: Int) -> (width: Int, height: Int) {
        guard width > 0, height > 0 else { return (1280, 720) }
        let scale = min(1, Double(maxSize.width) / Double(width), Double(maxSize.height) / Double(height))
        return (even(Double(width) * scale), even(Double(height) * scale))
    }

    /// The output's size: a grid's canvas, or the largest picture of the
    /// follow cut's sources (smaller ones are letterboxed in it), scaled
    /// down to fit 1920 by 1080.
    public static func outputSize(_ plan: EditPlan) -> (width: Int, height: Int) {
        switch plan.kind {
        case .grid:
            let c = plan.canvas ?? .init(width: 1920, height: 1080)
            return fitted(width: c.width, height: c.height)
        case .follow:
            let used = Set(plan.segments.map(\.source))
            let sources = plan.sources.filter { used.contains($0.source) }
            let w = sources.map(\.width).max() ?? 0, h = sources.map(\.height).max() ?? 0
            return fitted(width: w, height: h)
        }
    }

    /// How many frames the plan makes at `fps`.
    public static func frameCount(_ plan: EditPlan, fps: Int = framesPerSecond) -> Int {
        let ms = max(plan.endMs - plan.startMs, 0)
        return max(1, Int((ms * Int64(fps) + 999) / 1000))
    }

    /// The plan time (device-local milliseconds) of frame `i`; the last
    /// frame never lies past the plan's end.
    public static func time(_ plan: EditPlan, frame i: Int, fps: Int = framesPerSecond) -> Int64 {
        min(plan.startMs + Int64(i) * 1000 / Int64(fps), plan.endMs)
    }

    /// A piece of one file's sound written into the output.
    public struct AudioPiece: Sendable, Equatable {
        public var source: Int
        /// Where in the file the piece starts and ends.
        public var fileStartMs: Int64
        public var fileEndMs: Int64
        /// Where it starts in the output, from its first frame.
        public var atMs: Int64

        public init(source: Int, fileStartMs: Int64, fileEndMs: Int64, atMs: Int64) {
            self.source = source
            self.fileStartMs = fileStartMs
            self.fileEndMs = fileEndMs
            self.atMs = atMs
        }
    }

    /// The sound of the output, by the rules of the live view: the follow
    /// cut carries the shown camera's sound (it cuts with the picture); a
    /// grid carries one cell's (the first unless another is chosen) while it
    /// has video. Adjacent pieces of one file join.
    public static func audioPieces(_ plan: EditPlan, gridCell: Int = 0) -> [AudioPiece] {
        var raw: [AudioPiece] = []
        switch plan.kind {
        case .follow:
            for s in plan.segments where s.endMs > s.startMs {
                raw.append(.init(source: s.source, fileStartMs: s.sourceInMs, fileEndMs: s.sourceInMs + (s.endMs - s.startMs), atMs: s.startMs - plan.startMs))
            }
        case .grid:
            guard plan.cells.indices.contains(gridCell), let source = plan.source(plan.cells[gridCell].source) else { return [] }
            for span in plan.cells[gridCell].present where span.count == 2 {
                let a = max(span[0], plan.startMs), b = min(span[1], plan.endMs)
                guard b > a else { continue }
                raw.append(.init(source: source.source, fileStartMs: EditTimeline.filePosition(source, at: a), fileEndMs: EditTimeline.filePosition(source, at: b), atMs: a - plan.startMs))
            }
            raw.sort { $0.atMs < $1.atMs }
        }
        var out: [AudioPiece] = []
        for p in raw {
            if var last = out.last, last.source == p.source, last.atMs + (last.fileEndMs - last.fileStartMs) == p.atMs,
                last.fileEndMs == p.fileStartMs
            {
                last.fileEndMs = p.fileEndMs
                out[out.count - 1] = last
            } else {
                out.append(p)
            }
        }
        return out
    }
}

/// What an export reports while it runs and when it ends.
public struct ExportOutcome: Sendable, Equatable {
    public enum Result: Sendable, Equatable {
        case done
        case canceled
        case failed(String)
    }

    public var result: Result
    public var url: URL
    /// What was written (zero unless done).
    public var width: Int
    public var height: Int
    public var frames: Int
    public var bytes: Int64

    public init(result: Result, url: URL, width: Int = 0, height: Int = 0, frames: Int = 0, bytes: Int64 = 0) {
        self.result = result
        self.url = url
        self.width = width
        self.height = height
        self.frames = frames
        self.bytes = bytes
    }
}
