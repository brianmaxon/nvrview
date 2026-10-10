import AppKit
import QuartzCore
import Foundation
import NVRCore
import VideoKit

/// `--selftest SECONDS`: runs the real app (its window behind all others, no
/// focus, muted), then checks, in order:
///
/// 1. a device is connected, a tile is playing, and a non-flat picture is on
///    screen (once every tile plays and its rates are measured, or after
///    SECONDS at most);
/// 2. resizing the window continuously keeps the grid drawing and
///    resizes its drawable only now and then, then to the exact size;
/// 3. focusing a tile animates it to the whole stage and then switches its
///    camera to the main stream (with `--fake`, whose core models the tier
///    switch), still showing a picture;
/// 4. a muted camera reads a level (for its hover meter); listening makes
///    it audible and its level meter reads a level;
/// 5. leaving focus asks for the sub-stream again;
/// 6. with `--fake`, the fake core saw no contract violation;
/// 7. the sidebar took rows added and removed in several places in one
///    update (a group and a layout at once);
/// 8. with `--fake`, a second lens plays as its own tile on its grid tier;
/// 9. keys sent through the window's event path move the selection (right
///    arrow), focus and unfocus (Space) and reset the zoom (0);
/// 10. a mouse-wheel zoom step eases to 2x (Auto then asks for the main
///    stream) and the renderer samples exactly the zoomed region (the
///    destination unchanged);
/// 11. with full-screen chrome behavior on (simulated: the window stays
///    behind and never enters full screen), the headers, outline and zoom
///    map hide after the idle delay without resizing the drawable, a tile
///    with an active alert keeps its header, an interaction brings the
///    chrome back, and 0 eases the zoom back to exactly 1x (and the
///    sub-stream);
/// 12. AppKit logged none of the warnings in `forbiddenLog` during the run.
///
/// The viewer's clock is a manual one here (`AppDelegate.testClock`):
/// animations, easing and the idle delay finish when the test advances it,
/// not after real time. What really takes time (frames decoding, tier
/// switches, display refreshes, drawable resizes) is waited for by polling
/// for the condition, with a timeout.
///
/// Prints what it measured and exits 0 or 1.
@MainActor
final class SelfTest {
    private unowned let app: AppDelegate
    private var problems: [String] = []
    private let began = CACurrentMediaTime()

    init(app: AppDelegate) { self.app = app }

    /// Lines AppKit must never log (it says the first will become an assert;
    /// the second is a layout whose constraints cannot all hold, such as a
    /// stack view sized to zero).
    static let forbiddenLog = [
        "reentrant operation in its NSTableView delegate",
        "Unable to simultaneously satisfy constraints",
    ]

    func start(seconds: Double) {
        // Line by line, also into a pipe (a run that hangs still shows how far it got).
        setvbuf(stdout, nil, _IOLBF, 0)
        when({ [self] in gridReady }, timeout: seconds) { [self] in checkGrid() }
        // A hard limit for the whole run (the steps after the grid take a few
        // seconds: tier switches and display refreshes are real).
        after(seconds + 30) { [self] in finish(problems + ["timed out"]) }
    }

    /// Advances the viewer's clock and steps every window at the new time
    /// (what the display link does on a refresh).
    private func advance(_ seconds: Double) {
        guard let clock = app.testClock else { return }
        clock.advance(by: seconds)
        for w in app.windows {
            w.stage.tick()
            w.sidebarOverlay.tick()
        }
    }

    /// Every device connected, every tile playing, and the first tile's
    /// device and shown rates measured.
    private var gridReady: Bool {
        guard let model = app.model, let window, !model.devices.isEmpty else { return false }
        guard model.devices.allSatisfy({ $0.link.state == .connected }) else { return false }
        let cameras = window.state.tree.cameras
        guard !cameras.isEmpty,
            cameras.allSatisfy({ model.leases.tile(for: $0)?.snapshot.status == .playing }),
            let first = cameras.first, let tile = model.leases.tile(for: first)
        else { return false }
        if model.fake != nil, abs((tile.deviceRate ?? 0) - 20) > 1 { return false }
        return window.stage.shownRate(of: first) != nil
    }

