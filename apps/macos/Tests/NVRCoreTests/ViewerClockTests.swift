import CoreGraphics
import Foundation
import NVRCore
import Testing

// The viewer's time-driven pieces, stepped by a manual clock: nothing here
// waits on real time.

@Suite struct ManualViewerClockTests {
    @Test func movesOnlyWhenAdvanced() {
        let clock = ManualViewerClock(now: 10)
        #expect(clock.now == 10)
        clock.advance(by: 0.25)
        #expect(clock.now == 10.25)
        clock.advance(by: -5)  // never backward
        #expect(clock.now == 10.25)
    }
}

/// One step of `a` (a mutating call cannot sit inside `#expect`).
private func tick(_ a: inout FocusAnimation, _ now: Double) -> FocusAnimation.Finished? {
    a.tick(now: now)
}

@Suite struct FocusAnimationTests {
    @Test func runsForItsDurationAndReportsTheEnd() {
        var a = FocusAnimation(duration: 0.28)
        a.start(to: 1, now: 100)
        #expect(a.isRunning)
        #expect(tick(&a, 100.14) == nil)
        #expect(abs(a.progress - 0.5) < 1e-9)
        #expect(tick(&a, 100.28) == .focused)
        #expect(a.progress == 1)
        #expect(!a.isRunning)
        #expect(tick(&a, 200) == nil)
    }

    @Test func oneLateStepFinishesIt() {
        // A window behind others may get its next refresh seconds later.
        var a = FocusAnimation()
        a.start(to: 1, now: 0)
        #expect(tick(&a, 5) == .focused)
    }

    @Test func reversesFromWhereItIs() {
        var a = FocusAnimation(duration: 1)
        a.start(to: 1, now: 0)
        _ = tick(&a, 0.25)
        a.start(to: 0, now: 0.25)
        _ = tick(&a, 0.5)
        #expect(abs(a.progress - 0.1875) < 1e-9)
        #expect(tick(&a, 1.25) == .unfocused)
        #expect(a.progress == 0)
    }

    @Test func resetStopsAtTheGrid() {
        var a = FocusAnimation()
        a.start(to: 1, now: 0)
        _ = tick(&a, 0.1)
        a.reset()
        #expect(!a.isRunning && a.progress == 0)
        #expect(tick(&a, 10) == nil)
    }
}

@Suite struct ZoomEasingTests {
    private let camera = CameraRef(device: "dev-a", channel: 0)

    /// How a `picture` filling a 16:9 tile sits in it at 1x.
    private func fillBase(_ picture: CGSize) -> ZoomBase {
        ZoomBase(picture: picture, tile: CGSize(width: 640, height: 360), fill: true)
    }

    /// Steps `easing` every `dt` seconds until it ends (at most `limit`
    /// steps), applying each zoom to `zoom`; `base` gives the picture's 1x
    /// placement at each step number. Returns the steps taken.
    private func run(
        _ easing: inout ZoomEasing<CameraRef>, _ zoom: inout TileZoom, clock: ManualViewerClock, dt: Double,
        limit: Int = 1000, base: (Int) -> ZoomBase
    ) -> Int {
        var steps = 0
        while easing.isEasing, steps < limit {
            clock.advance(by: dt)
            let z = zoom
            let n = steps
            for step in easing.tick(now: clock.now, current: { _ in z }, base: { _, _ in base(n) }) {
                zoom = step.zoom
            }
            steps += 1
        }
        return steps
    }

