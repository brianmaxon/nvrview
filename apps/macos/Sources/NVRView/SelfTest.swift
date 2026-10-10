import AppKit
import QuartzCore
import Foundation
import NVRCore
import VideoKit

/// `--selftest SECONDS`: runs the real app (its window behind all others, no
/// focus, muted), then checks, in order:
///
/// 1. a device is connected, a tile is playing, and a non-flat picture is on
///    screen;
/// 2. resizing the window continuously keeps the grid drawing and
///    resizes its drawable only now and then, then to the exact size;
/// 3. focusing a tile animates it to the whole stage and then switches its
///    camera to the main stream (with `--fake`, whose core models the tier
///    switch), still showing a picture;
/// 4. listening makes the camera audible and its level meter reads a level;
/// 5. leaving focus asks for the sub-stream again;
/// 6. with `--fake`, the fake core saw no contract violation;
/// 7. the sidebar took rows added and removed in several places in one
///    update (a group and a layout at once), and AppKit logged none of the warnings
///    in `forbiddenLog` during the whole run.
///
/// Prints what it measured and exits 0 or 1.
@MainActor
final class SelfTest {
    private unowned let app: AppDelegate
    private var problems: [String] = []

    init(app: AppDelegate) { self.app = app }

    /// Lines AppKit must never log (it says the first will become an assert).
    static let forbiddenLog = ["reentrant operation in its NSTableView delegate"]

    func start(seconds: Double) {
        after(seconds) { [self] in checkGrid() }
        // A hard limit for the whole run.
        after(seconds + 30) { [self] in finish(problems + ["timed out"]) }
    }

