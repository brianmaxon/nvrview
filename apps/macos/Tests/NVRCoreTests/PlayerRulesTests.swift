import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Testing

/// The Recordings window player's pure rules (PlayerRules.swift).
@Suite struct PlayerRulesTests {
    @Test func speeds() {
        #expect(PlaybackSpeed.all == [0.25, 0.5, 1, 2, 4, 8, 16])
        #expect(PlaybackSpeed.label(0.25) == "0.25×")
        #expect(PlaybackSpeed.label(1) == "1×")
        #expect(PlaybackSpeed.label(16) == "16×")
        // From 2× up: keyframes only, no sound; below 1×: no sound.
        #expect(!PlaybackSpeed.keyframesOnly(1) && PlaybackSpeed.keyframesOnly(2))
        #expect(PlaybackSpeed.hasSound(1) && !PlaybackSpeed.hasSound(0.5) && !PlaybackSpeed.hasSound(2))
        #expect(PlaybackSpeed.hint(1) == nil)
        #expect(PlaybackSpeed.hint(8) == "Keyframes only, no sound")
        #expect(PlaybackSpeed.hint(0.5) == "No sound below 1×")
        #expect(PlaybackSpeed.faster(than: 1) == 2 && PlaybackSpeed.faster(than: 16) == nil)
        #expect(PlaybackSpeed.slower(than: 1) == 0.5 && PlaybackSpeed.slower(than: 0.25) == nil)
    }

    @Test func openRefusalsSayWhy() {
        #expect(PlaybackOpenMessage.text(.invalidArgument) == "A camera's device is not searched for recordings")
        #expect(PlaybackOpenMessage.text(.limit) == "Too many cameras or playbacks to play this")
        #expect(PlaybackOpenMessage.text(.notConfigured) == "Recordings are not set up yet")
        #expect(PlaybackOpenMessage.text(.unknownID) == "That incident changed; select it again")
        #expect(PlaybackOpenMessage.text(.internal) == "The recording could not be played (status -5)")
    }

    /// The same vectors as the Windows app's RecordingsBrowseTests "timeline viewport".
    @Test func timelineViewport() {
        func near(_ a: Double, _ b: Double) -> Bool { abs(a - b) < 1e-9 }
        let whole = TimelineViewport()
        #expect(whole.zoom == 1 && whole.start == 0 && whole.visible == 1)
        // A notch at the middle: the middle stays.
        let a = whole.zoomed(by: TimelineViewport.notch, at: 0.5)
        #expect(near(a.zoom, 1.25) && near(a.start, 0.1) && near(a.fraction(atPointer: 0.5), 0.5))
        // Off center: the time under the pointer stays under it.
        let b = TimelineViewport(zoom: 4, start: 0.3)
        let b2 = b.zoomed(by: 1.25, at: 0.25)
        #expect(near(b2.zoom, 5) && near(b2.start, 0.3125))
        #expect(near(b.fraction(atPointer: 0.25), b2.fraction(atPointer: 0.25)))
        // Clamped to 48x, and to the day's ends.
        let c = TimelineViewport(zoom: 40, start: 0.5).zoomed(by: 2, at: 0)
        #expect(c.zoom == 48 && near(c.start, 0.5))
        let d = TimelineViewport(zoom: 2, start: 0.5).zoomed(by: 0.25, at: 1)
        #expect(d.zoom == 1 && d.start == 0)
        let e = whole.zoomed(by: 2, at: 1)
        #expect(e.zoom == 2 && near(e.start, 0.5))
        #expect(near(TimelineViewport(zoom: 2).panned(by: 5).start, 0.5))
        #expect(TimelineViewport(zoom: 2, start: 0.25).panned(by: -5).start == 0)
        #expect(near(TimelineViewport(zoom: 4, start: 0.25).panned(by: 0.5).start, 0.375))
        let f = TimelineViewport(zoom: 100, start: 2)
        #expect(f.zoom == 48 && near(f.start, 1 - 1.0 / 48))
        #expect(near(TimelineViewport.wheelFactor(notches: 2), 1.5625) && near(TimelineViewport.wheelFactor(notches: -1), 0.8))
    }

