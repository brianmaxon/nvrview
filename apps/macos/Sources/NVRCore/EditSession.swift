import CoreGraphics
import Foundation
import VideoKit

/// An edit list's downloaded files, open for drawing (docs/UX.md
/// "Recordings", "Follow and Grid"): each source's MP4 decoded by
/// VideoToolbox (`RecordedFileSource`), and at any time `t` the plan's
/// pictures as `GridTile`s for the Metal grid renderer, the one live view
/// uses. Nothing is re-encoded; the screen and an export (M4) draw the same
/// tiles: the screen through `request` (decoding off the main thread, the
/// newest request winning) and its display link, an export or the self-test
/// through `prepare` and `render` (decoding before drawing, frame by frame).
@MainActor
public final class EditSession {
    public let plan: EditPlan
    /// The open files by source number; a source whose file could not be
    /// opened is missing and draws black.
    public let files: [Int: RecordedFileSource]
    /// Sources whose files could not be opened, with why.
    public let failures: [Int: RecordedFileError]

    /// How long before a cut the incoming camera starts decoding its first
    /// picture, so the cut shows it at once.
    public static let prerollMs: Int64 = 1500

    private init(plan: EditPlan, files: [Int: RecordedFileSource], failures: [Int: RecordedFileError]) {
        self.plan = plan
        self.files = files
        self.failures = failures
    }

    /// Opens every source's file (only those the plan shows).
    public static func open(_ plan: EditPlan) async -> EditSession {
        var used = Set(plan.segments.map(\.source)).union(plan.cells.map(\.source))
        if used.isEmpty { used = Set(plan.sources.map(\.source)) }
        var files: [Int: RecordedFileSource] = [:]
        var failures: [Int: RecordedFileError] = [:]
        for s in plan.sources where used.contains(s.source) {
            do throws(RecordedFileError) {
                files[s.source] = try await RecordedFileSource.open(URL(fileURLWithPath: s.file))
            } catch {
                failures[s.source] = error
            }
        }
        return EditSession(plan: plan, files: files, failures: failures)
    }

    /// The file positions the pictures at `t` need, with the follow cut's
    /// next camera when its cut is near.
    private func wanted(at t: Int64) -> [(source: Int, ms: Int64)] {
        var out = EditComposition.layers(plan, at: t, in: .zero).map { ($0.source, $0.filePositionMs) }
        if let next = EditTimeline.nextShot(plan, after: t), next.at - t <= Self.prerollMs,
            !out.contains(where: { $0.0 == next.shot.source })
        {
            out.append((next.shot.source, next.shot.filePositionMs))
        }
        return out
    }

    /// Asks for the pictures at `t`; each file decodes on its own queue.
    public func request(at t: Int64, keyframesOnly: Bool = false) {
        for w in wanted(at: t) { files[w.source]?.want(w.ms, keyframesOnly: keyframesOnly) }
    }

    /// Decodes the pictures at `t` before returning (export, self-test).
    public func prepare(at t: Int64, keyframesOnly: Bool = false) {
        for l in EditComposition.layers(plan, at: t, in: .zero) {
            files[l.source]?.show(l.filePositionMs, keyframesOnly: keyframesOnly)
        }
    }

    /// The tiles to draw the edit at `t` into `bounds` (pixels of the
    /// target, top-left origin): each picture fitted (or filled) in its
    /// place with its file's colors. A missing file draws nothing (black).
    public func tiles(at t: Int64, in bounds: CGRect) -> [GridTile] {
        EditComposition.layers(plan, at: t, in: bounds).compactMap { layer -> GridTile? in
            guard let file = files[layer.source] else { return nil }
            let size = file.latestPicture().map { CGSize(width: $0.width, height: $0.height) }
                ?? CGSize(width: file.info.width, height: file.info.height)
            let fit = PictureFit.fit(picture: size, in: layer.rect, fill: layer.fill)
            return GridTile(picture: file, color: file.info.color, destination: fit.destination, source: fit.source)
        }
    }

    /// Where each picture at `t` is drawn in `bounds` (letterboxed), by
    /// source: what the self-test samples.
    public func pictureRects(at t: Int64, in bounds: CGRect) -> [(source: Int, cell: Int?, rect: CGRect)] {
        EditComposition.layers(plan, at: t, in: bounds).compactMap { layer in
            guard let file = files[layer.source] else { return nil }
            let size = CGSize(width: file.info.width, height: file.info.height)
            return (layer.source, layer.cell, PictureFit.fit(picture: size, in: layer.rect, fill: layer.fill).destination)
        }
    }

    /// Decodes and draws the edit at `t` into an offscreen frame of
    /// `width` by `height` (an export's frame; the self-test's samples).
    public func render(at t: Int64, width: Int, height: Int, with renderer: GridRenderer) -> RenderedImage? {
        prepare(at: t)
        renderer.tiles = tiles(at: t, in: CGRect(x: 0, y: 0, width: width, height: height))
        return renderer.renderOffscreen(width: width, height: height)
    }

    /// Lets go of every file's reader and picture.
    public func close() {
        for f in files.values { f.close() }
    }
}