    @Test func resetEndsExactlyAtNoZoom() {
        let clock = ManualViewerClock(now: 50)
        var easing = ZoomEasing<CameraRef>()
        var zoom = TileZoom(scale: 2, center: CGPoint(x: 0.3, y: 0.7))
        easing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5)), for: camera, now: clock.now)
        let steps = run(&easing, &zoom, clock: clock, dt: 1.0 / 60) { _ in .whole }
        #expect(zoom == .none)
        #expect(!zoom.isZoomed)
        #expect(steps > 3 && steps < 60)
    }

    /// The regression: 2x back to 1x while the picture changes size and
    /// shape mid-ease (Auto drops from the main stream to the sub-stream
    /// below 1.6x), with display refreshes arriving in sparse bursts as
    /// for a window behind others. The reset must still end exactly at 1x.
    @Test func resetFollowsAPictureThatChangesSizeMidEase() {
        let clock = ManualViewerClock(now: 0)
        var easing = ZoomEasing<CameraRef>()
        var zoom = TileZoom(scale: 2, center: CGPoint(x: 0.6, y: 0.4)).clamped(in: fillBase(CGSize(width: 2560, height: 1440)))
        easing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5)), for: camera, now: clock.now)
        let main = fillBase(CGSize(width: 2560, height: 1440))  // 16:9: the whole picture
        let sub = fillBase(CGSize(width: 640, height: 480))  // 4:3: a centered crop
        #expect(main != sub)
        // Three quick refreshes on the main stream's picture...
        var steps = run(&easing, &zoom, clock: clock, dt: 0.02, limit: 3) { _ in main }
        #expect(easing.isEasing)
        #expect(zoom.scale < 1.6)
        // ...then the sub-stream's, and the next refresh two seconds later.
        steps += run(&easing, &zoom, clock: clock, dt: 2) { _ in sub }
        #expect(zoom == .none)
        #expect(!easing.isEasing)
        #expect(steps == 4)
    }

    @Test func zoomingInEndsExactlyOnTheTargetAroundThePoint() {
        let clock = ManualViewerClock(now: 0)
        var easing = ZoomEasing<CameraRef>()
        var zoom = TileZoom.none
        let unit = CGPoint(x: 0.25, y: 0.25)
        easing.set(.init(scale: 2, unit: unit), for: camera, now: clock.now)
        _ = run(&easing, &zoom, clock: clock, dt: 1.0 / 120) { _ in .whole }
        #expect(zoom.scale == 2)
        // The picture point under the unit point stayed put.
        let q = zoom.picturePoint(atUnit: unit)
        #expect(abs(q.x - 0.25) < 1e-9 && abs(q.y - 0.25) < 1e-9)
    }

    @Test func aStaleStartDoesNotJump() {
        // The first step is timed from when the target was set, not from an
        // earlier step long ago.
        let clock = ManualViewerClock(now: 0)
        var easing = ZoomEasing<CameraRef>()
        _ = easing.tick(now: 0, current: { _ in .none }, base: { _, _ in .whole })
        clock.advance(by: 30)
        easing.set(.init(scale: 4, unit: CGPoint(x: 0.5, y: 0.5)), for: camera, now: clock.now)
        clock.advance(by: 1.0 / 60)
        let first = easing.tick(now: clock.now, current: { _ in .none }, base: { _, _ in .whole })
        #expect(first.count == 1)
        #expect(first[0].zoom.scale > 1 && first[0].zoom.scale < 2)
    }

    @Test func cancelStopsWhereItIs() {
        var easing = ZoomEasing<CameraRef>()
        easing.set(.init(scale: 3, unit: CGPoint(x: 0.5, y: 0.5)), for: camera, now: 0)
        #expect(easing.target(for: camera)?.scale == 3)
        easing.cancel(camera)
        #expect(!easing.isEasing)
        #expect(easing.tick(now: 1, current: { _ in .none }, base: { _, _ in .whole }).isEmpty)
    }
}

@Suite struct FocusAnimationClockTests {
    @Test func advancingTheClockByTheDurationFinishesIt() {
        // 0.28 added to a large time lands a hair short of start + 0.28.
        let clock = ManualViewerClock(now: 225_170.069_284_458)
        var a = FocusAnimation()
        a.start(to: 1, now: clock.now)
        clock.advance(by: FocusAnimation.defaultDuration)
        #expect(tick(&a, clock.now) == .focused)
    }
}