    private func after(_ seconds: Double, _ body: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated { body() } }
    }

    private var window: ViewerWindowController? { app.windows.first }

    private func checkGrid() {
        guard let model = app.model, let window else { finish(["no window"]) }
        let connected = model.devices.filter { $0.link.state == .connected }.count
        print("devices: \(model.devices.count), connected: \(connected)")
        if connected == 0 { problems.append("no device connected") }
        let cameras = window.state.tree.cameras
        let playing = cameras.filter { model.leases.tile(for: $0)?.snapshot.status == .playing }.count
        print("window \"\(window.state.name)\": \(window.state.tree.leafCount) tiles, \(cameras.count) cameras, \(playing) playing")
        if let f = window.window?.frame {
            print("windows: \(app.windows.count), first at \(Int(f.minX)),\(Int(f.minY)) \(Int(f.width))x\(Int(f.height))")
        }
        if playing == 0 { problems.append("no tile is playing") }
        if let first = cameras.first(where: { model.leases.tile(for: $0) != nil }), let tile = model.leases.tile(for: first) {
            let shown = window.stage.shownRate(of: first)
            print("rates: " + FrameRateText.make(
                device: tile.deviceRate, shown: shown, displayHz: window.window?.screen?.maximumFramesPerSecond ?? 0))
            // The fake sends 20 pictures per second of camera time.
            if model.fake != nil, abs((tile.deviceRate ?? 0) - 20) > 1 { problems.append("device rate not measured") }
            if shown == nil { problems.append("shown rate not measured") }
        }
        capture("grid") { [self] in resizeStep() }
    }

    /// Resizes the window on every refresh for half a second, like a
    /// full-screen transition: the grid keeps drawing while the drawable
    /// size follows only now and then, and exactly once it settles.
    private func resizeStep() {
        guard let window, let nsWindow = window.window else { finish(problems) }
        let grid = window.stage.grid
        let start = nsWindow.frame
        let resizes = grid.drawableResizes, refreshes = grid.refreshes, drawn = grid.framesDrawn
        let began = CACurrentMediaTime()
        let steps = 60
        func advance(_ step: Int) {
            let t = Double(step) / Double(steps)
            var f = start
            f.size.width += 240 * t
            f.size.height += 135 * t
            nsWindow.setFrame(f, display: true)
            if step < steps {
                after(0.5 / Double(steps)) { advance(step + 1) }
                return
            }
            after(0.3) { [self] in
                let elapsed = CACurrentMediaTime() - began
                let r = grid.drawableResizes - resizes
                let shown = grid.refreshes - refreshes, made = grid.framesDrawn - drawn
                let scale = nsWindow.backingScaleFactor
                let want = CGSize(
                    width: (grid.bounds.width * scale).rounded(), height: (grid.bounds.height * scale).rounded())
                let have = grid.drawableSize
                print(String(
                    format: "resize: %d steps in %.2f s, %d drawable sizes, %d refreshes, %d frames drawn, drawable %.0fx%.0f for %.0fx%.0f",
                    steps, elapsed, r, shown, made, have.width, have.height, want.width, want.height))
                // At most one size per 100 ms, plus the first and the settled one.
                if Double(r) > elapsed / 0.1 + 2 {
                    problems.append("the drawable was resized \(r) times in \(String(format: "%.2f", elapsed)) s")
                }
                if have != want { problems.append("the drawable did not settle at the view's size") }
                if made < shown / 2 { problems.append("the grid stopped drawing during the resize") }
                nsWindow.setFrame(start, display: true)
                after(0.3) { [self] in focusStep() }
            }
        }
        advance(1)
    }

    private func focusStep() {
        guard let model = app.model, let window else { finish(problems) }
        guard let leaf = window.state.tree.leaves.first(where: { $0.camera != nil }), let camera = leaf.camera else {
            finish(problems + ["no camera to focus"])
        }
        window.stage.focus(leaf.path)
        // The animation, then the switch after the main stream's first keyframe.
        after(StageView.focusDuration + 4) { [self] in
            let tile = model.leases.tile(for: camera)
            let tier = tile?.tierState
            print("focused \(model.name(of: camera)): showing \(tier?.shown.name ?? "nothing"), requested \(tier?.requested.name ?? "-")")
            if model.fake != nil, tier?.shown != .main { problems.append("focus did not switch to the main stream") }
            capture("focused") { [self] in listenStep(camera) }
        }
    }

    private func listenStep(_ camera: CameraRef) {
        guard let model = app.model else { finish(problems) }
        model.toggleListening(camera, additive: false)
        after(0.5) { [self] in
            let level = model.level(camera)
            print("listening: \(model.audible.contains(camera)), level \(level.map { String(format: "%.2f", $0.peak) } ?? "n/a")")
            if !model.audible.contains(camera) { problems.append("the camera did not become audible") }
            if model.fake != nil, (level?.peak ?? 0) <= 0 { problems.append("no audio level while listening") }
            model.stopListening()
            unfocusStep(camera)
        }
    }

    private func unfocusStep(_ camera: CameraRef) {
        guard let model = app.model, let window else { finish(problems) }
        window.stage.unfocus()
        after(StageView.focusDuration + 4) { [self] in
            let tier = model.leases.tile(for: camera)?.tierState
            print("after focus: showing \(tier?.shown.name ?? "nothing")")
            if model.fake != nil, tier?.requested != .sub { problems.append("leaving focus did not ask for the sub-stream") }
            if window.stage.isFocused { problems.append("still focused") }
            for (what, ns) in model.timings.sorted(by: { $0.key < $1.key }) {
                print(String(format: "timing %@: %.1f ms", what, Double(ns) / 1e6))
            }
            if let fake = model.fake {
                problems += fake.violations.map { "core contract: \($0)" }
            }
            sidebarStep()
        }
    }

    /// Rows inserted and removed in two places of the sidebar in one update:
    /// what made a table-backed sidebar re-enter its row-height cache.
    private func sidebarStep() {
        guard let model = app.model else { finish(problems) }
        // Only with `--fake`: never touch the user's stored groups.
        guard model.fake != nil else { return checkLog() }
        let cameras = model.allCameras
        let group = model.createGroup(name: "Self-Test", cameras: Array(cameras.prefix(2)))
        let layout = model.createLayout(name: "Self-Test", root: LayoutNode.balanced(Array(cameras.prefix(2))))
        after(0.5) { [self] in
            model.deleteGroup(group)
            if let layout { model.deleteLayout(layout) }
            after(0.5) { [self] in checkLog() }
        }
    }

    private func checkLog() {
        if let watch = app.logWatch {
            for p in watch.patterns {
                let n = watch.count(p)
                print("AppKit warning \"\(p)\": \(n)")
                if n > 0 { problems.append("AppKit logged \"\(p)\" \(n) times") }
            }
        } else {
            problems.append("could not watch standard error")
        }
        finish(problems)
    }

    private func capture(_ what: String, then next: @escaping @MainActor () -> Void) {
        guard let window else { finish(problems + ["no window"]) }
        print("\(what): display refreshes \(window.stage.grid.refreshes), frames drawn \(window.stage.grid.framesDrawn)")
        var done = false
        window.stage.grid.captureNextFrame { [self] image in
            MainActor.assumeIsolated {
                done = true
                let spread = Self.spread(image)
                print(String(format: "%@ frame %dx%d, spread %.1f", what, image.width, image.height, spread))
                if spread < 8 { problems.append("the \(what) picture is flat") }
                next()
            }
        }
        after(5) { [self] in
            if !done {
                print("\(what): display refreshes \(window.stage.grid.refreshes), frames drawn \(window.stage.grid.framesDrawn)")
                finish(problems + ["no \(what) frame was drawn"])
            }
        }
    }

    private static func spread(_ image: RenderedImage) -> Double {
        var sum = 0.0, sumSq = 0.0, n = 0.0
        for y in stride(from: 0, to: image.height, by: 4) {
            for x in stride(from: 0, to: image.width, by: 4) {
                let v = Double(image.rgb(x: x, y: y).wrappedSum()) / 3
                sum += v
                sumSq += v * v
                n += 1
            }
        }
        let mean = sum / max(n, 1)
        return max(sumSq / max(n, 1) - mean * mean, 0).squareRoot()
    }

    private func finish(_ problems: [String]) -> Never {
        app.model?.shutdown()
        if problems.isEmpty {
            print("selftest: PASS")
            exit(0)
        }
        for p in problems { print("  \(p)") }
        print("selftest: FAIL")
        exit(1)
    }
}
