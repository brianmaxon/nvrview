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
/// 12. with `--fake`, a doorbell press (`visitor` alarm) while another tile
///    is focused opens the doorbell's inset as a call (drawn by the same
///    grid; muted with its Unmute control, as nothing was playing); its
///    tapped audio reaches the transcript; Talk without the permission is
///    refused and sends nothing, with it the microphone's audio and a typed
///    reply go out; a click swaps it with the focused camera, Tab swaps
///    back, Esc dismisses it and ends the call; a doorbell inset closes by
///    itself a minute after the press; a smoke alarm's inset stays until
///    dismissed, and dismissing it dismisses its alert. The microphone,
///    speech recognition and text-to-speech are fakes: no permission
///    prompt, ever;
/// 13. with `--fake` and the fisheye fixture among the files, a fisheye
///    camera put in a tile finds its image circle (within 1% of the
///    fixture's), each fisheye view draws through the dewarp pipeline with
///    its pixels matching the fixture's colors where the reference mapping
///    says, and a drag, a wheel step (eased) and 0 move and reset the
///    virtual camera;
/// 14. AppKit logged none of the warnings in `forbiddenLog` during the run.
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
        app.model?.tickCalls()
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
        // The renderer as the stage lays it out now (after the focus steps,
        // a window behind others may not have had a refresh yet).
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
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
                stage.alertActive = { [weak model] in model?.alerts.isActive($0) == true }
                insetStep()
            }
        }
    }

    // MARK: Alert insets and doorbell calls

    private func insetStep() {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        let stage = window.stage
        guard let device = model.runtime("fake-1"), let id = device.deviceID else {
            problems.append("no fake device for the doorbell")
            return checkLog()
        }
        let doorbell = CameraRef(device: device.key, channel: 3)
        guard let other = window.state.tree.leaves.first(where: { $0.camera != nil && $0.camera != doorbell }),
            let otherCamera = other.camera
        else {
            problems.append("no other camera to focus")
            return checkLog()
        }
        model.stopListening()
        stage.focus(other.path)
        advance(StageView.focusDuration)
        let startsBefore = fake.talkStarts
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ stage.insetBoard.item(for: doorbell) != nil && model.calls.call(for: doorbell) != nil }, timeout: 3) { [self] in
            guard stage.insetBoard.item(for: doorbell) != nil, let call = model.calls.call(for: doorbell) else {
                problems.append("a doorbell press did not open its inset and call")
                return checkLog()
            }
            let views = stage.shownInsetViews
            print("doorbell: inset \(views.count), unmute shown \(views.first?.unmuteShown == true), talk \(call.talk.text)")
            if views.count != 1 { problems.append("expected one inset view, saw \(views.count)") }
            if !call.showsUnmute || views.first?.unmuteShown != true {
                problems.append("the doorbell call did not show Unmute while nothing was playing")
            }
            if model.audible.contains(doorbell) { problems.append("the doorbell became audible without listening or the setting") }
            if fake.talkStarts != startsBefore { problems.append("talk-back started on its own") }
            // The inset is one more tile of the same grid, drawn on top.
            let grid = stage.grid
            grid.prepare?(grid.renderer, grid.drawableSize)
            // In focus the grid draws the focused tile; the inset adds one.
            if grid.renderer.tiles.count != 2 {
                problems.append("the grid draws \(grid.renderer.tiles.count) tiles for the focused tile and one inset")
            }
            guard let tile = model.leases.tile(for: doorbell) else {
                problems.append("the doorbell has no tile")
                return checkLog()
            }
            if grid.renderer.tiles.last.map({ ObjectIdentifier($0.picture as AnyObject) }) != ObjectIdentifier(tile) {
                problems.append("the inset is not drawn last (on top)")
            }
            // Its tapped audio reaches the visitor's transcript (fake engine).
            fake.pumpAudio(tile: tile.id, count: 17)
            when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
                let lines = model.calls.call(for: doorbell)?.transcript.lines ?? []
                print("transcript: \(lines.map { "\($0.source.label): \($0.text)" })")
                if lines.first?.source != .visitor { problems.append("the doorbell's audio did not reach the transcript") }
                talkStep(doorbell, otherCamera, startsBefore)
            }
        }
    }

    private func talkStep(_ doorbell: CameraRef, _ otherCamera: CameraRef, _ startsBefore: Int) {
        guard let model = app.model, let fake = model.fake, let tile = model.leases.tile(for: doorbell) else {
            return checkLog()
        }
        // Without the permission: refused, nothing sent.
        model.calls.startTalking(doorbell)
        let refused = model.calls.call(for: doorbell)?.talk.phase
        print("talk without permission: \(model.calls.call(for: doorbell)?.talk.text ?? "-")")
        if refused != .refused(reason: "not_permitted", retryMilliseconds: nil) {
            problems.append("talk-back without the permission was not refused (\(String(describing: refused)))")
        }
        if fake.talkStarts != startsBefore { problems.append("a refused talk reached the device") }
        model.calls.stopTalking(doorbell)
        // With it: the microphone's audio goes out while Talk is held.
        model.setAllowTalk(doorbell, true)
        model.calls.startTalking(doorbell)
        when({ model.calls.call(for: doorbell)?.talk.isActive == true }, timeout: 2) { [self] in
            model.calls.fakeMicrophone?.speak(count: 16_000)
            let sent = fake.talkPushedMilliseconds(tile: tile.id)
            print(String(format: "talk with permission: %@, %.0f ms pushed", model.calls.call(for: doorbell)?.talk.text ?? "-", sent))
            if model.calls.call(for: doorbell)?.talk.isActive != true { problems.append("talk-back did not become active") }
            if abs(sent - 1000) > 1 { problems.append("the microphone's audio was not pushed (\(sent) ms)") }
            model.calls.stopTalking(doorbell)
            when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 2) { [self] in
                // A typed reply: rendered, pushed once active, drained.
                model.calls.sendReply("Hello, one moment please", to: doorbell)
                when({
                    if case .stopped(let r)? = model.calls.call(for: doorbell)?.talk.phase { return r == "finished" }
                    return false
                }, timeout: 3) { [self] in
                    let phase = model.calls.call(for: doorbell)?.talk.phase
                    print("typed reply: \(model.calls.fakeSpeech?.rendered ?? []), talk \(String(describing: phase))")
                    if model.calls.fakeSpeech?.rendered != ["Hello, one moment please"] {
                        problems.append("the typed reply was not rendered")
                    }
                    if phase != .stopped(reason: "finished") { problems.append("the typed reply's talk did not drain") }
                    model.setAllowTalk(doorbell, false)
                    swapStep(doorbell, otherCamera)
                }
            }
        }
    }

    private func swapStep(_ doorbell: CameraRef, _ otherCamera: CameraRef) {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        model.calls.unmute(doorbell)
        if !model.audible.contains(doorbell) { problems.append("Unmute did not make the doorbell audible") }
        // A click swaps: the doorbell fills the stage, the focused camera
        // goes into the return inset.
        stage.swapInset(doorbell)
        let swapped = stage.insetBoard.swap?.shown == doorbell
            && stage.insetBoard.items.first.map { $0.isReturn && $0.camera == otherCamera } == true
        print("swap: doorbell on the stage \(swapped), wants \(model.leases.wantedTier(doorbell).name)")
        if !swapped { problems.append("clicking the inset did not swap it with the focused camera") }
        if model.leases.wantedTier(doorbell) != .main { problems.append("the swapped-in doorbell did not ask for the main stream") }
        if !stage.showsCamera(doorbell) || stage.showsCamera(otherCamera) {
            problems.append("after the swap the stage does not show the doorbell")
        }
        press("\t", keyCode: ViewerKeys.tab)
        let back = stage.insetBoard.swap == nil && stage.insetBoard.item(for: doorbell) != nil
        print("tab: swapped back \(back)")
        if !back { problems.append("Tab did not swap back") }
        press("\u{1b}", keyCode: ViewerKeys.escape)
        let gone = stage.insetBoard.isEmpty && model.calls.call(for: doorbell) == nil
        print("esc: inset dismissed and call ended \(gone), audible \(model.audible.count)")
        if !gone {
            problems.append("Esc did not dismiss the inset and end the call: \(stage.insetBoard.items.map { "\($0.camera) \($0.kind)" })")
        }
        if !model.audible.isEmpty { problems.append("ending the call did not restore what played before") }
        if window.state.focused == nil { problems.append("Esc on an inset also left focus") }
        autoCloseStep(doorbell)
    }

    /// A doorbell inset closes by itself a minute after the press ended.
    private func autoCloseStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let id = model.runtime("fake-1")?.deviceID else {
            return checkLog()
        }
        let stage = window.stage
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: false, alarm: true)
        when({ stage.insetBoard.item(for: doorbell)?.active == false }, timeout: 3) { [self] in
            advance(InsetBoard.autoCloseDelay - 1)
            let stayed = stage.insetBoard.item(for: doorbell) != nil
            advance(2)
            let closed = stage.insetBoard.item(for: doorbell) == nil
            print("doorbell inset: open after 59 s \(stayed), closed after 61 s \(closed), call ended \(model.calls.call(for: doorbell) == nil)")
            if !stayed || !closed { problems.append("the doorbell inset did not close a minute after the press") }
            if model.calls.call(for: doorbell) != nil { problems.append("the call did not end a minute after the last activity") }
            smokeStep()
        }
    }

    /// A smoke alarm's inset stays until dismissed; dismissing it dismisses
    /// the alert.
    private func smokeStep() {
        guard let model = app.model, let window, let fake = model.fake, let device = model.runtime("fake-1"),
            let id = device.deviceID
        else { return checkLog() }
        let stage = window.stage
        let hall = CameraRef(device: device.key, channel: 4)
        fake.emitSoundDetection(device: id, channel: 4, start: true)
        fake.emitSoundDetection(device: id, channel: 4, start: false)
        when({ stage.insetBoard.item(for: hall)?.active == false }, timeout: 3) { [self] in
            let critical = stage.insetBoard.item(for: hall)?.kind.isCritical == true
            advance(InsetBoard.lostStopAfter + InsetBoard.autoCloseDelay + 60)
            let stays = stage.insetBoard.item(for: hall) != nil
            print("smoke inset: critical \(critical), still open after 4 min \(stays), alert \(model.alerts.isActive(hall))")
            if !critical || !stays { problems.append("the smoke alarm's inset did not stay") }
            press("\u{1b}", keyCode: ViewerKeys.escape)
            print("smoke inset dismissed \(stage.insetBoard.isEmpty), alert dismissed \(!model.alerts.isActive(hall))")
            if !stage.insetBoard.isEmpty { problems.append("Esc did not dismiss the smoke alarm's inset") }
            if model.alerts.isActive(hall) { problems.append("dismissing the smoke inset did not dismiss its alert") }
            stage.unfocus()
            advance(StageView.focusDuration)
            fisheyeStep()
        }
    }

    // MARK: Fisheye (docs/UX.md "Fisheye cameras")

    /// The synthetic fisheye fixture's colors by azimuth quadrant, ceiling
    /// mount (fixtures/synthetic/make-fisheye.sh): 0-90° red, 90-180° green,
    /// 180-270° blue, 270-360° yellow; 15° checker cells, a white dot at
    /// tilt 20°. Nil when a pixel is none of them clearly.
    private static func fixtureSector(_ c: SIMD3<Int>) -> Int? {
        let (r, g, b) = (c.x, c.y, c.z)
        if r > g + 40 && r > b + 40 { return 0 }
        if g > r + 30 && g > b + 30 { return 1 }
        if b > r + 40 && b > g + 40 { return 2 }
        if r > b + 50 && g > b + 50 { return 3 }
        return nil
    }

    /// With `--fake` and the fisheye fixture: a fisheye camera put in a tile
    /// finds its circle, and every view draws through the dewarp pipeline
    /// with pixels matching the fixture's colors where the reference mapping
    /// says they should; a drag and a wheel step move the virtual camera
    /// (eased on the clock), saved with the layout; 0 resets it.
    private func fisheyeStep() {
        guard let model = app.model, let window else { finish(problems) }
        guard let camera = model.allCameras.first(where: { model.isFisheye($0) }) else {
            problems.append("no fisheye camera (run with fixtures/synthetic/fisheye/h264-fisheye.h264 among the files)")
            return checkLog()
        }
        let stage = window.stage
        guard let path = window.state.tree.frames(in: stage.bounds).leaves.first?.path else {
            problems.append("no tile for the fisheye camera")
            return checkLog()
        }
        stage.assign(camera, at: path)
        when({ model.leases.tile(for: camera)?.latestPicture() != nil }, timeout: 3) { [self] in
            let grid = stage.grid
            // The circle is measured from the first pictures, on the clock.
            stage.setFisheyeMode(.panorama, at: path)
            for _ in 0..<FisheyeCircleTracker.maxTries where stage.fisheyeDewarp(of: path) == nil {
                grid.prepare?(grid.renderer, grid.drawableSize)
                advance(FisheyeCircleTracker.interval)
            }
            guard let lens = stage.fisheyeDewarp(of: path)?.lens else {
                problems.append("the fisheye tile is not dewarped")
                return checkLog()
            }
            let c = lens.circle
            print(String(format: "fisheye circle: center %.4f, %.4f radius %.4f (fixture 0.49, 0.505, 0.46)", c.cx, c.cy, c.r))
            if abs(c.cx - 0.49) > 0.01 || abs(c.cy - 0.505) > 0.01 || abs(c.r - 0.46) > 0.01 {
                problems.append("the fisheye circle is more than 1% off the fixture's")
            }
            for mode in FisheyeViewMode.allCases where mode.dewarps {
                stage.setFisheyeMode(mode, at: path)
                checkFisheyePixels(path, mode)
            }
            // Virtual PTZ: a drag turns it, a wheel step zooms it (eased).
            let size = window.state.tree.frames(in: stage.bounds).leaf(path)?.rect.size ?? .zero
            stage.turnFisheye(path, by: CGVector(dx: size.width / 4, dy: 0), size: size)
            stage.zoomFisheye(path, by: 2, eased: true)
            advance(0.03)
            let midway = window.state.tree[path]?.fisheye.aim?.fov ?? 0
            advance(1)
            let aim = window.state.tree[path]?.fisheye.aim
            print(String(format: "virtual PTZ: pan %.1f, field of view %.1f after a second (%.1f midway)",
                aim?.pan ?? .nan, aim?.fov ?? .nan, midway))
            if aim.map({ abs($0.pan + 22.5) < 0.01 }) != true { problems.append("a drag did not turn the virtual camera") }
            if aim?.fov != 45 || stage.isEasingFisheye || !(midway > 45 && midway < 90) {
                problems.append("the virtual camera did not ease its zoom to 45°")
            }
            checkFisheyePixels(path, .ptz)
            stage.resetZoom(at: path)
            if window.state.tree[path]?.fisheye.aim != nil { problems.append("0 did not reset the virtual camera") }
            stage.setFisheyeMode(.original, at: path)
            grid.prepare?(grid.renderer, grid.drawableSize)
            if stage.fisheyeDewarp(of: path) != nil { problems.append("Original still dewarps") }
            checkLog()
        }
    }

    /// Draws the window offscreen and compares the fisheye tile's pixels
    /// with the fixture's color at the direction each one looks along.
    private func checkFisheyePixels(_ path: NodePath, _ mode: FisheyeViewMode) {
        guard let window else { return }
        let stage = window.stage
        let grid = stage.grid
        grid.prepare?(grid.renderer, grid.drawableSize)
        guard let dewarp = stage.fisheyeDewarp(of: path), let rect = stage.renderedDestination(of: path),
            let image = grid.renderer.renderOffscreen(width: Int(grid.drawableSize.width), height: Int(grid.drawableSize.height))
        else {
            problems.append("\(mode.title): not drawn through the dewarp pipeline")
            return
        }
        let drawn = grid.renderer.dewarpedTilesDrawn
        var checked = 0, matched = 0
        let step = max(rect.width, rect.height) / 40
        for y in stride(from: rect.minY + 1, to: rect.maxY - 1, by: step) {
            for x in stride(from: rect.minX + 1, to: rect.maxX - 1, by: step) {
                let t = CGPoint(x: (x + 0.5 - rect.minX) / rect.width, y: (y + 0.5 - rect.minY) / rect.height)
                guard let d = dewarp.direction(atTile: t), dewarp.picturePoint(atTile: t) != nil else { continue }
                let (pan, tilt) = panTilt(of: d)
                let psi = (pan + 360).truncatingRemainder(dividingBy: 360)
                func clear(_ a: Double) -> Bool {
                    let m = a.truncatingRemainder(dividingBy: 15)
                    return m > 3 && m < 12
                }
                guard tilt > 5, tilt < 80, abs(tilt - 20) > 7, clear(psi), clear(tilt) else { continue }
                checked += 1
                if Self.fixtureSector(image.rgb(x: Int(x), y: Int(y))) == Int(psi / 90) % 4 { matched += 1 }
            }
        }
        print("fisheye \(mode.title): dewarped tiles drawn \(drawn), pixels matching the fixture \(matched) of \(checked)")
        if drawn < 1 { problems.append("\(mode.title): the dewarp pipeline drew nothing") }
        if checked < 20 || Double(matched) < 0.9 * Double(checked) {
            problems.append("\(mode.title): \(matched) of \(checked) pixels match the fixture")
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
