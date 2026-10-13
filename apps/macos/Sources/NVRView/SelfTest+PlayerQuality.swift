import AppKit
import FakeCore
import Foundation
import NVRCore

// The recording player's Quality choice (docs/UX.md "Recordings", "Quality"):
// Auto's streams by layout, speed and zoom, High and Low during playback,
// each camera's tier as the core reports it, the position and the zoom kept
// (the self-test's "recordings" steps; scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared
// helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// Each camera's tier as the core's playback status reports it, in the
    /// player's order.
    private func reportedTiers(_ fake: FakeCore, _ p: RecordingsPlayer) -> [String] {
        guard let id = p.playback, let text = try? fake.playbackStatus(id),
            let object = (try? JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any],
            let list = object["cameras"] as? [[String: Any]]
        else { return [] }
        return p.cells.map { cell in
            list.first { ($0["tile"] as? NSNumber)?.uint64Value == cell.tile.id }?["tier"] as? String ?? "?"
        }
    }

    /// The incident plays on Auto (the sub-stream in the grid), the single
    /// view makes Auto pick the main stream, Low and High are forced during
    /// playback and at speed, a zoom past 2x raises a grid tile, and the
    /// core reports each camera's tier while the playback is never reopened,
    /// the position never goes back, and the zoom is kept.
    func playbackQuality(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-quality
        let p = b.player
        guard p.playback != nil, p.cells.count >= 2 else { return failPlayback("no player to change the quality of", b, fake) }
        let opens = p.opens
        let a = p.cells[0].camera, c = p.cells[1].camera
        let count = p.cells.count
        func expect(_ what: String, _ tiers: [String]) {
            let got = reportedTiers(fake, p)
            print("player quality: \(what): \(got)")
            if got != tiers { problems.append("player quality: \(what): the core reports \(got), not \(tiers)") }
        }
        func position() -> Int64 { (try? fake.playbackPosition(p.playback ?? 0).positionMs) ?? -1 }
        let sub = [String](repeating: "sub", count: count)
        func only(_ i: Int, _ tier: String = "main") -> [String] {
            var t = sub
            t[i] = tier
            return t
        }

        // Auto in the grid at 1x: the sub-stream.
        p.mode = .grid
        p.setRate(1)
        for cell in p.cells { p.setQuality(.auto, for: cell.camera) }
        expect("Auto in the grid", sub)
        if p.cells.map({ p.streams.label($0.camera) }).contains(where: { $0 != "sub stream" }) {
            problems.append("player quality: the header does not say sub stream in the grid")
        }

        // The zoom is kept across every change below.
        p.tileViews.zoom(a, by: 1.5, atUnit: CGPoint(x: 0.5, y: 0.5), base: .whole)
        let zoom = p.tileViews.zoom(a)
        let changes = p.streamChanges
        var last = position()

        // The single view: Auto asks for the main stream of that camera only.
        p.doubleClick(0)
        expect("Auto in the single view", only(0))
        if p.streams.label(a) != "main stream" { problems.append("player quality: the header says \(p.streams.label(a)) for the main stream") }
        if position() < last { problems.append("player quality: the position went back at the single view") }
        last = position()

        // Low during playback, then High on the other camera while it is hidden.
        p.setQuality(.low, for: a)
        expect("Low in the single view", sub)
        p.setQuality(.high, for: c)
        expect("High on another camera", only(1))
        if p.streams.label(a) != "Low · sub stream" { problems.append("player quality: the header says \(p.streams.label(a)) for Low") }

        // At speed: Auto goes to the sub-stream, forced choices stay.
        p.setQuality(.auto, for: a)
        expect("Auto in the single view again", [only(0)[0], "main"] + [String](repeating: "sub", count: max(count - 2, 0)))
        p.setRate(4)
        expect("4x: Auto is the sub-stream, High stays", only(1))
        p.setQuality(.auto, for: c)
        expect("4x: all Auto", sub)
        p.setRate(1)
        expect("1x again: the single view's camera is main", only(0))

        // Zoomed past 2x in the grid raises that tile (and 1.6x keeps it).
        p.mode = .grid
        expect("back to the grid", sub)
        p.tileViews.zoom(c, by: 2.1, atUnit: CGPoint(x: 0.5, y: 0.5), base: .whole)
        expect("zoomed past 2x in the grid", only(1))
        p.tileViews.setZoom(.none, for: c)
        expect("zoom reset", sub)

        if p.tileViews.zoom(a) != zoom { problems.append("player quality: the zoom changed from \(zoom) to \(p.tileViews.zoom(a))") }
        if p.opens != opens { problems.append("player quality: the playback was reopened") }
        if position() < last { problems.append("player quality: the position went back") }
        if p.streamChanges == changes { problems.append("player quality: no stream change reached the core") }
        p.tileViews.setZoom(.none, for: a)
        playbackEdits(b, fake, rec)
    }
}
