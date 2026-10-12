import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Testing
import VideoKit

/// The follow cut and grids as the player shows them: the plan parsed, the
/// plan-to-timeline mapping, cut selection, whose sound plays, the
/// composition, and the edit's clock (pure); then real files decoded and
/// drawn (`EditSession`).
@MainActor
struct EditPlanTests {
    static let cam = { (ch: Int) in #"{"device":"d-example","channel":\#(ch),"lens":0}"# }

    /// Section 7's example shape: two shots of a follow cut.
    static let followJSON = """
        {"version": 1, "kind": "follow", "download": "dl-1", "start_ms": 100000, "end_ms": 126000,
         "suggested_file": "<folder>/follow.mp4", "copy_possible": true,
         "sources": [
          {"source": 0, "camera": \(cam(3)), "name": "Camera 1", "file": "/x/1.mp4", "file_start_ms": 99400, "offset_ms": 0,
           "codec": "h264", "width": 2560, "height": 1440},
          {"source": 1, "camera": \(cam(5)), "name": "Camera 2", "file": "/x/2.mp4", "file_start_ms": 106900, "offset_ms": 0,
           "codec": "h264", "width": 2560, "height": 1440}],
         "segments": [{"start_ms": 100000, "end_ms": 111000, "source": 0, "source_in_ms": 600},
                      {"start_ms": 111000, "end_ms": 120000, "source": 1, "source_in_ms": 4100},
                      {"start_ms": 121000, "end_ms": 126000, "source": 1, "source_in_ms": 14100}],
         "labels": [{"source": 0, "name": "Camera 1", "time": true}]}
        """

    static let gridJSON = """
        {"version": 1, "kind": "grid", "download": "dl-1", "layout": "2up", "start_ms": 100000, "end_ms": 130000,
         "suggested_file": "x", "copy_possible": false,
         "sources": [
          {"source": 0, "camera": \(cam(3)), "name": "Camera 1", "file": "/x/1.mp4", "file_start_ms": 99000, "offset_ms": 0,
           "codec": "h264", "width": 1920, "height": 1080},
          {"source": 1, "camera": \(cam(5)), "name": "Camera 2", "file": "/x/2.mp4", "file_start_ms": 105000, "offset_ms": -400,
           "codec": "h264", "width": 1920, "height": 1080}],
         "canvas": {"width": 1920, "height": 1080},
         "cells": [{"rect": {"x": 0, "y": 0, "width": 960, "height": 1080}, "source": 0, "fit": "fit", "present": [[100000, 130000]]},
                   {"rect": {"x": 960, "y": 0, "width": 960, "height": 1080}, "source": 1, "fit": "fill", "present": [[105000, 125000]]}]}
        """

    @Test func parsesBothKinds() throws {
        let f = try #require(EditPlan.parse(Self.followJSON))
        #expect(f.kind == .follow && f.sources.count == 2 && f.segments.count == 3 && f.copyPossible)
        #expect(f.sources[1].camera == RecordingCamera(device: "d-example", channel: 5))
        #expect(f.labels.first?.time == true)
        let g = try #require(EditPlan.parse(Self.gridJSON))
        #expect(g.kind == .grid && g.cells.count == 2 && g.canvas == .init(width: 1920, height: 1080))
        #expect(g.sources[1].offsetMs == -400 && g.cells[1].fit == "fill")
        // A plan naming a source it does not list, or not a plan at all.
        #expect(EditPlan.parse(Self.followJSON.replacingOccurrences(of: #""source": 1, "source_in_ms": 4100"#, with: #""source": 7, "source_in_ms": 4100"#)) == nil)
        #expect(EditPlan.parse("{}") == nil)
    }

    @Test func followMapsTimeToShotAndFilePosition() throws {
        let p = try #require(EditPlan.parse(Self.followJSON))
        #expect(EditTimeline.shot(p, at: 100000) == .init(segment: 0, source: 0, filePositionMs: 600))
        #expect(EditTimeline.shot(p, at: 110999) == .init(segment: 0, source: 0, filePositionMs: 11599))
        // The cut instant shows the incoming shot.
        #expect(EditTimeline.shot(p, at: 111000) == .init(segment: 1, source: 1, filePositionMs: 4100))
        // A gap is black; the plan's last instant shows the last picture.
        #expect(EditTimeline.shot(p, at: 120500) == nil)
        #expect(EditTimeline.shot(p, at: 126000)?.filePositionMs == 19100)
        #expect(EditTimeline.shot(p, at: 99999) == nil && EditTimeline.shot(p, at: 126001) == nil)
    }

    @Test func cutsAreSourceChangesAndGaps() throws {
        let p = try #require(EditPlan.parse(Self.followJSON))
        #expect(EditTimeline.cuts(p) == [111000, 121000])
        var same = p
        same.segments = [
            .init(startMs: 0, endMs: 10, source: 0, sourceInMs: 0), .init(startMs: 10, endMs: 20, source: 0, sourceInMs: 10),
            .init(startMs: 20, endMs: 30, source: 1, sourceInMs: 0),
        ]
        #expect(EditTimeline.cuts(same) == [20])
        #expect(EditTimeline.cuts(try #require(EditPlan.parse(Self.gridJSON))).isEmpty)
        // The next camera and where it starts (the renderer decodes it early).
        #expect(EditTimeline.nextShot(p, after: 105000)?.at == 111000)
        #expect(EditTimeline.nextShot(p, after: 105000)?.shot == .init(segment: 1, source: 1, filePositionMs: 4100))
        #expect(EditTimeline.nextShot(p, after: 112000) == nil)
        #expect(EditTimeline.nextShot(p, after: 99000)?.shot.source == 0)
    }

    @Test func gridCellsArePresentWhereTheirFilesHaveVideo() throws {
        let p = try #require(EditPlan.parse(Self.gridJSON))
        #expect(EditTimeline.cellPosition(p, cell: 0, at: 100000) == 1000)
        // The offset moves the camera's clock: -400 ms means its file is read 400 ms later.
        #expect(EditTimeline.cellPosition(p, cell: 1, at: 110000) == 5400)
        #expect(EditTimeline.cellPosition(p, cell: 1, at: 104999) == nil)
        #expect(EditTimeline.cellPosition(p, cell: 1, at: 125000) == nil)
        #expect(EditTimeline.cellPosition(p, cell: 0, at: 130000) == 31000)
        #expect(EditTimeline.cellPosition(p, cell: 2, at: 110000) == nil)
    }

    @Test func soundFollowsThePicture() throws {
        let f = try #require(EditPlan.parse(Self.followJSON))
        #expect(EditTimeline.audibleSource(f, at: 105000, rate: 1, muted: false) == 0)
        #expect(EditTimeline.audibleSource(f, at: 111000, rate: 1, muted: false) == 1)
        #expect(EditTimeline.audibleSource(f, at: 120500, rate: 1, muted: false) == nil)
        #expect(EditTimeline.audibleSource(f, at: 105000, rate: 1, muted: true) == nil)
        #expect(EditTimeline.audibleSource(f, at: 105000, rate: 2, muted: false) == nil)
        #expect(EditTimeline.audibleSource(f, at: 105000, rate: 0.5, muted: false) == nil)
        let g = try #require(EditPlan.parse(Self.gridJSON))
        #expect(EditTimeline.audibleSource(g, at: 110000, rate: 1, muted: false) == 0)
        #expect(EditTimeline.audibleSource(g, at: 110000, rate: 1, muted: false, gridCell: 1) == 1)
        #expect(EditTimeline.audibleSource(g, at: 101000, rate: 1, muted: false, gridCell: 1) == nil)
    }

    @Test func compositionLetterboxesTheCanvas() throws {
        let g = try #require(EditPlan.parse(Self.gridJSON))
        // A square view: the 16:9 canvas sits in the middle, its cells scaled.
        let bounds = CGRect(x: 0, y: 0, width: 960, height: 960)
        #expect(EditComposition.frame(g, in: bounds) == CGRect(x: 0, y: 210, width: 960, height: 540))
        #expect(EditComposition.cellRects(g, in: bounds) == [CGRect(x: 0, y: 210, width: 480, height: 540), CGRect(x: 480, y: 210, width: 480, height: 540)])
        let layers = EditComposition.layers(g, at: 110000, in: bounds)
        #expect(layers.map(\.source) == [0, 1] && layers.map(\.fill) == [false, true] && layers.map(\.cell) == [0, 1])
        #expect(EditComposition.layers(g, at: 101000, in: bounds).map(\.source) == [0])
        let f = try #require(EditPlan.parse(Self.followJSON))
        #expect(EditComposition.layers(f, at: 112000, in: bounds) == [.init(source: 1, filePositionMs: 5100, rect: bounds, fill: false)])
    }

    @Test func clockRunsPausesSeeksAndStopsAtTheEnd() {
        var c = EditClock(startMs: 1000, endMs: 11000)
        #expect(c.position(at: 50) == 1000 && c.state(at: 50) == .paused)
        c.play(at: 100)
        #expect(c.position(at: 102.5) == 3500 && c.state(at: 102.5) == .playing)
        c.setRate(4, at: 102.5)
        #expect(c.position(at: 103) == 5500)
        c.pause(at: 103)
        #expect(c.position(at: 200) == 5500)
        c.seek(to: 10500, at: 200)
        c.setRate(1, at: 200)
        c.play(at: 200)
        #expect(c.position(at: 201) == 11000 && c.state(at: 201) == .ended)
        // Play at the end starts again.
        c.play(at: 201)
        #expect(c.position(at: 201) == 1000 && c.state(at: 201) == .playing)
        c.seek(to: 0, at: 202)
        #expect(c.position(at: 202) == 1000)
        #expect(c.transport(at: 202).isPlaying)
    }

    /// Two synthetic recordings decoded and drawn by the grid renderer:
    /// the follow cut shows the right camera's color and the right frame on
    /// each side of a cut, forward, backward, and at keyframes only.
    @Test func sessionDrawsTheRightPictures() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-edit-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.mp4"), b = dir.appendingPathComponent("b.mp4")
        // 10 fps, a keyframe every 20 frames; 6 s each.
        try await SyntheticRecording.write(.init(frames: 60, color: 0), to: a)
        try await SyntheticRecording.write(.init(frames: 60, color: 2), to: b)
        let cam = RecordingCamera(device: "d-example", channel: 0)
        var plan = EditPlan(
            kind: .follow, startMs: 10_000, endMs: 15_000,
            sources: [
                .init(source: 0, camera: cam, name: "A", file: a.path, fileStartMs: 10_000, width: 640, height: 360),
                .init(source: 1, camera: cam, name: "B", file: b.path, fileStartMs: 9_000, width: 640, height: 360),
            ],
            segments: [
                .init(startMs: 10_000, endMs: 12_000, source: 0, sourceInMs: 0),
                .init(startMs: 12_000, endMs: 15_000, source: 1, sourceInMs: 3_000),
            ])
        plan.labels = []
        let session = await EditSession.open(plan)
        defer { session.close() }
        #expect(session.failures.isEmpty)
        let file = try #require(session.files[0])
        #expect(file.info.keyframesMs == [0, 2000, 4000] && file.info.width == 640 && file.info.codec == "h264")
        #expect(file.keyframe(atOrBefore: 3999) == 2000)
        let renderer = try GridRenderer()
        func look(_ t: Int64) throws -> (color: Int?, frame: Int) {
            let image = try #require(session.render(at: t, width: 320, height: 180, with: renderer))
            return SyntheticRecording.read(image, picture: CGRect(x: 0, y: 0, width: 320, height: 180))
        }
        // Source 0 at file 1.55 s is frame 15; across the cut, source 1 at 3.0 s is frame 30.
        #expect(try look(11_550) == (0, 15))
        #expect(try look(11_999) == (0, 19))
        #expect(try look(12_000) == (2, 30))
        #expect(try look(14_250) == (2, 52))
        // Backward across the cut and inside a group of pictures.
        #expect(try look(10_350) == (0, 3))
        let at13 = try look(13_000)
        #expect(at13 == (2, 40), "\(at13) keyframes \(session.files[1]!.info.keyframesMs)")
        #expect(try look(12_100) == (2, 31))
        // Keyframes only: the keyframe at or before.
        let other = try #require(session.files[1])
        other.show(3_900, keyframesOnly: true)
        #expect(other.shownPositionMs == 2_000)
    }
}