    @Test func transport() {
        let t = TransportState(startMs: 1_000_000, endMs: 1_060_000, positionMs: 1_055_000, state: .paused, rate: 1)
        #expect(t.showsPlay && !t.isPlaying)
        #expect(t.skipped(by: TransportState.skipMs) == 1_060_000, "clamped to the end")
        #expect(t.skipped(by: -TransportState.skipMs) == 1_045_000)
        #expect(t.clamp(0) == 1_000_000)
        var playing = t
        playing.state = .buffering
        #expect(playing.isPlaying && !playing.showsPlay, "buffering still counts as playing")
        #expect(TransportState.fraction(1_030_000, from: 1_000_000, to: 1_060_000) == 0.5)
        #expect(TransportState.fraction(5, from: 10, to: 10) == 0)
        #expect(TransportState.time(atFraction: 0.25, from: 0, to: 60_000) == 15_000)
        #expect(TransportState.time(atFraction: 2, from: 0, to: 60_000) == 60_000)
        // 2026-10-10 07:42:05.900 device time shows the whole second.
        #expect(TransportState.clock((1_791_590_400 + 7 * 3600 + 42 * 60 + 5) * 1000 + 900) == "07:42:05")
        #expect((-1 as Int64).floorDiv(1000) == -1)
    }

    @Test func gridCells() {
        let b = CGRect(x: 0, y: 0, width: 802, height: 402)
        #expect(PlayerGrid.cells(count: 1, mode: .grid, in: b) == [b])
        let two = PlayerGrid.cells(count: 2, mode: .grid, in: b, gap: 2)
        #expect(two == [CGRect(x: 0, y: 0, width: 400, height: 402), CGRect(x: 402, y: 0, width: 400, height: 402)])
        let four = PlayerGrid.cells(count: 4, mode: .grid, in: b, gap: 2).compactMap { $0 }
        #expect(four.count == 4 && four[3] == CGRect(x: 402, y: 202, width: 400, height: 200))
        // Three take a two by two grid with one cell empty.
        #expect(PlayerGrid.cells(count: 3, mode: .grid, in: b, gap: 2).compactMap { $0 }.count == 3)
        // One large: three quarters of the width; the others beside it,
        // no taller than a third.
        let big = PlayerGrid.cells(count: 3, mode: .emphasis(1), in: b, gap: 2)
        #expect(big[1] == CGRect(x: 0, y: 0, width: 600, height: 402))
        #expect(big[0]?.minX == 602 && big[2]?.minX == 602)
        #expect((big[0]?.height ?? 0) <= 402 / 3 + 0.001)
        #expect((big[0]?.maxY ?? 0) <= (big[2]?.minY ?? 0))
        // Alone: only that camera.
        #expect(PlayerGrid.cells(count: 3, mode: .single(2), in: b) == [nil, nil, b])
        // A mode beyond the cameras falls back to the grid.
        #expect(PlayerGrid.normalized(.single(3), count: 2) == .grid)
        #expect(PlayerGrid.normalized(.emphasis(0), count: 1) == .grid)
    }

    @Test func gridClicks() {
        #expect(PlayerGrid.click(.grid, on: 2, count: 3) == .emphasis(2))
        #expect(PlayerGrid.click(.emphasis(2), on: 2, count: 3) == .grid)
        #expect(PlayerGrid.click(.emphasis(2), on: 0, count: 3) == .emphasis(0))
        #expect(PlayerGrid.click(.single(1), on: 1, count: 3) == .single(1))
        #expect(PlayerGrid.click(.grid, on: 0, count: 1) == .grid)
        #expect(PlayerGrid.doubleClick(.grid, on: 1, count: 2) == .single(1))
        #expect(PlayerGrid.doubleClick(.single(1), on: 1, count: 2) == .grid)
        #expect(PlayerGrid.doubleClick(.emphasis(0), on: 1, count: 2) == .single(1))
        #expect(PlayerGrid.audibleIndex(.grid, count: 3) == 0)
        #expect(PlayerGrid.audibleIndex(.emphasis(2), count: 3) == 2)
        #expect(PlayerGrid.audibleIndex(.grid, count: 0) == nil)
        #expect(PlayerGrid.cameras([1, 2, 3, 4, 5]) == [1, 2, 3, 4])
    }