    private func after(_ seconds: Double, _ body: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated { body() } }
    }

    private var window: ViewerWindowController? { app.windows.first }

    /// Runs `body` once `condition` holds or `timeout` seconds have passed
    /// (a window behind others gets few display refreshes, and animations
    /// advance only on refreshes).
    private func when(
        _ condition: @escaping @MainActor () -> Bool, timeout: Double, _ body: @escaping @MainActor () -> Void
    ) {
        let deadline = CACurrentMediaTime() + timeout
        func poll() {
            if condition() || CACurrentMediaTime() >= deadline { return body() }
            after(0.02) { poll() }
        }
        poll()
    }

    private func checkGrid() {
        guard let model = app.model, let window else { finish(["no window"]) }
        let connected = model.devices.filter { $0.link.state == .connected }.count
        print(String(format: "grid after %.2f s", CACurrentMediaTime() - began))
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
            // Until the drawable settles at the view's size (it follows on
            // display refreshes).
            when({ grid.drawableSize == Self.pixelSize(of: grid) }, timeout: 2) { [self] in
                let elapsed = CACurrentMediaTime() - began
                let r = grid.drawableResizes - resizes
                let shown = grid.refreshes - refreshes, made = grid.framesDrawn - drawn
                let want = Self.pixelSize(of: grid)
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
                when({ grid.drawableSize == Self.pixelSize(of: grid) }, timeout: 2) { [self] in focusStep() }
            }
        }
        advance(1)
    }

    /// The drawable size a view's bounds want, in pixels.
    private static func pixelSize(of view: NSView) -> CGSize {
        let scale = view.window?.backingScaleFactor ?? 1
        return CGSize(width: (view.bounds.width * scale).rounded(), height: (view.bounds.height * scale).rounded())
    }

    private func focusStep() {
        guard let model = app.model, let window else { finish(problems) }
        guard let leaf = window.state.tree.leaves.first(where: { $0.camera != nil }), let camera = leaf.camera else {
            finish(problems + ["no camera to focus"])
        }
        // Replacing the window's content while a tile is focused must only
        // end the focus: the renderer keeps following the stage (it once
        // kept drawing the old arrangement under the new tiles).
        window.stage.focus(leaf.path)
        window.show(window.state.content)
        if window.stage.isFocused { problems.append("focus survived a change of content") }
        if window.stage.grid.prepare == nil { problems.append("a change of content during focus stopped the renderer") }
        window.stage.focus(leaf.path)
        // The animation (on the test clock), then the switch after the main
        // stream's first keyframe (real).
        advance(StageView.focusDuration)
        if window.stage.isAnimatingFocus { problems.append("the focus animation did not finish on time") }
        when({ model.leases.tile(for: camera)?.tierState.shown == .main }, timeout: 5) { [self] in
            let tile = model.leases.tile(for: camera)
            let tier = tile?.tierState
            print("focused \(model.name(of: camera)): showing \(tier?.shown.name ?? "nothing"), requested \(tier?.requested.name ?? "-")")
            if model.fake != nil, tier?.shown != .main { problems.append("focus did not switch to the main stream") }
            capture("focused") { [self] in listenStep(camera) }
        }
    }

    private func listenStep(_ camera: CameraRef) {
        guard let model = app.model else { finish(problems) }
        // A muted camera's meter (on hover) reads its decoded audio too.
        let muted = model.level(camera)
        print("muted level \(muted.map { String(format: "%.2f", $0.peak) } ?? "n/a")")
        if model.fake != nil, (muted?.peak ?? 0) <= 0 { problems.append("no audio level while muted") }
        model.toggleListening(camera, additive: false)
        when({ model.audible.contains(camera) && (model.level(camera)?.peak ?? 0) > 0 }, timeout: 2) { [self] in
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
        // The sub-stream is asked for at once; the animation ends on the clock.
        advance(StageView.focusDuration)
        let requested = model.leases.tile(for: camera)?.tierState.requested
        if model.fake != nil, requested != .sub { problems.append("leaving focus did not ask for the sub-stream") }
        if window.stage.isFocused { problems.append("still focused") }
        // Then the switch back (real), so the quality step starts from the grid's stream.
        when({ model.fake == nil || model.leases.tile(for: camera)?.tierState.shown == .sub }, timeout: 5) { [self] in
            let tier = model.leases.tile(for: camera)?.tierState
            print("after focus: showing \(tier?.shown.name ?? "nothing")")
            if model.fake != nil, tier?.shown != .sub { problems.append("leaving focus did not return to the sub-stream") }
            for (what, ns) in model.timings.sorted(by: { $0.key < $1.key }) {
                print(String(format: "timing %@: %.1f ms", what, Double(ns) / 1e6))
            }
            if let fake = model.fake {
                problems += fake.violations.map { "core contract: \($0)" }
            }
            qualityStep(camera)
        }
    }

    /// Clear on a tile switches its camera to the main stream in the grid;
    /// back to Auto returns it to the sub stream.
    private func qualityStep(_ camera: CameraRef) {
        guard let model = app.model, let window else { finish(problems) }
        guard let leaf = window.state.tree.leaves.first(where: { $0.camera == camera }) else { return sidebarStep() }
        window.stage.setQuality(.clear, at: leaf.path)
        when({ model.leases.tile(for: camera)?.tierState.shown == .main }, timeout: 5) { [self] in
            let tier = model.leases.tile(for: camera)?.tierState
            print("quality Clear: showing \(tier?.shown.name ?? "nothing"), requested \(tier?.requested.name ?? "-")")
            if model.fake != nil, tier?.shown != .main { problems.append("Clear did not switch to the main stream") }
            window.stage.setQuality(.auto, at: leaf.path)
            when({ model.leases.tile(for: camera)?.tierState.shown == .sub }, timeout: 5) { [self] in
                let back = model.leases.tile(for: camera)?.tierState
                print("quality Auto: showing \(back?.shown.name ?? "nothing")")
                if model.fake != nil, back?.shown != .sub { problems.append("Auto did not return to the sub stream") }
                if window.state.tree[leaf.path]?.quality != .auto { problems.append("the quality did not reset") }
                sidebarStep()
            }
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
        // A run-loop turn for the sidebar to take each update.
        after(0.1) { [self] in
            model.deleteGroup(group)
            if let layout { model.deleteLayout(layout) }
            after(0.1) { [self] in revealStep() }
        }
    }

    /// The edge reveal: with the sidebar collapsed, a pointer resting at the
    /// left edge slides the sidebar over the stage without resizing the stage
    /// or its drawable, and it slides away again after the pointer leaves.
    /// The dwell and the hide delay run on the test clock.
    private func revealStep() {
        guard let window else { return lensStep() }
        window.setSidebarCollapsedForTest(true)
        let grid = window.stage.grid
        when({ grid.drawableSize == Self.pixelSize(of: grid) }, timeout: 2) { [self] in
            let size = grid.bounds.size, drawable = grid.drawableSize, resizes = grid.drawableResizes
            let overlay = window.sidebarOverlay
            overlay.simulatedPointer = CGPoint(x: 2, y: 200)
            overlay.pointerAt(CGPoint(x: 2, y: 200))
            advance(overlay.reveal.dwell / 2)
            if overlay.isShown { problems.append("the sidebar slid in before the dwell") }
            advance(overlay.reveal.dwell / 2)
            if !overlay.isShown { problems.append("the sidebar did not slide in at the left edge") }
            // A few real refreshes with the overlay sliding in over the stage.
            after(0.25) { [self] in
                if grid.bounds.size != size || grid.drawableSize != drawable || grid.drawableResizes != resizes {
                    problems.append("the stage or its drawable changed size while the sidebar overlay was up")
                }
                print("reveal: shown, stage \(Int(size.width))x\(Int(size.height)) unchanged")
                overlay.simulatedPointer = CGPoint(x: 900, y: 200)
                overlay.pointerAt(CGPoint(x: 900, y: 200))
                advance(overlay.reveal.hideDelay)
                if overlay.isShown { problems.append("the sidebar overlay did not slide away") }
                overlay.simulatedPointer = nil
                window.setSidebarCollapsedForTest(false)
                when({ grid.drawableSize == Self.pixelSize(of: grid) }, timeout: 2) { [self] in lensStep() }
            }
        }
    }

    /// With `--fake` (one made-up channel has two lenses): the second lens
    /// streams as its own tile on the grid's lens-2 tier.
    private func lensStep() {
        guard let model = app.model else { finish(problems) }
        guard let lens = model.allCameras.first(where: { $0.lens == 1 }) else {
            problems.append("no second lens among the cameras")
            return checkLog()
        }
        model.leases.acquire(lens)
        when({ model.leases.tile(for: lens).map { $0.tierState.shown == .mobile && $0.latestPicture() != nil } == true },
            timeout: 3)
        { [self] in
            let tile = model.leases.tile(for: lens)
            print("\(model.name(of: lens)): showing \(tile?.tierState.shown.name ?? "nothing"), picture \(tile?.latestPicture() != nil)")
            if tile?.tierState.shown != .mobile || tile?.latestPicture() == nil {
                problems.append("the second lens is not playing on its grid tier")
            }
            model.leases.release(lens)
            keyStep()
        }
    }

    /// A key press as AppKit delivers it to the window (the app stays
    /// inactive and the window behind; events are addressed by number).
    private func press(_ characters: String, keyCode: UInt16) {
        guard let nsWindow = window?.window else { return }
        for type in [NSEvent.EventType.keyDown, .keyUp] {
            guard
                let e = NSEvent.keyEvent(
                    with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                    windowNumber: nsWindow.windowNumber, context: nil, characters: characters,
                    charactersIgnoringModifiers: characters, isARepeat: false, keyCode: keyCode)
            else { continue }
            NSApp.sendEvent(e)
        }
    }

    private func keyStep() {
        guard let window else { finish(problems) }
        let stage = window.stage
        let frames = window.state.tree.frames(in: stage.bounds)
        guard let first = frames.leaves.first?.path, let right = frames.neighbor(of: first, .right) else {
            problems.append("no two tiles side by side for the keys")
            return checkLog()
        }
        window.window?.makeFirstResponder(stage)
        window.state.selected = first
        press(String(Character(UnicodeScalar(NSRightArrowFunctionKey)!)), keyCode: ViewerKeys.rightArrow)
        let moved = window.state.selected
        print("key right arrow: selection \(moved == right ? "moved right" : "did not move")")
        if moved != right { problems.append("the right arrow did not move the selection") }
        press(" ", keyCode: ViewerKeys.space)
        advance(StageView.focusDuration)
        let focused = window.state.focused
        print("key space: \(focused == right && !stage.isAnimatingFocus ? "focused" : "not focused")")
        if focused != right || stage.isAnimatingFocus { problems.append("Space did not focus the selected tile") }
        press(" ", keyCode: ViewerKeys.space)
        advance(StageView.focusDuration)
        print("key space again: \(window.state.focused == nil ? "back to the grid" : "still focused")")
        if window.state.focused != nil { problems.append("Space did not leave focus") }
        zoomStep(right)
    }

    private func zoomStep(_ path: NodePath) {
        guard let model = app.model, let window else { finish(problems) }
        let stage = window.stage
        guard let leaf = window.state.tree.frames(in: stage.bounds).leaf(path), let camera = leaf.camera else {
            problems.append("no camera to zoom")
            return checkLog()
        }
        let before = stage.renderedSource(of: path)
        // One mouse-wheel step's worth, eased over the next refreshes.
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: true)
        advance(1)
        if stage.isEasingZoom { problems.append("the zoom was still easing after a second") }
        if model.leases.wantedTier(camera) != .main { problems.append("Auto zoomed to 2x did not ask for the main stream") }
        // The renderer takes the zoom in the stage's layout pass, which the
        // display link runs before drawing; run it now rather than wait for
        // a refresh (a window behind others gets about one a second).
        let grid = stage.grid
        grid.prepare?(grid.renderer, grid.drawableSize)
        do {
            let z = stage.zoom(of: camera)
            let source = stage.renderedSource(of: path)
            print(String(format: "zoom: %.2fx, source %@ (was %@)", z.scale, "\(source.map { "\($0)" } ?? "none")",
                "\(before.map { "\($0)" } ?? "none")"))
            if abs(z.scale - 2) > 0.01 { problems.append("the zoom did not ease to 2x") }
            if let before, let source {
                let want = z.source(fit: before, picture: before.size)
                if abs(source.width - want.width) > 0.5 || abs(source.minX - want.minX) > 0.5
                    || abs(source.height - before.height / 2) > 0.5
                {
                    problems.append("the renderer's source \(source) is not the zoomed region \(want)")
                }
            } else {
                problems.append("no source rectangle for the zoomed tile")
            }
            chromeStep(path, camera)
        }
    }

    private func chromeStep(_ path: NodePath, _ camera: CameraRef) {
        guard let model = app.model, let window else { finish(problems) }
        let stage = window.stage
        let grid = stage.grid
        let resizes = grid.drawableResizes
        // Another tile, with an active alert: its header must stay.
        let other = window.state.tree.leaves.first { $0.path != path && $0.camera != nil && $0.camera != camera }
        let alerting = other?.camera
        stage.alertActive = { $0 == alerting }
        stage.hidesPointer = false
        stage.chromeHideDelay = 0.5
        stage.fullScreen = true
        stage.refreshChrome()
        stage.showHeader(path)
        let shownBefore = stage.headerShown(path) == true && stage.zoomMapShown(path) == true
        advance(0.25)
        if !stage.chromeVisible { problems.append("the chrome hid before the idle delay") }
        advance(0.25)
        do {
            let header = stage.headerShown(path) == true
            let map = stage.zoomMapShown(path) == true
            let pinned = other.map { stage.headerShown($0.path) == true }
            print("chrome before idle: header and map \(shownBefore ? "shown" : "not shown")")
            print("chrome after idle: visible \(stage.chromeVisible), header \(header), map \(map), alert header \(pinned.map { "\($0)" } ?? "n/a"), drawable resizes \(grid.drawableResizes - resizes)")
            if !shownBefore { problems.append("the header or zoom map was not shown before the idle delay") }
            if stage.chromeVisible || header || map { problems.append("the chrome did not hide when idle") }
            if pinned == false { problems.append("the header of a tile with an active alert hid") }
            if grid.drawableResizes != resizes { problems.append("hiding the chrome resized the drawable") }
            // 0 resets the selected tile's zoom: select the zoomed one.
            window.state.selected = path
            press("0", keyCode: 29)
            // The key brings the chrome back at once (it may hide again,
            // with the short delay, before the zoom has eased back).
            let back = stage.chromeVisible
            advance(1)
            do {
                let z = stage.zoom(of: camera)
                print("key 0: zoom \(String(format: "%.4f", z.scale))x, chrome back \(back)")
                if z != .none || stage.isEasingZoom { problems.append("0 did not reset the zoom") }
                if !back { problems.append("a key press did not bring the chrome back") }
                if model.leases.wantedTier(camera) != .sub {
                    problems.append("Auto back at 1x did not ask for the sub stream")
                }
                stage.fullScreen = false
                stage.chromeHideDelay = 3
                stage.alertActive = { _ in false }
                checkLog()
            }
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
        print(String(format: "selftest took %.2f s", CACurrentMediaTime() - began))
        if problems.isEmpty {
            print("selftest: PASS")
            exit(0)
        }
        for p in problems { print("  \(p)") }
        print("selftest: FAIL")
        exit(1)
    }
}
