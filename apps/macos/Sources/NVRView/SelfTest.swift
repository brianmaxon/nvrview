import AppKit
import QuartzCore
import Foundation
import FakeCore
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
/// 14. with `--fake`, the add-device screen in Settings (its window behind
///    the others) searches the made-up network when it is shown, the list
///    fills (each device once, even after a restart), marks the devices
///    already added, and choosing a device fills the form's address, port
///    and name (but not for one the viewer cannot add); closing the window
///    stops the search;
/// 14b. with `--fake`, "Start Conversation" opens the doorbell's call with no
///    press (audible, nothing sent by itself, Talk refused without the
///    permission) and ends through the close path;
/// 14c. with `--fake`, a call popped out of two viewer windows' insets into
///    its own window (behind the others, never key): the insets go from
///    both, the window draws exactly the doorbell's tile, the call, its tap,
///    a running "Keep talking" talk and its draft survive popping out and in
///    (no talk stop or start), the leases and the stream demand follow, a
///    viewer window opened meanwhile records the call detached, Pop In
///    brings the insets back on top with a fresh minute in every viewer
///    window (that one too); closing the
///    window, Esc in it and a minute without activity end the call; the
///    remembered frame is reused, and one off every screen falls back to the
///    top right of the active screen; with "Open calls in their own window"
///    on, a ring opens a call window without the focus (the insets kept
///    detached, a joining press keeping it there), a doorbell's Inset
///    choice wins over the setting, and its Window choice opens Start
///    Conversation in a window;
/// 14d. with `--fake`, the inset column dragged through the window's own
///    mouse events: a small move on the picture still swaps, the header
///    drag snaps an axis within 24 pt of an edge, the picture drags past
///    6 pt, the window is saved with the position, resizing keeps the
///    fraction, a restored window puts the column there, Reset Position
///    returns it to the corner, and two insets move as one column that
///    grows away from the nearer horizontal edge;
/// 15. AppKit logged none of the warnings in `forbiddenLog` during the run.
///
/// The viewer's clock is a manual one here (`AppDelegate.testClock`):
/// animations, easing and the idle delay finish when the test advances it,
/// not after real time. What really takes time (frames decoding, tier
/// switches, display refreshes, drawable resizes) is waited for by polling
/// for the condition, with a timeout.
///
/// Every step carries a parity marker (a comment naming a kebab-case ID) above its code, shared with
/// the Windows self-test where it checks the same thing (docs/PARITY.md;
/// `scripts/check-parity.py` compares them).
///
/// Prints what it measured and exits 0 or 1.
@MainActor
final class SelfTest {
    private unowned let app: AppDelegate
    private var problems: [String] = []
    /// Viewer windows a step opened besides the second (closed with it).
    private var extraWindows: [ViewerWindowController] = []
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
        // parity: grid-plays
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
        // parity: resize
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
        // parity: focus-content-change
        // Replacing the window's content while a tile is focused must only
        // end the focus: the renderer keeps following the stage (it once
        // kept drawing the old arrangement under the new tiles).
        window.stage.focus(leaf.path)
        window.show(window.state.content)
        if window.stage.isFocused { problems.append("focus survived a change of content") }
        if window.stage.grid.prepare == nil { problems.append("a change of content during focus stopped the renderer") }
        window.stage.focus(leaf.path)
        // parity: focus-main-stream
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
        // parity: audio-level-muted
        // A muted camera's meter (on hover) reads its decoded audio too.
        let muted = model.level(camera)
        print("muted level \(muted.map { String(format: "%.2f", $0.peak) } ?? "n/a")")
        if model.fake != nil, (muted?.peak ?? 0) <= 0 { problems.append("no audio level while muted") }
        // parity: audio-listen
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
        // parity: unfocus-sub-stream
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
            // parity: core-contract
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
        // parity: quality-clear-auto
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
        // parity: sidebar-rows-update
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
        // parity: sidebar-reveal
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
        // parity: second-lens
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
        // parity: key-selection-right
        window.window?.makeFirstResponder(stage)
        window.state.selected = first
        press(String(Character(UnicodeScalar(NSRightArrowFunctionKey)!)), keyCode: ViewerKeys.rightArrow)
        let moved = window.state.selected
        print("key right arrow: selection \(moved == right ? "moved right" : "did not move")")
        if moved != right { problems.append("the right arrow did not move the selection") }
        // parity: key-space-focus
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
        // parity: zoom-digital
        // One mouse-wheel step's worth, eased over the next refreshes.
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: true)
        advance(1)
        if stage.isEasingZoom { problems.append("the zoom was still easing after a second") }
        // parity: zoom-auto-quality
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
        // parity: fullscreen-chrome-autohide
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
            // parity: zoom-reset-key
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
        // parity: call-press-opens-call
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
            // parity: alert-inset-in-grid
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
            // parity: call-camera-audio-recognized
            // parity: call-captions
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
        // parity: call-talk-permission-prompt
        // Without the permission Talk asks first; Cancel sends nothing, the
        // call stays open, and the camera's option stays off.
        guard let prompter = model.calls.fakePrompter else { return checkLog() }
        prompter.answers = [false]
        model.calls.startTalking(doorbell)
        let name = model.name(of: doorbell)
        print("talk without permission: asked \(prompter.asked.map(\.title)), \(model.calls.call(for: doorbell)?.talk.text ?? "-")")
        if prompter.asked.map(\.title) != ["Allow talk-back for \(name)?"] {
            problems.append("Talk without the permission did not ask to allow talk-back for the camera")
        }
        if model.calls.call(for: doorbell)?.talk.phase != .idle { problems.append("a canceled prompt left talk-back started") }
        if fake.talkStarts != startsBefore { problems.append("talk-back reached the device before the user allowed it") }
        if model.cameraOptions(doorbell).allowTalk { problems.append("Cancel turned talk-back on") }
        if model.calls.call(for: doorbell) == nil { problems.append("Cancel ended the call") }
        model.calls.stopTalking(doorbell)
        // A typed reply asks too; Cancel keeps the text (accepted false) and sends nothing.
        prompter.answers = [false]
        var accepted: Bool?
        model.calls.setDraft(doorbell, "Not yet")
        model.calls.sendReply("Not yet", to: doorbell) { accepted = $0 }
        if accepted != false || prompter.asked.count != 2 { problems.append("a typed reply without the permission did not ask and keep its text") }
        if model.calls.call(for: doorbell)?.draft != "Not yet" { problems.append("a canceled typed reply lost the call's draft") }
        if fake.talkStarts != startsBefore || model.calls.fakeSpeech?.rendered.isEmpty == false {
            problems.append("a canceled typed reply was rendered or sent")
        }
        // parity: call-talk-allowed
        // Allow persists the option and starts the talk without pressing
        // again; the microphone's audio goes out while Talk is held.
        prompter.answers = [true]
        model.calls.startTalking(doorbell)
        if !model.cameraOptions(doorbell).allowTalk { problems.append("Allow did not turn talk-back on for the camera") }
        when({ model.calls.call(for: doorbell)?.talk.isActive == true }, timeout: 2) { [self] in
            model.calls.fakeMicrophone?.speak(count: 16_000)
            let sent = fake.talkPushedMilliseconds(tile: tile.id)
            print(String(format: "talk with permission: %@, %.0f ms pushed", model.calls.call(for: doorbell)?.talk.text ?? "-", sent))
            if model.calls.call(for: doorbell)?.talk.isActive != true { problems.append("talk-back did not become active") }
            if abs(sent - 1000) > 1 { problems.append("the microphone's audio was not pushed (\(sent) ms)") }
            model.calls.stopTalking(doorbell)
            when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 2) { [self] in
                // parity: call-typed-reply
                // A typed reply: rendered, pushed once active, drained.
                model.calls.setDraft(doorbell, "Hello, one moment please")
                model.calls.sendReply("Hello, one moment please", to: doorbell)
                if model.calls.call(for: doorbell)?.draft.isEmpty != true { problems.append("a sent reply stayed in the call's draft") }
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
                    coreRefusalStep(doorbell, otherCamera, startsBefore: fake.talkStarts)
                }
            }
        }
    }

    /// The core still refuses (its permission was revoked behind the app's
    /// back): the same prompt appears; Cancel leaves it refused.
    private func coreRefusalStep(_ doorbell: CameraRef, _ otherCamera: CameraRef, startsBefore: Int) {
        guard let model = app.model, let fake = model.fake, let prompter = model.calls.fakePrompter,
            let id = model.runtime(doorbell.device)?.deviceID
        else { return checkLog() }
        // macOS only: the core's own refusal (its permission revoked behind
        // the app's back) shows the same prompt. Windows re-sends the setting
        // before every start, so it cannot be out of sync there.
        try? model.bridge.setTalkPermission(device: id, channel: UInt8(doorbell.channel), allowed: false)
        let asked = prompter.asked.count
        prompter.answers = [false]
        // With "Keep talking": the latch is the call's, and a refusal turns it off.
        model.calls.setLatched(doorbell, true)
        when({ prompter.asked.count == asked + 1 }, timeout: 2) { [self] in
            print("core refusal: asked again \(prompter.asked.count == asked + 1), \(model.calls.call(for: doorbell)?.talk.text ?? "-")")
            if fake.talkStarts != startsBefore { problems.append("a refused talk reached the device") }
            if model.calls.call(for: doorbell)?.talk.isTalking == true { problems.append("Cancel after a core refusal left talk-back running") }
            if model.calls.call(for: doorbell)?.latched == true { problems.append("Keep talking stayed on after a refusal") }
            model.calls.stopTalking(doorbell)
            model.setAllowTalk(doorbell, false)
            swapStep(doorbell, otherCamera)
        }
    }

    private func swapStep(_ doorbell: CameraRef, _ otherCamera: CameraRef) {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        // parity: call-unmute
        model.calls.unmute(doorbell)
        if !model.audible.contains(doorbell) { problems.append("Unmute did not make the doorbell audible") }
        // parity: alert-inset-swap
        // A click swaps: the doorbell fills the stage, the focused camera
        // goes into the return inset. Clicking a call's inset is call activity.
        let clicked = model.clock.now
        stage.swapInset(doorbell)
        if (model.calls.call(for: doorbell)?.lastActivity ?? 0) < clicked { problems.append("clicking the call's inset did not count as call activity") }
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
        // parity: call-close-ends
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
        // parity: call-auto-close
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
            conversationStep(doorbell)
        }
    }

    /// "Start Conversation" opens the call with no press, sends nothing by
    /// itself, refuses Talk without the permission, and ends through the
    /// same paths.
    private func conversationStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        let stage = window.stage
        let startsBefore = fake.talkStarts
        // parity: call-conversation-start
        stage.startConversation(doorbell)
        let call = model.calls.call(for: doorbell)
        let opened = stage.insetBoard.item(for: doorbell)?.kind == .conversation
        print("conversation: inset \(opened), presses \(call?.presses ?? -1), audible \(model.audible.contains(doorbell))")
        if !opened || call == nil || call?.presses != 0 || call?.origin != .conversation {
            problems.append("Start Conversation did not open the inset and a call with no press")
        }
        if !model.audible.contains(doorbell) { problems.append("a conversation did not make the camera audible") }
        if fake.talkStarts != startsBefore { problems.append("a conversation sent talk-back by itself") }
        model.calls.startTalking(doorbell)
        if model.calls.fakePrompter?.asked.count != 5 || model.calls.call(for: doorbell)?.talk.phase != .idle {
            problems.append("Talk in a conversation did not ask first (and send nothing on Cancel)")
        }
        if fake.talkStarts != startsBefore { problems.append("a canceled Talk in a conversation reached the device") }
        model.calls.stopTalking(doorbell)
        // parity: call-conversation-end
        stage.endConversation(doorbell)
        let ended = model.calls.call(for: doorbell) == nil && stage.insetBoard.item(for: doorbell) == nil
        print("conversation: ended through the close path \(ended), still audible \(model.audible.contains(doorbell))")
        if !ended { problems.append("ending a conversation left its call or inset open") }
        if model.audible.contains(doorbell) { problems.append("ending a conversation left the camera audible") }
        popOutStep(doorbell)
    }

    // MARK: Calls in their own windows (docs/UX.md "A call in its own window")

    /// A press shown in two viewer windows' insets pops out into a call
    /// window: the insets go from both, the window draws exactly the
    /// doorbell's shared tile, and the call (tap, talk, leases) is untouched.
    private func popOutStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let calls = app.callWindows,
            let id = model.runtime(doorbell.device)?.deviceID,
            let otherCamera = window.state.tree.cameras.first(where: { $0.firstLens != doorbell })
        else { return smokeStep() }
        // A second viewer window that does not show the doorbell either.
        guard let second = app.openWindow(.scratch(name: "Second", root: .leaf(otherCamera))) else {
            problems.append("could not open a second viewer window")
            return smokeStep()
        }
        let stages = [window.stage, second.stage]
        let baseline = model.leases.leaseCount(doorbell)
        // parity: call-popout-open
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ model.calls.call(for: doorbell) != nil && stages.allSatisfy { !$0.shownInsetViews.isEmpty } }, timeout: 3) { [self] in
            guard let before = model.calls.call(for: doorbell), let tile = model.leases.tile(for: doorbell) else {
                problems.append("a press did not open the call in both viewer windows")
                return finishPopOut(second)
            }
            let insetsBefore = stages.map { $0.shownInsetViews.count }
            let leasesBefore = model.leases.leaseCount(doorbell)
            let starts = fake.talkStarts
            let popButtons = stages.allSatisfy { $0.shownInsetViews.first?.popShown == true }
            model.popOutCall(doorbell)
            guard let c = calls.controller(for: doorbell) else {
                problems.append("Pop Out opened no call window")
                return finishPopOut(second)
            }
            let gone = stages.allSatisfy { stage in
                stage.shownInsetViews.isEmpty && stage.insetBoard.detachedCameras == [doorbell]
                    && !stage.insetBoard.shown.contains { $0.camera == doorbell }
            }
            draw(c)
            let drawn = c.grid.renderer.tiles
            let exact = drawn.count == 1 && drawn.first.map { ObjectIdentifier($0.picture as AnyObject) } == ObjectIdentifier(tile)
            let after = model.calls.call(for: doorbell)
            let same = after?.startedAt == before.startedAt && after?.presses == before.presses
                && after?.transcript == before.transcript && after?.presentation == .window
            let tapped = model.calls.isTapped(doorbell) && fake.pumpAudio(tile: tile.id, count: 1) == 1
            let leases = model.leases.leaseCount(doorbell)
            let wantedLeases = leasesBefore - insetsBefore.reduce(0, +) + 1
            let tier = model.leases.wantedTier(doorbell)
            let tiers = model.leases.tiers(for: doorbell)
            print(
                "pop out: insets \(insetsBefore) -> gone \(gone), window draws the doorbell tile \(exact), same call \(same), "
                    + "tapped \(tapped), leases \(leasesBefore) -> \(leases), main \(c.asksForMainStream) (wants \(tier.name))")
            if !popButtons { problems.append("a call's inset did not offer Pop Out in its header") }
            if !gone { problems.append("Pop Out left the call's inset in a viewer window") }
            if !exact { problems.append("the call window does not draw exactly the doorbell's tile (\(drawn.count) tiles)") }
            if !same { problems.append("Pop Out changed the call") }
            if !tapped { problems.append("Pop Out stopped the doorbell's audio tap") }
            if fake.talkStarts != starts { problems.append("Pop Out started talk-back") }
            if leases != wantedLeases || model.leases.tile(for: doorbell) !== tile {
                problems.append("Pop Out leases: \(leases), expected \(wantedLeases) on the same tile")
            }
            if tier != (c.asksForMainStream ? tiers.focus : tiers.grid) {
                problems.append("the call window's stream demand did not reach the leases (\(tier.name))")
            }
            if c.window?.isKeyWindow == true || NSApp.isActive { problems.append("the call window took the focus in the self-test") }
            popOutTalkStep(doorbell, tile: tile, stages: stages, second: second, baseline: baseline)
        }
    }

    /// Lays out a call window and runs one display refresh's preparation
    /// (a window behind the others gets few refreshes).
    private func draw(_ c: CallWindowController) {
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let scale = c.window?.backingScaleFactor ?? 1
        let b = c.grid.bounds
        c.grid.prepare?(c.grid.renderer, CGSize(width: b.width * scale, height: b.height * scale))
    }

    /// Talk with "Keep talking" and a typed draft survives popping in and
    /// out again: no stop, no new start, the latch and the draft shown.
    private func popOutTalkStep(
        _ doorbell: CameraRef, tile: CoreTile, stages: [StageView], second: ViewerWindowController, baseline: Int
    ) {
        guard let model = app.model, let fake = model.fake, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-popout-talk
        model.setAllowTalk(doorbell, true)
        model.calls.setLatched(doorbell, true)
        model.calls.setDraft(doorbell, "Be right there")
        when({ model.calls.call(for: doorbell)?.talk.isActive == true }, timeout: 2) { [self] in
            let starts = fake.talkStarts
            model.popInCall(doorbell)
            let insetsShow = stages.allSatisfy { stage in
                stage.shownInsetViews.first.map { $0.callPanel.keepTalkingShown && $0.callPanel.replyText == "Be right there" } == true
            }
            model.popOutCall(doorbell)
            let panel = calls.controller(for: doorbell)?.inset.callPanel
            let windowShows = panel?.keepTalkingShown == true && panel?.replyText == "Be right there"
            // Any stop or start would arrive as an event: give them a moment.
            after(0.3) { [self] in
                let call = model.calls.call(for: doorbell)
                let kept = call?.talk.isActive == true && fake.isTalking(tile: tile.id) && fake.talkStarts == starts
                print(
                    "pop in and out while talking: talk kept \(kept), latch \(call?.latched == true), draft \(call?.draft ?? "-"), "
                        + "insets show them \(insetsShow), window shows them \(windowShows)")
                if !kept { problems.append("popping in and out stopped or restarted talk-back") }
                if call?.latched != true || call?.draft != "Be right there" { problems.append("popping lost Keep talking or the draft") }
                if !insetsShow { problems.append("the insets did not show Keep talking and the draft after Pop In") }
                if !windowShows { problems.append("the call window did not show Keep talking and the draft") }
                model.calls.setLatched(doorbell, false)
                when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 2) { [self] in
                    model.setAllowTalk(doorbell, false)
                    model.calls.setDraft(doorbell, "")
                    newWindowStep(doorbell, stages: stages, second: second, baseline: baseline)
                }
            }
        }
    }

    /// A viewer window opened while the call is popped out records the
    /// call's item, detached, so Pop In shows the inset there too.
    private func newWindowStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let window,
            let otherCamera = window.state.tree.cameras.first(where: { $0.firstLens != doorbell })
        else { return finishPopOut(second) }
        // parity: call-popin-new-window
        guard let third = app.openWindow(.scratch(name: "Third", root: .leaf(otherCamera))) else {
            problems.append("could not open a third viewer window")
            return popInStep(doorbell, stages: stages, second: second, baseline: baseline)
        }
        extraWindows.append(third)
        when({ third.stage.insetBoard.detachedCameras == [doorbell] }, timeout: 1) { [self] in
            let recorded = third.stage.insetBoard.detachedCameras == [doorbell] && third.stage.shownInsetViews.isEmpty
            print("new window while popped out: call recorded detached \(recorded), leases \(model.leases.leaseCount(doorbell))")
            if !recorded { problems.append("a viewer window opened while the call was popped out has no detached item for it") }
            popInStep(doorbell, stages: stages + [third.stage], second: second, baseline: baseline)
        }
    }

    /// Pop In: the window closes, every viewer window shows the inset again
    /// on top with a fresh minute, and the leases follow.
    private func popInStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-popin
        let panel = calls.controller(for: doorbell)?.window
        let leasesBefore = model.leases.leaseCount(doorbell)
        model.popInCall(doorbell)
        let now = model.clock.now
        let back = stages.allSatisfy { stage in
            stage.insetBoard.shown.first?.camera == doorbell && stage.insetBoard.detachedCameras.isEmpty
                && stage.shownInsetViews.first?.callPanel.isHidden == false
                && stage.insetBoard.item(for: doorbell).flatMap(InsetBoard.closesAt).map { $0 >= now + InsetBoard.autoCloseDelay } == true
        }
        let leases = model.leases.leaseCount(doorbell)
        let tiers = model.leases.tiers(for: doorbell)
        let closed = calls.controller(for: doorbell) == nil && panel?.isVisible != true
        print(
            "pop in: window closed \(closed), insets back on top \(back), "
                + "leases \(leasesBefore) -> \(leases), wants \(model.leases.wantedTier(doorbell).name)")
        if !closed { problems.append("Pop In left the call window open") }
        if !back { problems.append("Pop In did not bring the inset back on top, with a fresh minute, in every viewer window") }
        if leases != leasesBefore - 1 + stages.count {
            problems.append("Pop In leases: \(leases), expected \(leasesBefore - 1 + stages.count)")
        }
        if model.leases.wantedTier(doorbell) != tiers.grid { problems.append("the closed call window's stream demand stayed") }
        popOutEndStep(doorbell, stages: stages, second: second, baseline: baseline)
    }

    /// Closing the call window, Esc in it, and a minute without activity
    /// end the call and close the window; the viewer windows keep nothing.
    private func popOutEndStep(_ doorbell: CameraRef, stages: [StageView], second: ViewerWindowController, baseline: Int) {
        guard let model = app.model, let calls = app.callWindows, let window else { return finishPopOut(second) }
        // parity: call-popout-end
        func ended(_ how: String) {
            let clean = model.calls.call(for: doorbell) == nil && calls.controller(for: doorbell) == nil
                && stages.allSatisfy { $0.insetBoard.items.isEmpty } && model.leases.leaseCount(doorbell) == baseline
            print("call window \(how): call ended and window closed \(clean), leases \(model.leases.leaseCount(doorbell))")
            if !clean { problems.append("\(how) did not end the call, close its window and let its insets and leases go") }
        }
        // The close button (and Cmd-W): performClose.
        model.popOutCall(doorbell)
        let first = calls.controller(for: doorbell)?.window
        first?.performClose(nil)
        if first?.isVisible == true { problems.append("the closed call window is still visible") }
        ended("closed")
        // Esc while the reply field is not being edited.
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        if let panel = calls.controller(for: doorbell)?.window,
            let esc = NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: panel.windowNumber, context: nil, characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}",
                isARepeat: false, keyCode: ViewerKeys.escape)
        {
            panel.sendEvent(esc)
        }
        ended("Esc")
        // A minute without activity, on the manual clock.
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        advance(DoorbellCall.idleTimeout - 1)
        let stayed = calls.controller(for: doorbell) != nil
        advance(2)
        if !stayed { problems.append("the call window closed before a minute without activity") }
        ended("expiry")
        callWindowFrameStep(doorbell, second: second)
    }

    /// The remembered frame is reused; one whose screen is gone falls back
    /// to the top right of the active viewer window's screen.
    private func callWindowFrameStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let calls = app.callWindows, let window,
            let screen = window.window?.screen ?? NSScreen.screens.first
        else { return finishPopOut(second) }
        // parity: call-window-frame
        let visible = CallWindows.topLeft(screen.visibleFrame)
        let remembered = WindowRect(x: visible.x + 40, y: visible.y + 60, width: 400, height: 360)
        calls.placementMode = .remembered
        calls.rememberedFrame = remembered
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        let reused = calls.controller(for: doorbell)?.window.map { CallWindows.topLeft($0.frame) }
        // The user moves it: that frame is remembered.
        if let panel = calls.controller(for: doorbell)?.window {
            panel.setFrameOrigin(NSPoint(x: panel.frame.minX + 30, y: panel.frame.minY - 20))
        }
        let moved = calls.rememberedFrame
        model.calls.end(doorbell, reason: "closed")
        // A frame on a screen that is gone.
        calls.rememberedFrame = WindowRect(x: -100_000, y: -100_000, width: 400, height: 360)
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        let fallback = calls.controller(for: doorbell)?.window.map { CallWindows.topLeft($0.frame) }
        let m = CallWindowPlacement.margin
        let topRight = WindowRect(x: visible.maxX - m - 400, y: visible.minY + m, width: 400, height: 360)
        model.calls.end(doorbell, reason: "closed")
        func near(_ a: WindowRect?, _ b: WindowRect) -> Bool {
            guard let a else { return false }
            return abs(a.x - b.x) < 0.5 && abs(a.y - b.y) < 0.5 && abs(a.width - b.width) < 0.5 && abs(a.height - b.height) < 0.5
        }
        let movedTo = WindowRect(x: remembered.x + 30, y: remembered.y + 20, width: 400, height: 360)
        print(
            "call window frame: remembered reused \(near(reused, remembered)), move remembered \(near(moved, movedTo)), "
                + "gone screen -> top right of the active screen \(near(fallback, topRight))")
        if !near(reused, remembered) {
            problems.append("the call window did not open at the remembered frame: \(String(describing: reused))")
        }
        if !near(moved, movedTo) { problems.append("moving the call window did not remember its frame: \(String(describing: moved))") }
        if !near(fallback, topRight) {
            problems.append("a remembered frame off every screen did not fall back to the top right: \(String(describing: fallback))")
        }
        callDefaultRingStep(doorbell, second: second)
    }

    /// "Open calls in their own window": a ring opens the call in a call
    /// window that does not take the focus, the viewer windows keep its
    /// item detached, a press that joins keeps the presentation, and the
    /// doorbell's own choice (Inset) wins over the setting.
    private func callDefaultRingStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let window, let fake = model.fake, let calls = app.callWindows,
            let id = model.runtime(doorbell.device)?.deviceID
        else { return finishPopOut(second) }
        let stages = [window.stage, second.stage]
        // parity: call-default-ring
        model.callsOpenInWindow = true
        let saved = calls.defaults.bool(forKey: AppModel.callsOpenInWindowKey)
        fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
        when({ calls.controller(for: doorbell) != nil && stages.allSatisfy { !$0.insetBoard.detachedCameras.isEmpty } }, timeout: 3) {
            [self] in
            let call = model.calls.call(for: doorbell)
            let c = calls.controller(for: doorbell)
            let detached = stages.allSatisfy { $0.insetBoard.detachedCameras == [doorbell] && $0.shownInsetViews.isEmpty }
            let quiet = calls.lastOpenActivated == false && c?.window?.isKeyWindow != true && !NSApp.isActive
            // A press that joins keeps the call where it is, whatever the setting says now.
            model.callsOpenInWindow = false
            fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
            when({ model.calls.call(for: doorbell)?.presses == 2 }, timeout: 3) { [self] in
                let joined = model.calls.call(for: doorbell)
                let kept = joined?.presentation == .window && calls.controller(for: doorbell) === c
                    && stages.allSatisfy { $0.shownInsetViews.isEmpty }
                model.calls.end(doorbell, reason: "closed")
                print(
                    "ring with calls in windows: saved \(saved), window \(call?.presentation == .window && c != nil), "
                        + "opened without focus \(quiet), insets detached \(detached), a joining press kept it \(kept)")
                if !saved { problems.append("Open calls in their own window was not saved") }
                if call?.presentation != .window || c == nil { problems.append("with the setting on, a ring did not open a call window") }
                if !quiet { problems.append("a call window opened by a ring took the focus (or asked to)") }
                if !detached { problems.append("a ring into a call window left an inset or no detached item in a viewer window") }
                if !kept { problems.append("a press joining a call in its window moved it") }
                // The doorbell's own choice wins: Inset with the setting on.
                model.callsOpenInWindow = true
                model.setCallPresentation(doorbell, .inset)
                fake.emitDetection(device: id, channel: 3, kind: "visitor", start: true, alarm: true)
                when({ stages.allSatisfy { !$0.shownInsetViews.isEmpty } }, timeout: 3) { [self] in
                    let inset = model.calls.call(for: doorbell)?.presentation == .inset && calls.controller(for: doorbell) == nil
                    print("ring with the doorbell set to Inset: insets in both windows \(inset)")
                    if !inset { problems.append("the doorbell's Inset choice did not win over Open calls in their own window") }
                    for stage in stages { stage.dismissInset(doorbell) }
                    model.callsOpenInWindow = false
                    callDefaultConversationStep(doorbell, second: second)
                }
            }
        }
    }

    /// The doorbell's Window choice opens Start Conversation in a call
    /// window (brought forward: the user asked for it) with the setting off;
    /// Default again leaves the camera's options empty.
    private func callDefaultConversationStep(_ doorbell: CameraRef, second: ViewerWindowController) {
        guard let model = app.model, let window, let calls = app.callWindows else { return finishPopOut(second) }
        // parity: call-default-conversation
        model.setCallPresentation(doorbell, .window)
        let stored = model.cameraOptions(doorbell)
        window.stage.startConversation(doorbell)
        let call = model.calls.call(for: doorbell)
        let opened = call?.presentation == .window && call?.origin == .conversation && calls.controller(for: doorbell) != nil
        let activates = calls.lastOpenActivated == true
        let detached = window.stage.insetBoard.detachedCameras == [doorbell] && window.stage.shownInsetViews.isEmpty
        model.calls.end(doorbell, reason: "closed")
        model.setCallPresentation(doorbell, nil)
        let cleared = model.cameraOptions(doorbell).callWindow == nil && model.cameraOptions(doorbell).isDefault
        // The saved choice reads back; a file without it reads as Default.
        let encoder = JSONEncoder()
        let roundTrip = (try? encoder.encode(stored)).flatMap { try? JSONDecoder().decode(CameraOptions.self, from: $0) }
        let plain = (try? encoder.encode(CameraOptions(camera: doorbell))).flatMap { String(data: $0, encoding: .utf8) }
        let readsBack = roundTrip?.callPresentation == .window && plain?.contains("callWindow") == false
        print(
            "conversation with the doorbell set to Window: call window \(opened), brought forward \(activates), "
                + "inset detached \(detached), Default clears the option \(cleared), saved choice reads back \(readsBack)")
        if !opened { problems.append("the doorbell's Window choice did not open Start Conversation in a call window") }
        if !activates { problems.append("a call window opened by Start Conversation was not brought forward") }
        if !detached { problems.append("Start Conversation into a call window left an inset in the viewer window") }
        if !cleared { problems.append("choosing Default did not clear the camera's call option") }
        if !readsBack { problems.append("the camera's call choice did not save and read back") }
        finishPopOut(second)
    }

    private func finishPopOut(_ second: ViewerWindowController) {
        app.model?.calls.endAll()
        for w in extraWindows { w.close() }
        extraWindows = []
        // The throwaway suite with the frame and pin goes.
        app.callWindows?.defaults.removePersistentDomain(forName: AppDelegate.selfTestDefaults)
        second.close()
        insetDragSnapStep()
    }

    // MARK: Moving the insets (docs/UX.md "Alert insets", "Moving them")

    /// A mouse event as AppKit delivers it to a viewer window (behind the
    /// others, the app inactive), at a point in the stage's inset layer.
    private func mouse(_ type: NSEvent.EventType, _ p: CGPoint, in controller: ViewerWindowController) {
        guard let nsWindow = controller.window else { return }
        let at = controller.stage.insetLayer.convert(p, to: nil)
        guard
            let e = NSEvent.mouseEvent(
                with: type, location: at, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: nsWindow.windowNumber, context: nil, eventNumber: 0, clickCount: 1,
                pressure: type == .leftMouseUp ? 0 : 1)
        else { return }
        nsWindow.sendEvent(e)
    }

    /// Presses at `from`, moves to `to` in a few steps, and lets go.
    private func drag(from: CGPoint, to: CGPoint, in controller: ViewerWindowController, steps: Int = 5) {
        mouse(.leftMouseDown, from, in: controller)
        for i in 1...steps {
            let t = CGFloat(i) / CGFloat(steps)
            mouse(.leftMouseDragged, CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t), in: controller)
        }
        mouse(.leftMouseUp, to, in: controller)
    }

    /// The anchor the column's frame measures as now (unsnapped).
    private static func measuredAnchor(_ column: CGRect, stage: CGRect) -> (x: Double, y: Double) {
        let m = InsetGeometry.margin
        let fx = Double(stage.width) - 2 * m - Double(column.width)
        let fy = Double(stage.height) - 2 * m - Double(column.height)
        return (fx > 0 ? (Double(column.minX) - m) / fx : 0, fy > 0 ? (Double(column.minY) - m) / fy : 0)
    }

    /// A conversation's inset (no press needed) dragged by its header lands
    /// where it was dropped, snapping each axis within 24 pt of an edge; a
    /// small move on the picture is still a click (it swaps); a longer one
    /// drags. The window is saved with the new position.
    private func insetDragSnapStep() {
        guard let model = app.model, let window, let device = model.runtime("fake-1") else { return smokeStep() }
        let stage = window.stage
        let doorbell = CameraRef(device: device.key, channel: 3)
        // parity: inset-drag-snap
        stage.startConversation(doorbell)
        guard let view = stage.shownInsetViews.first, let column = stage.insetColumn else {
            problems.append("no inset to drag")
            return smokeStep()
        }
        let b = stage.insetLayer.bounds
        let atDefault = stage.insetAnchor == nil
            && Self.near(Self.measuredAnchor(column, stage: b), InsetAnchor(corner: model.insetCorner))
        // A small move on the picture: a click, which swaps.
        let picture = view.videoRect.offsetBy(dx: view.frame.minX, dy: view.frame.minY)
        let p = CGPoint(x: picture.midX, y: picture.midY)
        drag(from: p, to: CGPoint(x: p.x + 3, y: p.y + 2), in: window, steps: 2)
        let clickSwapped = stage.insetBoard.swap?.shown == doorbell && stage.insetAnchor == nil
        press("\t", keyCode: ViewerKeys.tab)
        guard let header = stage.shownInsetViews.first?.frame, let start = stage.insetColumn else {
            problems.append("the inset did not come back after swapping back")
            return smokeStep()
        }
        // The header, to 10 pt from the right edge (snaps) and 40% down (does not).
        let m = InsetGeometry.margin
        let freeX = Double(b.width) - 2 * m - Double(start.width)
        let freeY = Double(b.height) - 2 * m - Double(start.height)
        let target = CGPoint(x: m + freeX - 10, y: m + 0.4 * freeY)
        let grab = CGPoint(x: header.minX + 40, y: header.minY + 8)
        let saves = savesCount
        drag(from: grab, to: CGPoint(x: grab.x + target.x - start.minX, y: grab.y + target.y - start.minY), in: window)
        let anchor = stage.insetAnchor
        let snapped = anchor.map { $0.x == 1 && abs($0.y - 0.4) < 0.01 } == true
        let placed = stage.insetColumn.map { abs(Double($0.maxX) - (Double(b.width) - m)) < 0.5 } == true
        let saved = window.savedWindow?.insetAnchor == anchor && anchor != nil
        let noSwap = stage.insetBoard.swap == nil
        // The picture past the click's slop drags too (40 pt left: off the snap).
        guard let moved = stage.shownInsetViews.first else { return smokeStep() }
        let pic = moved.videoRect.offsetBy(dx: moved.frame.minX, dy: moved.frame.minY)
        drag(from: CGPoint(x: pic.midX, y: pic.midY), to: CGPoint(x: pic.midX - 40, y: pic.midY), in: window)
        let pictureDrag = stage.insetAnchor.map { abs($0.x - (freeX - 40) / freeX) < 0.01 && abs($0.y - 0.4) < 0.01 } == true
            && stage.insetBoard.swap == nil
        print(
            "inset drag: default corner \(atDefault) at \(Int(column.minX)),\(Int(column.minY)), small move on the picture swaps "
                + "\(clickSwapped), header drag snaps x \(snapped) (anchor \(anchor.map { "\($0.x), \($0.y)" } ?? "-"), right edge "
                + "\(placed)), no swap \(noSwap), saved with the window \(saved), picture drag past the slop \(pictureDrag)")
        if window.window?.isKeyWindow == true || NSApp.isActive { problems.append("the self-test's mouse events took the focus") }
        if !atDefault { problems.append("the inset column did not start in the default corner") }
        if !clickSwapped { problems.append("a click with a small move on the inset's picture did not swap") }
        if !snapped || !placed { problems.append("dragging the inset's header did not snap to the right edge and keep 40% down") }
        if !noSwap { problems.append("dragging an inset swapped it") }
        if !saved || savesCount == saves { problems.append("the moved column was not saved with the window") }
        if !pictureDrag { problems.append("dragging the inset's picture past 6 pt did not move the column") }
        insetDragResizeStep(doorbell)
    }

    /// How often the windows asked to be saved (they save after a delay).
    private var savesCount: Int { app.windowSaveRequests }

    private static func near(_ a: (x: Double, y: Double), _ b: InsetAnchor) -> Bool {
        abs(a.x - b.x) < 0.01 && abs(a.y - b.y) < 0.01
    }

    /// Resizing the window keeps the column at the same fraction of the
    /// free space (full screen is a resize too; the self-test never enters
    /// it).
    private func insetDragResizeStep(_ doorbell: CameraRef) {
        guard let window, let nsWindow = window.window, let anchor = window.stage.insetAnchor else { return insetDragRestoreStep(doorbell) }
        let stage = window.stage
        // parity: inset-drag-resize
        let frame = nsWindow.frame
        var results: [Bool] = []
        for grow in [CGSize(width: 220, height: 120), CGSize(width: -120, height: -60)] {
            nsWindow.setFrame(
                NSRect(x: frame.minX, y: frame.minY, width: frame.width + grow.width, height: frame.height + grow.height),
                display: false)
            nsWindow.contentView?.layoutSubtreeIfNeeded()
            stage.layoutInsetViews()
            let kept = stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: stage.insetLayer.bounds), anchor) } == true
            results.append(kept && stage.insetAnchor == anchor)
        }
        nsWindow.setFrame(frame, display: false)
        nsWindow.contentView?.layoutSubtreeIfNeeded()
        print("inset drag: the column keeps its place when the window grows \(results[0]) and shrinks \(results[1])")
        if results.contains(false) { problems.append("resizing the window moved the inset column off its fraction") }
        insetDragRestoreStep(doorbell)
    }

    /// The position is saved with the window: windows.json's record reads
    /// back, a window restored from it puts its column there, and "Reset
    /// Position" in the inset's menu returns it to the default corner.
    private func insetDragRestoreStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let saved = window.savedWindow, let anchor = window.stage.insetAnchor else {
            return insetDragMultiStep(doorbell)
        }
        // parity: inset-drag-restore
        let record = (try? JSONEncoder().encode([saved])).flatMap { try? JSONDecoder().decode([SavedWindow].self, from: $0) }
        let readsBack = record?.first?.insetAnchor == anchor
        guard let restored = app.openWindow(window.state.content) else {
            problems.append("could not open a window to restore into")
            return insetDragMultiStep(doorbell)
        }
        restored.restoreInsetAnchor(record?.first?.insetAnchor)
        // The new window adopts the open conversation (after a turn of the main queue).
        when({ !restored.stage.shownInsetViews.isEmpty }, timeout: 2) { [self] in
            restored.window?.contentView?.layoutSubtreeIfNeeded()
            restored.stage.layoutInsetViews()
            let there = restored.stage.insetAnchor == anchor
                && restored.stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: restored.stage.insetLayer.bounds), anchor) } == true
            restored.close()
            // Reset Position, from the inset's own menu.
            let stage = window.stage
            var reset = false
            if let view = stage.shownInsetViews.first, let nsWindow = window.window,
                let e = NSEvent.mouseEvent(
                    with: .rightMouseDown, location: view.convert(CGPoint(x: 20, y: 8), to: nil), modifierFlags: [],
                    timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: nsWindow.windowNumber, context: nil,
                    eventNumber: 0, clickCount: 1, pressure: 1),
                let menu = view.menu(for: e), let item = menu.items.firstIndex(where: { $0.title == "Reset Position" })
            {
                menu.performActionForItem(at: item)
                reset = stage.insetAnchor == nil
                    && stage.insetColumn.map { Self.near(Self.measuredAnchor($0, stage: stage.insetLayer.bounds), InsetAnchor(corner: model.insetCorner)) } == true
                    && window.savedWindow?.insetAnchor == nil
            }
            print("inset drag: saved record reads back \(readsBack), a restored window puts the column there \(there), Reset Position \(reset)")
            if !readsBack { problems.append("the window's saved record lost the inset position") }
            if !there { problems.append("a window restored from the record did not put its insets there") }
            if !reset { problems.append("Reset Position did not return the column to the default corner") }
            insetDragMultiStep(doorbell)
        }
    }

    /// Several insets move as one column; at the bottom the newest is at
    /// the bottom and the others stack upward, at the top the reverse.
    private func insetDragMultiStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let device = model.runtime("fake-1"),
            let id = device.deviceID
        else { return smokeStep() }
        let stage = window.stage
        let hall = CameraRef(device: device.key, channel: 4)
        // parity: inset-drag-multi
        // A smoke alarm's inset (it stays after the stop) beside the conversation's.
        fake.emitSoundDetection(device: id, channel: 4, start: true)
        fake.emitSoundDetection(device: id, channel: 4, start: false)
        when({ stage.shownInsetViews.count == 2 }, timeout: 3) { [self] in
            guard let header = stage.shownInsetViews.first?.frame, let start = stage.insetColumn else {
                problems.append("a second inset did not open")
                return finishInsetDrag(doorbell, hall)
            }
            let b = stage.insetLayer.bounds
            @MainActor func column() -> [CGRect] { stage.shownInsetViews.map { $0.frame } }
            // To the bottom left (both axes snap).
            let grab = CGPoint(x: header.minX + 40, y: header.minY + 8)
            let bottomLeft = CGPoint(x: 15, y: b.height - start.height - 15)
            drag(from: grab, to: CGPoint(x: grab.x + bottomLeft.x - start.minX, y: grab.y + bottomLeft.y - start.minY), in: window)
            let low = column()
            let m = CGFloat(InsetGeometry.margin), gap = CGFloat(InsetGeometry.gap)
            let upward = stage.insetAnchor == InsetAnchor(x: 0, y: 1) && low.count == 2
                && low.allSatisfy { $0.minX == m } && abs(low[0].maxY - (b.height - m)) < 0.5
                && abs(low[1].maxY - (low[0].minY - gap)) < 0.5
            // To the top, a third of the way across (x does not snap).
            guard let newest = stage.shownInsetViews.first?.frame, let now = stage.insetColumn else {
                return finishInsetDrag(doorbell, hall)
            }
            let grab2 = CGPoint(x: newest.minX + 40, y: newest.minY + 8)
            let topThird = CGPoint(x: m + (b.width - 2 * m - now.width) / 3, y: 20)
            drag(from: grab2, to: CGPoint(x: grab2.x + topThird.x - now.minX, y: grab2.y + topThird.y - now.minY), in: window)
            let high = column()
            let downward = stage.insetAnchor.map { $0.y == 0 && abs($0.x - 1.0 / 3) < 0.01 } == true && high.count == 2
                && high[0].minX == high[1].minX && abs(high[0].minY - m) < 0.5 && abs(high[1].minY - (high[0].maxY + gap)) < 0.5
            print("inset drag with two insets: one column, bottom grows upward \(upward), top grows downward \(downward)")
            if !upward { problems.append("two insets at the bottom did not stack upward from it in one column: \(low)") }
            if !downward { problems.append("two insets at the top did not stack downward from it in one column: \(high)") }
            finishInsetDrag(doorbell, hall)
        }
    }

    private func finishInsetDrag(_ doorbell: CameraRef, _ hall: CameraRef) {
        guard let window else { return smokeStep() }
        let stage = window.stage
        stage.dismissInset(hall)
        stage.endConversation(doorbell)
        stage.resetInsetPosition()
        smokeStep()
    }

    /// A smoke alarm's inset stays until dismissed; dismissing it dismisses
    /// the alert.
    private func smokeStep() {
        guard let model = app.model, let window, let fake = model.fake, let device = model.runtime("fake-1"),
            let id = device.deviceID
        else { return checkLog() }
        let stage = window.stage
        let hall = CameraRef(device: device.key, channel: 4)
        // parity: alert-inset-smoke-stays
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
        // parity: fisheye-detected
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
            // parity: fisheye-circle
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
            // parity: fisheye-view-pixels
            for mode in FisheyeViewMode.allCases where mode.dewarps {
                stage.setFisheyeMode(mode, at: path)
                checkFisheyePixels(path, mode)
            }
            // parity: fisheye-ptz-drag
            // Virtual PTZ: a drag turns it, a wheel step zooms it (eased).
            let size = window.state.tree.frames(in: stage.bounds).leaf(path)?.rect.size ?? .zero
            stage.turnFisheye(path, by: CGVector(dx: size.width / 4, dy: 0), size: size)
            // parity: fisheye-ptz-zoom
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
            // parity: fisheye-reset-key
            stage.resetZoom(at: path)
            if window.state.tree[path]?.fisheye.aim != nil { problems.append("0 did not reset the virtual camera") }
            // parity: fisheye-original
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

    private var discoveryChecked = false
    private var retryChecked = false

    private func checkLog() {
        if !retryChecked, app.model?.fake != nil {
            retryChecked = true
            return retryStep()
        }
        guard !discoveryChecked, app.model?.fake != nil else { return checkLogNow() }
        discoveryChecked = true
        discoveryStep()
    }

    /// The add-device screen's "Find Devices on This Network" list against the
    /// fake core's made-up devices (docs/UX.md, "Finding devices").
    private func discoveryStep() {
        guard let model = app.model, let fake = model.fake, let settings = app.settingsWindowForTest() else {
            problems.append("no settings window")
            return checkLogNow()
        }
        let form = model.deviceForm
        // The add form is what Settings shows with no device selected; its
        // list searches when it appears.
        // parity: discovery-list
        form.startNew()
        when({ model.discovery.devices.count == FakeDiscoverable.samples.count }, timeout: 5) { [self] in
            let list = model.discovery
            print("discovery: \(list.devices.count) devices, searching \(list.isSearching)")
            if list.devices.count != FakeDiscoverable.samples.count || !list.isSearching {
                problems.append("the list did not fill while searching")
                return finishDiscovery(settings)
            }
            let byAddress = Dictionary(uniqueKeysWithValues: list.devices.map { ($0.address, $0) })
            guard let known = byAddress["192.0.2.10"], let fresh = byAddress["192.0.2.12"],
                let battery = byAddress["192.0.2.30"], let onvif = byAddress["192.0.2.40"]
            else {
                problems.append("a made-up device is missing from the list")
                return finishDiscovery(settings)
            }
            if !model.isAdded(known) || model.isAdded(fresh) { problems.append("added devices were not marked") }
            if fresh.channels != 16 || fresh.kind != .nvr || battery.batteryPercent != 80 {
                problems.append("device details did not arrive")
            }
            // parity: discovery-choose
            // A click on a row.
            model.chooseDiscovered(fresh)
            if form.draft.host != "192.0.2.12" || form.port != "9000" || form.draft.name != "NVR 3" {
                problems.append("choosing a device did not fill the form: \(form.draft.host) \(form.port) \(form.draft.name)")
            }
            // A device without a port in its answer gets the usual one; a name the user
            // changed stays; one the viewer cannot add is not chosen.
            form.draft.name = "My NVR"
            model.chooseDiscovered(battery)
            if form.draft.host != "192.0.2.30" || form.port != "9000" || form.draft.name != "My NVR" {
                problems.append("choosing the second device went wrong: \(form.draft.host) \(form.port) \(form.draft.name)")
            }
            model.chooseDiscovered(onvif)
            if form.draft.host != "192.0.2.30" { problems.append("a device the viewer cannot add was chosen") }
            model.chooseDiscovered(known)
            if form.draft.host != "192.0.2.30" { problems.append("an added device was chosen") }

            // The search's time is up; the list stays.
            fake.finishDiscovery()
            when({ !model.discovery.isSearching }, timeout: 5) { [self] in
                if model.discovery.devices.count != FakeDiscoverable.samples.count || model.discovery.foundNothing {
                    problems.append("the list changed when the search ended")
                }
                // parity: discovery-search-again
                // Search Again twice in a row: the list shows each device once.
                model.startDiscovery()
                model.startDiscovery()
                when({ model.discovery.isSearching && model.discovery.devices.count == FakeDiscoverable.samples.count },
                    timeout: 5
                ) { [self] in
                    let n = model.discovery.devices.count
                    print("discovery after Search Again twice: \(n) devices")
                    if n != FakeDiscoverable.samples.count { problems.append("a restarted search listed \(n) devices") }
                    // parity: discovery-stop-on-close
                    // Closing the window stops the search.
                    settings.window?.close()
                    if fake.isDiscovering || model.discovery.isSearching {
                        problems.append("closing Settings did not stop the search")
                    }
                    finishDiscovery(settings)
                }
            }
        }
    }

    private func finishDiscovery(_ settings: SettingsWindowController) {
        settings.window?.close()
        app.model?.stopDiscovery()
        if let fake = app.model?.fake { problems += fake.violations.map { "core contract: \($0)" } }
        checkLogNow()
    }

    /// The Retry button (docs/UX.md, "Tiles without a picture"): a device that
    /// has been reconnecting for a long time offers it under the message, a
    /// click reaches the core, and reconnecting takes it away.
    private func retryStep() {
        guard let model = app.model, let window, let fake = model.fake,
            let leaf = window.state.tree.leaves.first(where: { $0.camera != nil }), let camera = leaf.camera,
            let id = model.devices.first(where: { $0.key == camera.device })?.deviceID,
            let tile = model.leases.tile(for: camera)
        else { return checkLog() }
        let stage = window.stage
        let path = leaf.path
        // parity: tile-retry
        fake.emitForTest(#"{"type":"device_state","device":\#(id),"state":"backoff","attempt":2,"retry_ms":60000}"#)
        when({ model.devices.first { $0.key == camera.device }?.link.state == .backoff(attempt: 2, retryMilliseconds: 60000) },
            timeout: 2
        ) { [self] in
            stage.refreshChrome()
            if stage.retryShown(path) != false { problems.append("Retry showed as soon as the device was reconnecting") }
            advance(TileRetry.offerAfter - 1)
            stage.refreshChrome()
            if stage.retryShown(path) != false { problems.append("Retry showed before the delay") }
            advance(2)
            stage.refreshChrome()
            let shown = stage.retryShown(path)
            print("retry: shown after the delay \(shown.map { "\($0)" } ?? "n/a")")
            if shown != true { problems.append("Retry did not show after the delay") }
            stage.pressRetry(path)
            if !fake.retriedTiles.contains(tile.id) { problems.append("the Retry click did not reach the core") }
            stage.refreshChrome()
            if stage.retryShown(path) == true { problems.append("Retry stayed after it was pressed") }
            fake.emitForTest(#"{"type":"device_state","device":\#(id),"state":"connected"}"#)
            when({ model.devices.first { $0.key == camera.device }?.link.state == .connected }, timeout: 2) { [self] in
                advance(TileRetry.offerAfter + 1)
                stage.refreshChrome()
                if stage.retryShown(path) == true { problems.append("Retry showed for a tile with a picture") }
                checkLog()
            }
        }
    }

    private func checkLogNow() {
        // parity: appkit-warnings
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