    @Test func panes() {
        let both = RecordingsPanes.sidebarWidth + RecordingsPanes.inspectorWidth + RecordingsPanes.centerMin
        #expect(RecordingsPanes.shown(width: both, sidebar: true, inspector: true) == (true, true))
        // The inspector gives way first, then the sidebar.
        #expect(RecordingsPanes.shown(width: both - 1, sidebar: true, inspector: true) == (true, false))
        #expect(RecordingsPanes.windowMin.width == RecordingsPanes.sidebarWidth + RecordingsPanes.centerMin)
        #expect(RecordingsPanes.shown(width: RecordingsPanes.windowMin.width, sidebar: true, inspector: true) == (true, false))
        #expect(RecordingsPanes.shown(width: 600, sidebar: true, inspector: true) == (false, false))
        // The inspector alone fits where both side panes would not.
        #expect(RecordingsPanes.shown(width: 800, sidebar: false, inspector: true) == (false, true))
        #expect(RecordingsPanes.shown(width: 2000, sidebar: false, inspector: false) == (false, false))
        // Lanes: all of them while they fit in a third; at least two lanes.
        #expect(RecordingsPanes.lanesHeight(height: 900, lanes: 3) == CGFloat(18 + 3 * 26 + 8))
        #expect(RecordingsPanes.lanesHeight(height: 600, lanes: 16) == (600 - RecordingsPanes.transportHeight) / 3)
        #expect(RecordingsPanes.lanesHeight(height: 200, lanes: 16) == CGFloat(18 + 2 * 26 + 8))
    }

    @Test func splitAndStrip() {
        #expect(IncidentSplit.second(start: 100, end: 110, playheadMs: 105_400) == 105)
        #expect(IncidentSplit.second(start: 100, end: 110, playheadMs: 100_900) == nil, "at the first second")
        #expect(IncidentSplit.second(start: 100, end: 110, playheadMs: 110_000) == nil, "at the last second")
        #expect(IncidentSplit.second(start: 100, end: 110, playheadMs: 95_000) == nil)
        #expect(IncidentRowStrip.slots(1) == (1, 0))
        #expect(IncidentRowStrip.slots(2) == (2, 0))
        #expect(IncidentRowStrip.slots(4) == (2, 2))
        #expect(IncidentRowStrip.width == 2 * IncidentRowStrip.thumbnail.width + IncidentRowStrip.spacing)
    }

    @Test func requestsAndIncidentMargin() throws {
        let cam = RecordingCamera(device: "d-1", channel: 3)
        let want = RecordingsRequest.want([cam], months: [1_788_220_800])
        let o = try #require(try JSONSerialization.jsonObject(with: Data(want.utf8)) as? [String: Any])
        #expect(o["months"] as? [Int] == [1_788_220_800] && o["days"] == nil)
        let json = #"{"revision":1,"incidents":[{"key":"i-x","start":10,"end":20,"margin_s":5}]}"#
        #expect(IncidentsAnswer.parse(answer: json)?.incidents.first?.margin == 5)
    }

    @Test func fakeCalendarMonthAlone() throws {
        let core = FakeCore(options: FakeCore.Options(clips: []), sink: NullSink())
        try core.recordingsConfigure(json: "{}")
        try core.recordingsEnable(deviceKey: "d-1", json: #"{"channels":[{"channel":0}]}"#)
        let now = DeviceTime.today(now: Date(), utcOffset: TimeZone.current.secondsFromGMT())
        let previous = DeviceTime.month(DeviceTime.month(containing: now).start, adding: -1)
        let cam = RecordingCamera(device: "d-1", channel: 0)
        try core.recordingsWant(json: RecordingsRequest.want([cam], months: [previous + 86_400]))
        let m = DeviceTime.month(containing: previous)
        let cal = CalendarAnswer.parse(answer: try core.recordingsCalendar(
            query: RecordingsRequest.query(start: m.start, end: m.next - 1, cameras: [cam])))
        #expect(cal?.allDays.count == Int((m.next - m.start) / 86_400))
        #expect(core.recordingsMonthRequests == 1)
        #expect(throws: CoreStatus.limit) {
            try core.recordingsWant(json: RecordingsRequest.want([cam], months: (0..<13).map { Int64($0) * 2_700_000 }))
        }
    }
}

/// A sink that drops everything.
private final class NullSink: CoreSink, @unchecked Sendable {
    func core(didDeliver action: CoreAction, tile: UInt64) {}
    func core(didEmit json: UnsafeRawBufferPointer) {}
}
