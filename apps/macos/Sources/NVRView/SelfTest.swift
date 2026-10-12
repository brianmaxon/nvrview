import AppKit
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

/// `--selftest SECONDS`: runs the real app (its window behind all others, no
/// focus, muted), then checks, in order:
///
/// 0. right after launch, with the sidebar collapsed as a restored window
///    collapses it, the pointer resting at the left edge (fed to both the
///    edge reveal and AppKit's own collapsed-sidebar hover) shows the
///    overlay while the sidebar stays collapsed and the stage does not move;
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
/// 14e. with `--fake`, the recordings window (behind the others) against the
///    fake core's made-up recording store: the chosen cameras are searched,
///    today is asked for and held under the window's own hold, the calendar
///    marks the day and the timeline has a lane per camera, the animal
///    filter shows only the animal incident, another day is asked for but
///    not held, a cover thumbnail is decoded with VideoToolbox, shrunk to
///    320 points, and put back into the core's cache, rename, merge, split,
///    add and remove a camera, and revert apply, locking says the video is
///    downloading, a download pauses, resumes, finishes, shows in the
///    Finder, is forgotten, and goes to the Trash (both recorded, not done)
///    once the core lets go of it, a camera group joins its cameras'
///    incidents, paging the calendar asks for that month alone, the names
///    are the camera list's, the player plays a selected incident (pictures
///    decoded and drawn, pause, skip, speed, the grid's views with the
///    sound following, a lane click seeking, Remove from Incident and Split
///    Here without reopening), the panes give way in a narrow window, and
///    closing the window releases its hold and closes the playback;
/// 14f. bundled update defaults (docs/UPDATES.md): an `update-defaults.json` in a
///    throwaway app folder, with a throwaway key and source, makes the key
///    trusted and the source the default, listed as bundled; a user key,
///    turning the bundled key off, and the user's own source override it,
///    and with no file nothing is trusted;
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
        launchPeekStep { [self] in
            when({ [self] in gridReady }, timeout: seconds) { [self] in checkGrid() }
        }
        // A hard limit for the whole run (the steps after the grid take a few
        // seconds: tier switches and display refreshes are real).
        after(seconds + 30) { [self] in finish(problems + ["timed out"]) }
    }

    /// Right after launch, with the sidebar collapsed the way a restored
    /// window collapses it: the pointer resting at the left edge, as both our
    /// edge reveal and AppKit's own collapsed-sidebar hover see it, shows the
    /// overlay and leaves the sidebar collapsed and the stage where it was
    /// (AppKit's peek used to push the stage aside; see
    /// `ViewerSplitViewController`).
    private func launchPeekStep(then next: @escaping @MainActor () -> Void) {
        guard let window else { return next() }
        // parity: sidebar-system-peek
        window.restoreSidebar(collapsed: true)
        window.window?.contentView?.layoutSubtreeIfNeeded()
        let framesBefore = window.splitFramesForTest
        let overlay = window.sidebarOverlay
        if window.systemSidebarPeekAllowedForTest == true {
            problems.append("AppKit may still peek the collapsed sidebar into the split view")
        }
        let hovered = window.hoverCollapsedSidebarEdgeForTest(x: 5, inside: true)
        overlay.simulatedPointer = CGPoint(x: 2, y: 200)
        overlay.pointerAt(CGPoint(x: 2, y: 200))
        advance(overlay.reveal.dwell)
        if !overlay.isShown { problems.append("the sidebar did not slide in at the left edge right after launch") }
        // A fixed wait, to show that nothing happens: AppKit's hover runs on
        // its own real-time timer (it peeked about 0.75 s after the pointer
        // came to rest), which the test cannot advance.
        after(hovered ? 1.5 : 0) { [self] in
            let framesAfter = window.splitFramesForTest
            let stage = framesAfter.last.map { "\(Int($0.minX)),\(Int($0.minY)) \(Int($0.width))x\(Int($0.height))" } ?? "none"
            print(
                "launch peek: AppKit hover \(hovered ? "driven" : "absent"), peek allowed "
                    + "\(window.systemSidebarPeekAllowedForTest.map(String.init) ?? "n/a"), overlay shown \(overlay.isShown), "
                    + "stage \(stage)\(framesAfter == framesBefore ? " unchanged" : " moved from \(framesBefore)")")
            if framesAfter != framesBefore {
                problems.append("the stage moved or the sidebar opened while the edge reveal was up after launch")
            }
            if hovered { window.hoverCollapsedSidebarEdgeForTest(x: 5, inside: false) }
            overlay.simulatedPointer = CGPoint(x: 900, y: 200)
            overlay.pointerAt(CGPoint(x: 900, y: 200))
            advance(overlay.reveal.hideDelay)
            overlay.simulatedPointer = nil
            window.restoreSidebar(collapsed: false)
            window.window?.contentView?.layoutSubtreeIfNeeded()
            next()
        }
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

    /// Runs `body` after everything already queued on the main queue (the
    /// hops a fake's events and callbacks make), instead of after a
    /// guessed delay: the queue is first in, first out.
    private func afterQueued(_ body: @escaping @MainActor () -> Void) {
        DispatchQueue.main.async { MainActor.assumeIsolated { body() } }
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

    /// Resizes the window in a burst of steps, one per run-loop turn (each
    /// costs a layout pass), like a full-screen transition: the grid keeps drawing while the drawable
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
                afterQueued { advance(step + 1) }
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
        afterQueued { [self] in
            model.deleteGroup(group)
            if let layout { model.deleteLayout(layout) }
            afterQueued { [self] in revealStep() }
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
            // The hide waits while a button is down or a field is being
            // edited; the run must not depend on the real mouse (a click
            // elsewhere on the machine) or on focus, so those are pinned.
            let realButtonDown = overlay.isButtonDown, realEditing = overlay.isEditing
            overlay.isButtonDown = { false }
            overlay.isEditing = { false }
            overlay.simulatedPointer = CGPoint(x: 2, y: 200)
            overlay.pointerAt(CGPoint(x: 2, y: 200))
            advance(overlay.reveal.dwell / 2)
            if overlay.isShown { problems.append("the sidebar slid in before the dwell") }
            // To the dwell's deadline exactly: two half steps can land one
            // rounding step short of it, depending on the clock's start.
            let now = app.testClock?.now ?? 0
            advance(overlay.reveal.deadline.map { max(0, $0 - now) } ?? overlay.reveal.dwell / 2)
            if !overlay.isShown { problems.append("the sidebar did not slide in at the left edge") }
            // A layout pass with the overlay up over the stage (the next
            // run-loop turn; display refreshes are too slow to wait for).
            afterQueued { [self] in
                if grid.bounds.size != size || grid.drawableSize != drawable || grid.drawableResizes != resizes {
                    problems.append("the stage or its drawable changed size while the sidebar overlay was up")
                }
                print("reveal: shown, stage \(Int(size.width))x\(Int(size.height)) unchanged")
                overlay.simulatedPointer = CGPoint(x: 900, y: 200)
                overlay.pointerAt(CGPoint(x: 900, y: 200))
                advance(overlay.reveal.hideDelay)
                if overlay.isShown { problems.append("the sidebar overlay did not slide away") }
                overlay.simulatedPointer = nil
                overlay.isButtonDown = realButtonDown
                overlay.isEditing = realEditing
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
        // The tile's letterbox bar at 1x, for the zoom-fills-tile check.
        let pixelScale = stage.grid.drawableSize.width / max(stage.bounds.width, 1)
        zoomBar = stage.renderedDestination(of: path).flatMap {
            Self.bar(cell: leaf.rect.pixels(scale: pixelScale), picture: $0)
        }
        let barLitBefore = zoomBar.flatMap { litShare($0) }
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
            if let before, let source, let picture = stage.pictureSizes[camera] {
                // The part of the picture under the zoomed destination
                // (TileZoom), whatever the tile's shape; `before` carries
                // the buffer's offset.
                let scale = grid.drawableSize.width / max(stage.bounds.width, 1)
                let cell = leaf.rect.pixels(scale: scale)
                let v = z.visible(in: ZoomBase(picture: picture, tile: cell.size, fill: window.state.fill))
                let want = CGRect(
                    x: before.minX + v.minX * picture.width, y: before.minY + v.minY * picture.height,
                    width: v.width * picture.width, height: v.height * picture.height)
                if abs(source.width - want.width) > 1 || abs(source.minX - want.minX) > 1
                    || abs(source.height - want.height) > 1
                {
                    problems.append("the renderer's source \(source) is not the zoomed region \(want)")
                }
            } else {
                problems.append("no source rectangle for the zoomed tile")
            }
            // parity: zoom-fills-tile
            // Past the point where the picture fills the tile's height (or
            // width), the former bar shows picture.
            if let bar = zoomBar, let lit = litShare(bar), let was = barLitBefore {
                print(String(format: "zoom fills the tile: bar lit %.2f at 1x, %.2f at 2x", was, lit))
                if was > 0.02 { problems.append("the letterbox bar was not black at 1x") }
                if lit < 0.5 { problems.append("zoomed to 2x, the letterbox bar still shows no picture") }
            } else {
                problems.append("the zoomed tile has no letterbox bar to check")
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
                // The zoom-fills-tile check, back at 1x: the bar is black again.
                if let bar = zoomBar, let lit = litShare(bar) {
                    print(String(format: "zoom back at 1x: bar lit %.2f", lit))
                    if lit > 0.02 { problems.append("back at 1x, the letterbox bar is not black") }
                }
                if model.leases.wantedTier(camera) != .sub {
                    problems.append("Auto back at 1x did not ask for the sub stream")
                }
                stage.fullScreen = false
                stage.chromeHideDelay = 3
                stage.alertActive = { [weak model] in model?.alerts.isActive($0) == true }
                fillPanStep(path, camera)
            }
        }
    }

    // MARK: Fill Tiles: per tile, panned, saved (docs/UX.md "Fill Tiles")

    /// The letterboxed tile at `path` gets its own Fill Tile from its menu
    /// (the others stay fitted and the window's switch shows mixed); a drag
    /// on its picture moves the crop, stopping at the picture's edge; a
    /// drag on its header swaps it; the window's record restores the crop
    /// and the single tile's choice; the window's switch then sets every
    /// tile.
    private func fillPanStep(_ path: NodePath, _ camera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        let grid = stage.grid
        func prepare() { grid.prepare?(grid.renderer, grid.drawableSize) }
        func leaf(_ p: NodePath) -> LeafFrame? { stage.frames.leaf(p) }
        guard let tile = leaf(path), let other = stage.frames.leaves.first(where: { $0.camera != nil && $0.camera != camera }),
            let otherCamera = other.camera
        else {
            problems.append("no two cameras for the Fill Tiles steps")
            return headerStep()
        }
        // parity: fill-per-tile
        let menu = stage.contextMenu(for: tile)
        if let i = menu.items.firstIndex(where: { $0.title == "Fill Tile" }) {
            if menu.items[i].state != .off { problems.append("Fill Tile was checked on a fitted tile") }
            menu.performActionForItem(at: i)
        } else {
            problems.append("the tile menu has no Fill Tile")
        }
        let fillsMenu = NSMenuItem(title: "Fill Tiles", action: #selector(ViewerWindowController.toggleFill(_:)), keyEquivalent: "")
        _ = window.validateMenuItem(fillsMenu)
        let single = stage.fills(camera) && !stage.fills(otherCamera) && fillsMenu.state == .mixed
        let checked = stage.contextMenu(for: tile).items.first { $0.title == "Fill Tile" }?.state == .on
        print("fill per tile: this tile fills \(stage.fills(camera)), another \(stage.fills(otherCamera)), Fill Tiles \(fillsMenu.state == .mixed ? "mixed" : "not mixed"), item checked \(checked)")
        if !single { problems.append("Fill Tile on one tile did not fill only it, with Fill Tiles mixed") }
        if !checked { problems.append("Fill Tile is not checked on the tile that fills") }
        prepare()
        guard let picture = stage.pictureSizes[camera], let before = stage.renderedSource(of: path) else {
            problems.append("no picture to pan")
            return headerStep()
        }
        let base = ZoomBase(picture: picture, tile: tile.rect.size, fill: true)
        let wide = base.width > 1
        // parity: fill-pan-drag
        let middle = CGPoint(x: tile.rect.midX, y: tile.rect.midY + 20)
        let kind = stage.dragKind(at: middle)
        let step = wide ? CGVector(dx: tile.rect.width / 8, dy: 0) : CGVector(dx: 0, dy: tile.rect.height / 8)
        stage.simulateDrag(from: middle, to: CGPoint(x: middle.x + step.dx, y: middle.y + step.dy))
        prepare()
        let z = stage.zoom(of: camera)
        let moved = stage.renderedSource(of: path)
        let v = z.visible(in: base)
        // The crop moved toward the picture's left (or top) by the drag, as TileZoom says.
        let followed = moved.map { m -> Bool in
            if wide {
                let offset = before.minX - (1 - v.width) / 2 * picture.width
                return abs(m.minX - (offset + v.minX * picture.width)) < 1.5 && m.minX < before.minX
            }
            let offset = before.minY - (1 - v.height) / 2 * picture.height
            return abs(m.minY - (offset + v.minY * picture.height)) < 1.5 && m.minY < before.minY
        } == true
        // Far beyond the edge it stops there.
        stage.simulateDrag(from: middle, to: CGPoint(x: middle.x + step.dx * 40, y: middle.y + step.dy * 40))
        prepare()
        let edge = stage.zoom(of: camera).visible(in: base)
        let stopped = wide ? abs(edge.minX) < 1e-6 : abs(edge.minY) < 1e-6
        let panned = stage.isPanned(path) && !stage.zoom(of: camera).isZoomed
        let fittedSwaps = stage.dragKind(at: CGPoint(x: other.rect.midX, y: other.rect.midY + 20)) == "swap"
        print("fill pan drag: kind \(kind ?? "none"), crop followed \(followed), stopped at the edge \(stopped), panned \(panned), a fitted tile still swaps \(fittedSwaps)")
        if kind != "pan" { problems.append("a drag on a filled, cropped picture would not pan it") }
        if !followed { problems.append("dragging the filled picture did not move the crop with the drag") }
        if !stopped { problems.append("panning the filled picture did not stop at its edge") }
        if !panned { problems.append("the tile is not marked panned at 1x") }
        if !fittedSwaps { problems.append("a drag on a fitted tile's picture no longer swaps it") }
        // Back from the edge to a position of its own, for the restore check.
        stage.simulateDrag(from: middle, to: CGPoint(x: middle.x - step.dx * 2, y: middle.y - step.dy * 2))
        fillPanHeaderSwapStep(path, camera, otherCamera)
    }

    /// A drag on the shown header of a tile whose picture pans swaps the
    /// tiles; the crop goes with the camera.
    private func fillPanHeaderSwapStep(_ path: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        // parity: fill-pan-header-swap
        guard let tile = stage.frames.leaf(path), let target = stage.frames.leaves.first(where: { $0.camera == otherCamera })
        else {
            problems.append("no tiles to swap")
            return headerStep()
        }
        let zoom = stage.zoom(of: camera)
        stage.showHeader(path)
        let onHeader = CGPoint(x: tile.rect.midX, y: tile.rect.minY + 10)
        let kind = stage.dragKind(at: onHeader)
        stage.simulateDrag(from: onHeader, to: CGPoint(x: target.rect.midX, y: target.rect.midY))
        let swapped = window.state.tree[path]?.camera == otherCamera && window.state.tree[target.path]?.camera == camera
        let kept = stage.zoom(of: camera) == zoom
        // And back, the same way, from the tile it went to.
        if swapped, let there = stage.frames.leaf(target.path) {
            stage.showHeader(target.path)
            stage.simulateDrag(
                from: CGPoint(x: there.rect.midX, y: there.rect.minY + 10), to: CGPoint(x: tile.rect.midX, y: tile.rect.midY))
        }
        let back = window.state.tree[path]?.camera == camera
        print("fill pan header swap: kind \(kind ?? "none"), swapped \(swapped), crop kept with the camera \(kept), swapped back \(back)")
        if kind != "swap" { problems.append("a drag on the header of a panning tile would not swap it") }
        if !swapped { problems.append("dragging a tile's header onto another did not swap them") }
        if !kept { problems.append("the crop did not follow the camera through the swap") }
        if !back { problems.append("dragging the header back did not swap the tiles back") }
        fillPanRestoreStep(path, camera, otherCamera)
    }

    /// The window's record keeps the crop, the zoom and the single tile's
    /// Fill choice; a window restored from it shows the same part of the
    /// picture. Then the window's switch sets every tile, and Reset
    /// Position recenters.
    private func fillPanRestoreStep(_ path: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window, let saved = window.savedWindow else { return headerStep() }
        let stage = window.stage
        // parity: fill-pan-restore
        let record = (try? JSONEncoder().encode([saved])).flatMap { try? JSONDecoder().decode([SavedWindow].self, from: $0) }?.first
        let zoom = stage.zoom(of: camera)
        let readsBack = record.map { $0.views.zoom(camera) == zoom && $0.views.fills(camera) && !$0.views.fills(otherCamera) } == true
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
        let source = stage.renderedSource(of: path)
        guard let record, let restored = app.openWindow(window.state.content) else {
            problems.append("could not open a window to restore into")
            return headerStep()
        }
        restored.window?.setFrame(window.window?.frame ?? .zero, display: false)
        restored.window?.contentView?.layoutSubtreeIfNeeded()
        restored.restoreViews(record.views)
        when({
            restored.stage.grid.prepare?(restored.stage.grid.renderer, restored.stage.grid.drawableSize)
            return restored.stage.pictureSizes[camera] != nil
        }, timeout: 2) { [self] in
            let rs = restored.stage
            rs.grid.prepare?(rs.grid.renderer, rs.grid.drawableSize)
            let same = rs.zoom(of: camera) == zoom && rs.fills(camera) && !rs.fills(otherCamera)
            let shown = rs.renderedSource(of: path)
            let sameCrop = source.flatMap { a in shown.map { b in abs(a.minX - b.minX) < 2 && abs(a.minY - b.minY) < 2 && abs(a.width - b.width) < 2 } } == true
            restored.close()
            print("fill pan restore: record reads back \(readsBack), restored window same settings \(same), same crop \(sameCrop)")
            if !readsBack { problems.append("the window's saved record lost the crop or the tile's Fill choice") }
            if !same { problems.append("a window restored from the record did not get the crop and Fill choices back") }
            if !sameCrop { problems.append("a restored window does not show the same part of the picture") }
            // parity: fill-window-switch
            // The window's switch: every tile fills, then every tile fits.
            window.toggleFill(nil)
            let allFill = stage.fills(camera) && stage.fills(otherCamera) && !window.state.views.mixed
            window.toggleFill(nil)
            let allFit = !stage.fills(camera) && !stage.fills(otherCamera) && !window.state.views.mixed
            // The crop stays with the camera; Reset Position recenters it.
            stage.setFill(true, for: camera)
            var reset = false
            if let tile = stage.frames.leaf(path) {
                let menu = stage.contextMenu(for: tile)
                if let i = menu.items.firstIndex(where: { $0.title == "Reset Position" }) {
                    menu.performActionForItem(at: i)
                    reset = stage.zoom(of: camera) == .none
                }
            }
            print("fill per tile: Fill Tiles fills every tile \(allFill), then fits every tile \(allFit); Reset Position \(reset)")
            if !allFill || !allFit { problems.append("Fill Tiles did not set every tile of the window") }
            if !reset { problems.append("Reset Position did not recenter the filled crop") }
            stage.setFill(false, for: camera)
            headerStep()
        }
    }

    // MARK: The tile header (docs/UX.md "Tile header")

    /// A tile's header laid out by the shared model at a narrow, a middle
    /// and a wide width: the buttons on screen are the model's, in its
    /// order, inside the tile and clear of the title; "…" holds the rest
    /// and the right-click menu is the model's list. Capabilities from the
    /// `channels` event put Start Conversation in a doorbell's header and
    /// the PTZ buttons in a moving camera's. Recording is not
    /// built yet: its button and key say "coming soon".
    private func headerStep() {
        guard let model = app.model, let window else { return insetStep() }
        let stage = window.stage
        // The chrome as the model says now (the step before faked an alert).
        stage.refreshChrome()
        guard let leaf = stage.frames.leaves.first(where: { $0.camera != nil }), let camera = leaf.camera,
            let chrome = stage.chromes[leaf.path], let tile = stage.headerTile(camera, path: leaf.path)
        else {
            problems.append("no tile with a header")
            return insetStep()
        }
        let saved = chrome.frame
        let buttons = TileHeaderLayout.buttons(for: tile)
        let m = TileHeaderMetrics.standard
        /// Lays the header out `width` points wide: the buttons shown, and
        /// whether they sit inside the tile, apart, and right of the title.
        func at(_ width: CGFloat) -> (shown: [TileHeaderItem], layout: TileHeaderLayout?, placed: Bool) {
            chrome.frame = CGRect(x: saved.minX, y: saved.minY, width: width, height: saved.height)
            chrome.layoutSubtreeIfNeeded()
            let frames = chrome.headerButtonFrames.values.sorted { $0.minX < $1.minX }
            let apart = zip(frames, frames.dropFirst()).allSatisfy { $0.maxX <= $1.minX + 0.5 }
            let inside = frames.allSatisfy { $0.minX >= 0 && $0.maxX <= width + 0.5 }
            let clear = frames.first.map { chrome.titleFrame.maxX <= $0.minX } ?? true
            return (chrome.shownHeaderButtons, chrome.headerLayout, apart && inside && clear)
        }
        // parity: header-priority-narrow
        let narrow = at(110)
        // One point short of every button: some go into "…".
        let width = CGFloat(2 * m.padding + m.titleMin + m.icon * Double(buttons.count) - 1)
        let fit = Int((Double(width) - 2 * m.padding - m.titleMin - m.icon) / m.icon)
        let middle = at(width)
        let nudged = at(width + 5)
        let wide = at(1000)
        let narrowOK = narrow.shown == [.more] && narrow.layout?.visible == [.title, .more] && narrow.placed
        let middleOK = middle.shown == Array(buttons.prefix(fit)) + [.more]
            && middle.layout?.visible.filter({ $0 != .title }) == middle.shown && middle.placed
        let wideOK = wide.shown == buttons && wide.layout?.showsMore == false && wide.placed
        let steady = nudged.shown == middle.shown
        // Capabilities: the doorbell and the moving camera of the fake NVR.
        let doorbell = CameraRef(device: camera.device, channel: 3, lens: 0)
        let mover = CameraRef(device: camera.device, channel: 1, lens: 0)
        let doorbellButtons = stage.headerTile(doorbell, path: leaf.path).map(TileHeaderLayout.buttons(for:)) ?? []
        let moverButtons = stage.headerTile(mover, path: leaf.path).map(TileHeaderLayout.buttons(for:)) ?? []
        let plainTalk = model.channelInfo(camera)?.talk == true || model.channelInfo(camera)?.doorbell == true
        let caps = doorbellButtons.contains(.conversation) && !moverButtons.contains(.conversation)
            && [TileHeaderItem.panTilt, .presets, .lens].allSatisfy(moverButtons.contains)
            && !doorbellButtons.contains(.panTilt) && (plainTalk || !buttons.contains(.conversation))
        print(
            "header: 110 pt \(narrow.shown.map(\.rawValue)), \(Int(width)) pt \(middle.shown.map(\.rawValue)), "
                + "5 pt wider unchanged \(steady), 1000 pt \(wide.shown.map(\.rawValue)); "
                + "doorbell \(doorbellButtons.map(\.rawValue)), pan-tilt camera \(moverButtons.map(\.rawValue))")
        if !narrowOK { problems.append("below 120 pt the header did not show only the title and …") }
        if !middleOK { problems.append("one point short of every button, the header did not show the model's first \(fit) buttons and …") }
        if !wideOK { problems.append("a wide header did not show every button without …") }
        if !steady { problems.append("a 5 pt width change reflowed the header") }
        if !caps { problems.append("the channels event's talk, doorbell and PTZ capabilities did not reach the header") }
        // parity: header-overflow-menu
        _ = at(width)
        let more = stage.moreMenu(leaf.path)?.items.map(\.title) ?? []
        let wantMore = chrome.headerLayout?.moreMenu.map(\.label) ?? ["?"]
        let context = stage.contextMenu(for: leaf).items.map(\.title)
        let wantContext = TileHeaderLayout.menu(for: tile).map(\.label)
        let menusOK = more == wantMore && Array(context.prefix(wantContext.count)) == wantContext
            && context.contains("Split Side by Side") && more.contains("Entrance") && !more.contains("Listen")
        print("header menus: … \(more), right-click starts with the model's \(wantContext.count) items \(menusOK)")
        if !menusOK { problems.append("the … or right-click menu is not the header model's list (\(more) vs \(wantMore))") }
        // A button whose action is not built yet (Record) says so; so does R.
        _ = at(1000)
        chrome.pressHeader(.record)
        let clicked = stage.toastText
        advance(ToastView.duration + 0.1)
        let gone = stage.toastText == nil
        window.window?.makeFirstResponder(stage)
        window.state.selected = leaf.path
        press("r", keyCode: 15)
        let keyR = stage.toastText
        advance(ToastView.duration + 0.1)
        chrome.frame = saved
        stage.relayoutChrome()
        print("header stubs: click \(clicked ?? "nothing"), gone after \(ToastView.duration) s \(gone), R \(keyR ?? "nothing")")
        if clicked?.hasSuffix("is coming soon") != true || !gone { problems.append("Record's button did not say it is coming soon") }
        if keyR != "Start Recording is coming soon" { problems.append("R did not reach the header's actions") }
        pictureStep()
    }

    // MARK: Save picture (docs/CAPTURE.md "Save picture")

    /// A throwaway pictures folder for the model to save into.
    private func freshPictureFolder(_ model: AppModel) -> URL {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("nvrview-selftest-pictures-\(UUID().uuidString)", isDirectory: true)
        model.picturesFolder = folder
        model.pictureFormat = .png
        model.shareSafeNames = false
        return folder
    }

    /// Runs `action` (a key or a menu item), waits for the save it starts to
    /// finish, and gives the record (nil if none came).
    private func saving(
        _ stage: StageView, _ action: () -> Void, then body: @escaping @MainActor (PictureSaveRecord?) -> Void
    ) {
        let before = stage.pictureSaves
        action()
        when({ stage.pictureSaves > before }, timeout: 8) {
            body(stage.pictureSaves > before ? stage.lastPictureSave : nil)
        }
    }

    /// Checks one finished save: a file with the naming rule's shape and
    /// extension, its size, its dimensions (`size`, within a pixel, when
    /// given), and, for a copy of the decoded picture, its pixels against a
    /// CPU conversion of the buffer it was made from. Returns the image.
    @discardableResult
    private func checkSaved(
        _ record: PictureSaveRecord?, _ what: String, ext: String, nameEnds: String, size: CGSize?, tolerance: Int
    ) -> RenderedImage? {
        guard let record else {
            problems.append("\(what): no picture was saved")
            return nil
        }
        guard case .success(let saved) = record.result else {
            problems.append("\(what): the save failed: \(record.result)")
            return nil
        }
        let name = saved.url.lastPathComponent
        let shape = /\d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2}\.\d{3} .+/
        let fileSize = (try? FileManager.default.attributesOfItem(atPath: saved.url.path)[.size] as? Int) ?? nil
        guard let data = try? Data(contentsOf: saved.url), let image = PictureEncoder.decode(data) else {
            problems.append("\(what): \(name) is not an image file")
            return nil
        }
        let sized = size.map { abs(CGFloat(image.width) - $0.width) <= 1 && abs(CGFloat(image.height) - $0.height) <= 1 } ?? true
        let named = name.wholeMatch(of: shape) != nil && name.hasSuffix(nameEnds + "." + ext)
        let toast = window?.stage.toastText
        print("\(what): \(name), \(image.width)x\(image.height), \(data.count) bytes, toast \(toast ?? "none")")
        if !named { problems.append("\(what): the file name \(name) does not follow the rule") }
        if fileSize != data.count || data.count < 100 { problems.append("\(what): the file has \(fileSize ?? -1) bytes") }
        if !sized { problems.append("\(what): \(image.width)x\(image.height), expected \(size.map { "\($0)" } ?? "?")") }
        if toast != "Picture saved" || window?.stage.toast.actionTitle != "Show in Finder" {
            problems.append("\(what): the confirmation toast said \(toast ?? "nothing")")
        }
        if !record.dewarped {
            let (checked, matched) = PictureCheck.compare(
                image, buffer: record.picture.pixelBuffer, color: record.color, region: record.region, tolerance: tolerance)
            print("\(what): \(matched) of \(checked) sampled pixels match the decoded picture")
            if checked < 20 || matched * 100 < checked * 95 {
                problems.append("\(what): only \(matched) of \(checked) pixels match the decoded picture")
            }
        }
        return image
    }

    /// P saves the tile's full picture as PNG, a share-safe JPEG names the
    /// camera Camera A, and Save Zoomed Area saves the visible region of a
    /// zoomed tile at the source's resolution; each file is read back and its
    /// pixels compared with the decoded buffer.
    private func pictureStep() {
        guard let model = app.model, let window else { return insetStep() }
        let stage = window.stage
        guard let leaf = stage.frames.leaves.first(where: { $0.camera.map { !model.isFisheye($0) } == true }),
            let camera = leaf.camera
        else {
            problems.append("no plain tile to save a picture from")
            return insetStep()
        }
        let path = leaf.path
        let folder = freshPictureFolder(model)
        let name = PictureNaming.sanitize(model.name(of: camera))
        when({ model.leases.tile(for: camera)?.latestPicture() != nil }, timeout: 3) { [self] in
            // parity: capture-picture-tile
            window.window?.makeFirstResponder(stage)
            window.state.selected = path
            let size = stage.pictureSizes[camera]
            saving(stage, { press("p", keyCode: 35) }) { [self] record in
                checkSaved(record, "picture", ext: "png", nameEnds: " " + name, size: size, tolerance: 3)
                // Share-safe names and JPEG.
                model.shareSafeNames = true
                model.pictureFormat = .jpeg
                saving(stage, { press("p", keyCode: 35) }) { [self] record in
                    checkSaved(record, "picture (share-safe JPEG)", ext: "jpg", nameEnds: " Camera A", size: size, tolerance: 14)
                    model.shareSafeNames = false
                    model.pictureFormat = .png
                    zoomedPictureStep(path, camera, folder: folder)
                }
            }
        }
    }

    private func zoomedPictureStep(_ path: NodePath, _ camera: CameraRef, folder: URL) {
        guard let model = app.model, let window else { return insetStep() }
        let stage = window.stage
        guard let leaf = stage.frames.leaves.first(where: { $0.path == path }) else { return insetStep() }
        // parity: capture-picture-zoomed
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: true)
        advance(1)
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
        let want = stage.renderedSource(of: path)
        let full = stage.pictureSizes[camera]
        let name = PictureNaming.sanitize(model.name(of: camera))
        saving(stage, { stage.performOnSelected(.saveZoomedArea) }) { [self] record in
            if let want, let full {
                checkSaved(record, "zoomed area", ext: "png", nameEnds: " " + name, size: want.size, tolerance: 3)
                if let r = record?.region, abs(r.minX - want.minX) > 1 || abs(r.minY - want.minY) > 1 {
                    problems.append("zoomed area: the region \(r) is not the one shown, \(want)")
                }
                if want.width >= full.width || want.height >= full.height {
                    problems.append("zoomed area: the region \(want.size) is not smaller than the picture \(full)")
                }
            } else {
                problems.append("zoomed area: the tile drew no source rectangle")
            }
            stage.resetZoom(at: path)
            advance(1)
            try? FileManager.default.removeItem(at: folder)
            insetStep()
        }
    }

    /// A fisheye tile's saved view is the dewarped picture at the source's
    /// resolution (its pixels match the fixture's colors where the reference
    /// mapping says), and Save Original Fisheye Picture is the circle as
    /// received.
    private func fisheyePictureStep(_ path: NodePath, _ camera: CameraRef) {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        let folder = freshPictureFolder(model)
        let name = PictureNaming.sanitize(model.name(of: camera))
        stage.setFisheyeMode(.panorama, at: path)
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
        window.window?.makeFirstResponder(stage)
        window.state.selected = path
        // parity: capture-picture-fisheye
        saving(stage, { stage.performOnSelected(.savePicture) }) { [self] record in
            let image = checkSaved(record, "fisheye view", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3)
            if record?.dewarped != true { problems.append("fisheye view: the saved picture was not dewarped") }
            if let image, let record,
                let dewarp = stage.fisheyeDewarp(
                    camera: camera, path: path, picture: record.picture,
                    tileSize: CGSize(width: image.width, height: image.height))
            {
                let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
                let (checked, matched) = Self.fisheyeMatches(dewarp, in: rect) { image.rgb(x: $0, y: $1) }
                print("fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                if checked < 20 || Double(matched) < 0.9 * Double(checked) {
                    problems.append("fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                }
            }
            saving(stage, { stage.performOnSelected(.saveOriginalFisheye) }) { [self] original in
                let circle = checkSaved(original, "original fisheye", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3)
                if original?.dewarped != false { problems.append("original fisheye: the saved picture was dewarped") }
                if let circle, let image, circle.width != image.width || circle.height != image.height {
                    problems.append("original fisheye: \(circle.width)x\(circle.height), not the source's size \(image.width)x\(image.height)")
                }
                try? FileManager.default.removeItem(at: folder)
                checkLog()
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
                // (Letting go of Talk above also drained, so "finished"
                // alone does not show that the reply went out.)
                when({
                    guard model.calls.fakeSpeech?.rendered.isEmpty == false else { return false }
                    if case .stopped(let r)? = model.calls.call(for: doorbell)?.talk.phase { return r == "finished" }
                    return false
                }, timeout: 3) { [self] in
                    let phase = model.calls.call(for: doorbell)?.talk.phase
                    print("typed reply: \(model.calls.fakeSpeech?.rendered ?? []), talk \(String(describing: phase))")
                    if model.calls.fakeSpeech?.rendered != ["Hello, one moment please"] {
                        problems.append("the typed reply was not rendered")
                    }
                    if phase != .stopped(reason: "finished") { problems.append("the typed reply's talk did not drain") }
                    // parity: call-caption-echo
                    // The doorbell's microphone hears its speaker play the reply: the
                    // visitor caption that repeats it is dropped; other speech is kept.
                    guard let engine = model.calls.visitorEngine(doorbell) else { return checkLog() }
                    engine.onPartial?("Hello, one moment")
                    engine.onFinal?("Hello, one moment please.")
                    engine.onFinal?("I have a delivery for the house")
                    when({ model.calls.call(for: doorbell)?.transcript.finals.contains { $0.text.hasPrefix("I have a delivery") } == true }, timeout: 2) { [self] in
                        let call = model.calls.call(for: doorbell)
                        let texts = call?.transcript.finals.map(\.text) ?? []
                        print("caption echo: \(texts), shown \(call?.transcript.lines.map(\.text) ?? [])")
                        if texts.contains(where: { $0.hasPrefix("Hello, one moment") }) { problems.append("the reply came back as a visitor caption (echo not dropped)") }
                        if call?.transcript.partials[.visitor] != nil { problems.append("the echo's volatile caption is still shown") }
                        if call?.echo.dropped != 2 { problems.append("expected two dropped echo captions, saw \(call?.echo.dropped ?? -1)") }
                        coreRefusalStep(doorbell, otherCamera, startsBefore: fake.talkStarts)
                    }
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
        // parity: call-talk-conversation
        // The core is told the call is open on the doorbell's tile (its talk
        // starts skip the per-minute limit), and told again when it ends.
        let markedTile = model.leases.tile(for: doorbell)
        let marked = markedTile.map { fake.inConversation(tile: $0.id) } ?? false
        print("conversation: core told the call is open \(marked)")
        if !marked || model.calls.conversationTile(doorbell) !== markedTile {
            problems.append("opening a conversation did not tell the core (no talk-back limit during a call)")
        }
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
        if let markedTile, fake.inConversation(tile: markedTile.id) {
            problems.append("ending a conversation did not tell the core it ended")
        }
        if model.audible.contains(doorbell) { problems.append("ending a conversation left the camera audible") }
        echoStep(doorbell)
    }

    /// Echo cancellation (docs/AUDIO.md "Echo cancellation"): the fake
    /// microphone reports the system's canceller on, then unavailable. Real
    /// echo cannot be heard in a self-test: this checks what the call shows
    /// and logs, and the caption gate's rule in each mode (the microphone is
    /// always sent; only its captions are gated).
    private func echoStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake, let mic = model.calls.fakeMicrophone else {
            return checkLog()
        }
        let stage = window.stage
        // parity: call-echo-cancellation
        stage.startConversation(doorbell)
        guard let tile = model.leases.tile(for: doorbell) else {
            problems.append("the conversation has no tile")
            return transcriptFileStep(doorbell)
        }
        let prepared = mic.prepared
        func youFinals() -> Int { model.calls.call(for: doorbell)?.transcript.finals.filter { $0.source == .you }.count ?? 0 }
        let allowed = model.cameraOptions(doorbell).allowTalk
        model.setAllowTalk(doorbell, true)
        mic.echo = .system
        // The doorbell plays ordinary speech (above the half-duplex gate's
        // threshold, below the safety duck's).
        fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.3, rms: 0.1))
        model.calls.startTalking(doorbell)
        let talking: @MainActor () -> Bool = { model.calls.call(for: doorbell)?.talk.isActive == true && model.calls.echoCancellation(for: doorbell) != nil }
        when(talking, timeout: 2) { [self] in
            let state = model.calls.echoCancellation(for: doorbell)
            let note = model.calls.note(for: doorbell) ?? "-"
            let gateOn = model.calls.echoGateCancelled(doorbell)
            mic.speak(count: 16_000)
            when({ youFinals() > 0 }, timeout: 1) { [self] in
                let fullDuplex = youFinals()
                // Very loud: the short safety duck.
                fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.8, rms: 0.4))
                mic.speak(count: 16_000)
                // The samples were processed inside `speak`; any caption they
                // make is queued behind this hop.
                afterQueued { [self] in
                    let ducked = youFinals() == fullDuplex
                    model.calls.stopTalking(doorbell)
                    print("echo cancellation on: \(String(describing: state)), note \"\(note)\", captions while the doorbell speaks \(fullDuplex > 0), ducked when very loud \(ducked), prepared at call open \(prepared)")
                    if state != .system || gateOn != true { problems.append("the microphone's echo cancellation was not taken up by the call") }
                    if note != "Talking · Echo cancellation on" { problems.append("the call does not show that echo cancellation is on: \(note)") }
                    if fullDuplex == 0 { problems.append("with echo cancellation the microphone was not captioned while the doorbell spoke") }
                    if !ducked { problems.append("with echo cancellation a very loud doorbell did not duck the microphone's captions") }
                    if !prepared { problems.append("opening a call did not prepare echo cancellation") }
                    // The drained talk ends first (its end would stop the next one's microphone).
                    when({ model.calls.call(for: doorbell)?.talk.isTalking == false }, timeout: 3) { [self] in
                        echoFallbackStep(doorbell, tile: tile, allowed: allowed)
                    }
                }
            }
        }
    }

    /// The fallback: no echo canceller; the half-duplex caption gate.
    private func echoFallbackStep(_ doorbell: CameraRef, tile: CoreTile, allowed: Bool) {
        guard let model = app.model, let window, let fake = model.fake, let mic = model.calls.fakeMicrophone else {
            return checkLog()
        }
        mic.echo = .gated("self-test: no echo canceller")
        fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.3, rms: 0.1))
        func youFinals() -> Int { model.calls.call(for: doorbell)?.transcript.finals.filter { $0.source == .you }.count ?? 0 }
        let before = youFinals()
        model.calls.startTalking(doorbell)
        let talking: @MainActor () -> Bool = { model.calls.call(for: doorbell)?.talk.isActive == true && model.calls.echoCancellation(for: doorbell) != nil }
        when(talking, timeout: 2) { [self] in
            let pushedBefore = fake.talkPushedMilliseconds(tile: tile.id)
            let note = model.calls.note(for: doorbell) ?? "-"
            let gateOn = model.calls.echoGateCancelled(doorbell)
            mic.speak(count: 16_000)
            afterQueued { [self] in
                let gated = youFinals() == before
                let pushed = fake.talkPushedMilliseconds(tile: tile.id) - pushedBefore
                model.calls.stopTalking(doorbell)
                let log = model.alerts.log.map(\.text)
                let onLogged = log.filter { $0 == "Talk-back microphone: echo cancellation on" }.count
                let offLogged = log.filter { $0.hasPrefix("Talk-back microphone: echo cancellation unavailable (self-test: no echo canceller)") }.count
                print("echo cancellation off: note \"\(note)\", captions gated \(gated), \(Int(pushed)) ms still sent, logged on \(onLogged)x, off \(offLogged)x")
                if gateOn != false { problems.append("without echo cancellation the caption gate did not take over") }
                if note != "Talking · No echo cancellation" { problems.append("the call does not show that echo cancellation is off: \(note)") }
                if !gated { problems.append("without echo cancellation the microphone was captioned while the doorbell spoke") }
                if abs(pushed - 1000) > 1 { problems.append("without echo cancellation the microphone was not sent (\(pushed) ms)") }
                if onLogged != 1 || offLogged != 1 { problems.append("echo cancellation was not logged once per outcome") }
                fake.setAudioLevel(tile: tile.id, nil)
                mic.echo = .system
                model.setAllowTalk(doorbell, allowed)
                window.stage.endConversation(doorbell)
                if mic.prepared { problems.append("the last call ending did not release echo cancellation's preparation") }
                transcriptFileStep(doorbell)
            }
        }
    }

    /// "Save call transcripts" on: a call's transcript is written as a text
    /// file when it ends (in a temporary folder here); off: nothing is.
    private func transcriptFileStep(_ doorbell: CameraRef) {
        guard let model = app.model, let window, let fake = model.fake else { return checkLog() }
        let stage = window.stage
        // parity: call-transcript-file
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(
            "nvrview-selftest-transcripts-\(UUID().uuidString)", isDirectory: true)
        let before = model.transcripts.folder
        model.transcripts.folder = folder
        model.saveCallTranscripts = true
        stage.startConversation(doorbell)
        guard let tile = model.leases.tile(for: doorbell) else {
            problems.append("the conversation has no tile")
            return popOutStep(doorbell)
        }
        fake.pumpAudio(tile: tile.id, count: 17)
        when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
            stage.endConversation(doorbell)
            let names = model.transcripts.names()
            let text = names.first.flatMap { try? String(contentsOf: folder.appendingPathComponent($0), encoding: .utf8) } ?? ""
            let camera = CallTranscriptFile.sanitized(model.name(of: doorbell))
            print("transcript file: \(names), \(text.split(separator: "\n").count) lines")
            if names.count != 1 || names.first?.hasSuffix(" \(camera).txt") != true {
                problems.append("ending a call with saving on did not write one transcript file named for the camera: \(names)")
            }
            if !text.hasPrefix("NVRView call transcript\nCamera: \(model.name(of: doorbell))\n")
                || !text.contains("\nCall: conversation started in the app\n") || !text.contains(" Visitor: ")
            {
                problems.append("the transcript file lacks its header or the visitor's line")
            }
            // Off (the default): the next call writes nothing.
            model.saveCallTranscripts = false
            stage.startConversation(doorbell)
            fake.pumpAudio(tile: tile.id, count: 17)
            when({ model.calls.call(for: doorbell)?.transcript.finals.isEmpty == false }, timeout: 2) { [self] in
                stage.endConversation(doorbell)
                if model.transcripts.names().count != 1 { problems.append("a call with saving off wrote a transcript file") }
                try? FileManager.default.removeItem(at: folder)
                model.transcripts.folder = before
                popOutStep(doorbell)
            }
        }
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
            let drawn = c.stage.grid.renderer.tiles
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
            callTileStep(doorbell, c) { [self] in
                popOutTalkStep(doorbell, tile: tile, stages: stages, second: second, baseline: baseline)
            }
        }
    }

    /// The call window's picture is a regular tile (docs/CAPTURE.md "The
    /// call window is a regular tile"): a pinch zooms it, the zoom map
    /// shows, the renderer samples the zoomed region, Auto asks for the
    /// main stream, and the header's Reset Zoom eases it back; its menu is
    /// the header model's (Pop In, End Conversation, Record Call) with no
    /// layout items, and focus and split do nothing there.
    private func callTileStep(_ doorbell: CameraRef, _ c: CallWindowController, then next: @escaping @MainActor () -> Void) {
        guard let model = app.model else { return next() }
        let stage = c.stage
        draw(c)
        guard stage.fixedTile, stage.frames.leaves.count == 1, let leaf = stage.frames.leaves.first,
            leaf.camera == doorbell, let full = stage.renderedSource(of: leaf.path)
        else {
            problems.append("the call window's picture is not one tile of the doorbell")
            return next()
        }
        // parity: call-window-tile-zoom
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: false)
        stage.noteInteraction()
        draw(c)
        let zoomed = stage.zoom(of: doorbell)
        let map = stage.zoomMapShown(leaf.path) == true
        let source = stage.renderedSource(of: leaf.path)
        let narrower = source.map { $0.width < full.width * 0.9 || $0.height < full.height * 0.9 } == true
        let wantsMain = c.asksForMainStream && model.leases.wantedTier(doorbell) == model.leases.tiers(for: doorbell).focus
        let resetShown = stage.chromes[leaf.path]?.shownHeaderButtons.contains(.resetZoom) == true
        stage.chromes[leaf.path]?.pressHeader(.resetZoom)
        advance(1)
        draw(c)
        let back = stage.zoom(of: doorbell) == .none && !stage.isEasingZoom
        print(
            String(format: "call window tile: pinch to %.2fx, ", zoomed.scale)
                + "zoom map \(map), source cropped \(narrower), main stream \(wantsMain), "
                + "header Reset Zoom shown \(resetShown) and back to 1x \(back)")
        if abs(zoomed.scale - 2) > 0.01 { problems.append("a pinch did not zoom the call window's tile") }
        if !map { problems.append("the call window's zoomed tile showed no zoom map") }
        if !narrower { problems.append("the call window's renderer did not sample the zoomed region") }
        if !wantsMain { problems.append("the zoomed call window tile did not ask for the main stream") }
        if !resetShown || !back { problems.append("the call window's header did not offer Reset Zoom or did not reset") }
        // parity: call-window-tile-menu
        let menu = stage.contextMenu(for: leaf).items.map(\.title)
        let want = stage.headerTile(doorbell, path: leaf.path).map { TileHeaderLayout.menu(for: $0).map(\.label) } ?? []
        let callItems = ["Pop In Call", "End Conversation", "Record Call", "Save Picture"].allSatisfy(menu.contains)
        let layoutItems = ["Focus", "Split Side by Side", "Remove Tile", "Clear Tile", "Show Camera", "Record Main Stream"]
            .filter(menu.contains)
        stage.toggleFocus()
        stage.split(.row)
        let fixed = stage.state.focused == nil && stage.state.tree.leafCount == 1 && stage.selectedPath == leaf.path
        let header = stage.chromes[leaf.path]?.shownHeaderButtons ?? []
        c.captureRecord(nil)
        let toast = stage.toastText
        advance(ToastView.duration + 0.1)
        print(
            "call window tile menu: the model's \(want.count) items \(menu == want), call items \(callItems), "
                + "layout items \(layoutItems), focus and split ignored \(fixed), header \(header.map(\.rawValue)), ⇧⌘R \(toast ?? "nothing")")
        if menu != want || !callItems { problems.append("the call window tile's menu is not the header model's call menu: \(menu)") }
        if !layoutItems.isEmpty { problems.append("the call window tile's menu offers \(layoutItems)") }
        if !fixed { problems.append("focus or split changed the call window's single tile") }
        if !header.contains(.conversation) { problems.append("the doorbell's call window header has no conversation button") }
        if toast != "Record Call is coming soon" { problems.append("⇧⌘R in the call window did not reach Record Call") }
        next()
    }

    /// Lays out a call window and runs one display refresh's preparation
    /// (a window behind the others gets few refreshes).
    private func draw(_ c: CallWindowController) {
        c.window?.contentView?.layoutSubtreeIfNeeded()
        let grid = c.stage.grid
        let scale = c.window?.backingScaleFactor ?? 1
        let b = grid.bounds
        grid.prepare?(grid.renderer, CGSize(width: b.width * scale, height: b.height * scale))
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
            // Any stop or start would arrive as an event, queued behind this hop.
            afterQueued { [self] in
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

    /// Closing the call window and Esc in it end the call and close the
    /// window; the viewer windows keep nothing. A call in its own window
    /// never ends by inactivity; Pop In restarts the minute.
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
        // No expiry while popped out: ten minutes without activity, on the
        // manual clock, leave the call and its window open.
        window.stage.startConversation(doorbell)
        model.popOutCall(doorbell)
        advance(10 * DoorbellCall.idleTimeout)
        let stayed = calls.controller(for: doorbell) != nil && model.calls.call(for: doorbell) != nil
        print("call window after 10 min without activity: still open \(stayed)")
        if !stayed { problems.append("a call in its own window ended by itself without activity") }
        // Pop In restores the minute, starting fresh.
        model.popInCall(doorbell)
        advance(DoorbellCall.idleTimeout - 1)
        let fresh = model.calls.call(for: doorbell) != nil
        advance(2)
        print("popped in: open after 59 s \(fresh), ended after 61 s \(model.calls.call(for: doorbell) == nil)")
        if !fresh { problems.append("Pop In did not give the call a fresh minute") }
        ended("expiry after Pop In")
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
            fisheyePictureStep(path, camera)
        }
    }

    /// How many of the fisheye view's sample points show the fixture's color
    /// for the direction they look along (`pixel` reads the picture at
    /// whole-pixel positions of `rect`, where the view is drawn).
    private static func fisheyeMatches(
        _ dewarp: FisheyeDewarp, in rect: CGRect, pixel: (Int, Int) -> SIMD3<Int>
    ) -> (checked: Int, matched: Int) {
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
                if fixtureSector(pixel(Int(x), Int(y))) == Int(psi / 90) % 4 { matched += 1 }
            }
        }
        return (checked, matched)
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
        let (checked, matched) = Self.fisheyeMatches(dewarp, in: rect) { image.rgb(x: $0, y: $1) }
        print("fisheye \(mode.title): dewarped tiles drawn \(drawn), pixels matching the fixture \(matched) of \(checked)")
        if drawn < 1 { problems.append("\(mode.title): the dewarp pipeline drew nothing") }
        if checked < 20 || Double(matched) < 0.9 * Double(checked) {
            problems.append("\(mode.title): \(matched) of \(checked) pixels match the fixture")
        }
    }

    /// The zoomed tile's letterbox bar at 1x, in drawable pixels (the
    /// zoom-fills-tile check reads it at 2x and back at 1x).
    private var zoomBar: CGRect?

    /// One of the tile's bars at 1x (drawable pixels, a pixel in from its
    /// edges), or nil if the picture fills the tile.
    private static func bar(cell: CGRect, picture: CGRect) -> CGRect? {
        if picture.minY - cell.minY >= 4 {
            return CGRect(x: cell.minX + 2, y: cell.minY + 1, width: cell.width - 4, height: picture.minY - cell.minY - 2)
        }
        if picture.minX - cell.minX >= 4 {
            return CGRect(x: cell.minX + 1, y: cell.minY + 2, width: picture.minX - cell.minX - 2, height: cell.height - 4)
        }
        return nil
    }

    /// The share of `rect`'s pixels (sampled) that are not black, drawn
    /// offscreen now.
    private func litShare(_ rect: CGRect) -> Double? {
        guard let window else { return nil }
        let grid = window.stage.grid
        grid.prepare?(grid.renderer, grid.drawableSize)
        guard
            let image = grid.renderer.renderOffscreen(
                width: Int(grid.drawableSize.width), height: Int(grid.drawableSize.height))
        else { return nil }
        var lit = 0, n = 0
        let step = max(min(rect.width, rect.height) / 8, 1)
        for y in stride(from: rect.minY, to: rect.maxY, by: step) {
            for x in stride(from: rect.minX, to: rect.maxX, by: step) {
                let xi = Int(x), yi = Int(y)
                guard xi >= 0, yi >= 0, xi < image.width, yi < image.height else { continue }
                n += 1
                if image.rgb(x: xi, y: yi).wrappedSum() > 24 { lit += 1 }
            }
        }
        return n > 0 ? Double(lit) / Double(n) : nil
    }

    private var discoveryChecked = false
    private var retryChecked = false

    private func checkLog() {
        if !retryChecked, app.model?.fake != nil {
            retryChecked = true
            return retryStep()
        }
        if !recordingsChecked, app.model?.recordings.available == true {
            recordingsChecked = true
            return app.model?.fake != nil ? recordingsStep() : recordingsRealStep()
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

    // MARK: Recordings

    private var recordingsChecked = false

    /// What the recordings screens handed to the system (the Trash, the
    /// Finder) during the step: recorded, never done.
    private final class SystemCalls {
        var trashed: [URL] = []
        var revealed: [[URL]] = []
    }

    /// The recordings window (docs/UX.md "Recordings") against the fake
    /// core's made-up recording store: choosing cameras, the day asked for
    /// and today held under the window's own hold, the calendar and the
    /// timeline lanes, the animal filter, a cover thumbnail decoded, shrunk
    /// and put back, the edits, a lock, the downloads panel, camera groups,
    /// and the hold released when the window closes. The fake finishes
    /// fetches and downloads only when told (`completeRecordingWork`).
    private func recordingsStep() {
        guard let model = app.model, let fake = model.fake, let rec = model.recordings, rec.available,
            let device = model.devices.first?.key
        else { return checkLog() }
        let calls = SystemCalls()
        rec.trasher = { calls.trashed.append($0) }
        rec.revealer = { calls.revealed.append($0) }
        // parity: recordings-enable
        let cams = (0..<3).map { RecordingCamera(device: device, channel: $0) }
        for c in cams { rec.setEnabled(c, true) }
        if rec.enabledCameras != cams { problems.append("recordings: the chosen cameras are \(rec.enabledCameras)") }
        guard let controller = app.openRecordingsWindow() else {
            problems.append("recordings: no window opened")
            return checkLog()
        }
        let b = controller.browser
        // parity: recordings-hold-today
        when({ fake.recordingsHolds[b.holdID]?.count == 3 && b.lanes.count == 3 }, timeout: 3) { [self] in
            print("recordings: hold \(b.holdID) on \(fake.recordingsHolds[b.holdID]?.count ?? 0) cameras, \(b.lanes.count) lanes")
            if !b.holding || fake.recordingsHolds[b.holdID]?.count != 3 { problems.append("recordings: today is not held") }
            // parity: recordings-calendar-lanes
            when({ b.recordedDays.contains(b.day) && b.lanes.allSatisfy { !$0.spans.isEmpty } && b.dayIncidents.count == 7 },
                timeout: 3
            ) { [self] in
                let events = b.lanes.reduce(0) { $0 + $1.events.count }
                print("recordings: calendar \(b.recordedDays.count) days, \(events) events, \(b.dayIncidents.count) incidents")
                if !b.recordedDays.contains(b.day) { problems.append("recordings: the calendar does not mark today") }
                if events != 7 || b.dayIncidents.count != 7 {
                    problems.append("recordings: \(events) events and \(b.dayIncidents.count) incidents, expected 7 and 7")
                }
                if let lane = b.lanes.first, let e = lane.events.first {
                    b.select(event: e)
                    if b.selected?.involves(e.camera) != true { problems.append("recordings: a timeline click selected nothing") }
                }
                // parity: recordings-animal-filter
                b.kindFilters = [.animal]
                let animals = b.incidents.map(\.kinds)
                print("recordings: animal filter \(animals)")
                if animals != [["dog_cat"]] { problems.append("recordings: the animal filter shows \(animals)") }
                b.kindFilters = [.people, .vehicle]
                if b.incidents.count != 6 { problems.append("recordings: people and vehicles show \(b.incidents.count), expected 6") }
                b.kindFilters = []
                recordingsNavigate(b, fake, rec, calls)
            }
        }
    }

    /// A day before today is asked for but not held; back on today it is held again.
    private func recordingsNavigate(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-navigate-day
        let today = b.day
        b.previousDay()
        if b.holding || fake.recordingsHolds[b.holdID] != nil { problems.append("recordings: yesterday is held") }
        when({ b.dayIncidents.count == 7 && b.recordedDays.contains(today - DeviceTime.day) }, timeout: 3) { [self] in
            print("recordings: yesterday has \(b.dayIncidents.count) incidents")
            b.nextDay()
            if b.day != today || fake.recordingsHolds[b.holdID]?.count != 3 { problems.append("recordings: today is not held again") }
            when({ b.dayIncidents.count == 7 }, timeout: 3) { [self] in recordingsThumbnail(b, fake, rec, calls) }
        }
    }

    /// A cover thumbnail: pending, its event, the keyframe decoded and shrunk
    /// off the main thread, and the small image put back into the core's cache.
    private func recordingsThumbnail(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-thumbnail
        guard let incident = b.dayIncidents.first, let key = incident.coverKeys.first else {
            problems.append("recordings: no incident for a thumbnail")
            return recordingsEdits(b, fake, rec, calls)
        }
        rec.requestThumbnail(key)
        if rec.thumbnails[key] != .pending { problems.append("recordings: a thumbnail was not pending first") }
        fake.completeRecordingWork()
        when({
            if case .image = rec.thumbnails[key] { return true }
            if case .failed = rec.thumbnails[key] { return true }
            return false
        }, timeout: 5) { [self] in
            guard case .image(let jpeg) = rec.thumbnails[key] else {
                problems.append("recordings: the thumbnail is \(String(describing: rec.thumbnails[key]))")
                return recordingsEdits(b, fake, rec, calls)
            }
            let image = NSImage(data: jpeg)
            let cached = try? fake.thumbnailGet(key: key.json)
            var putBack = false
            if case .ready(let t)? = cached, t.kind == .image, t.data == jpeg { putBack = true }
            let size = image.map { "\(Int($0.size.width))x\(Int($0.size.height))" } ?? "none"
            print("recordings: thumbnail \(jpeg.count) bytes, \(size), put back \(putBack)")
            if image == nil || (image?.size.width ?? 0) > 320 { problems.append("recordings: the thumbnail image is \(size)") }
            if !putBack { problems.append("recordings: the small image was not put back in the core's cache") }
            recordingsEdits(b, fake, rec, calls)
        }
    }

    /// Rename, merge, split, add and remove a camera, and revert, through
    /// the browser as the window's controls call them.
    private func recordingsEdits(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-edits
        let people = b.dayIncidents.filter { $0.kinds == ["people"] }.sorted { $0.start < $1.start }
        guard people.count == 3 else {
            problems.append("recordings: \(people.count) people incidents, expected 3")
            return recordingsLock(b, fake, rec, calls)
        }
        b.rename(people[0].key, "Delivery")
        when({ b.dayIncidents.contains { $0.name == "Delivery" } }, timeout: 2) { [self] in
            b.selection = [people[0].key, people[1].key]
            b.merge()
            when({ b.dayIncidents.count == 6 }, timeout: 2) { [self] in
                guard let merged = b.selected, merged.cameras.count == 2 else {
                    problems.append("recordings: the merge did not make one incident of two cameras")
                    return recordingsLock(b, fake, rec, calls)
                }
                print("recordings: merged into \(merged.cameras.count) cameras, edited \(merged.edited)")
                b.split(merged.key, at: people[1].start)
                when({ b.dayIncidents.count == 7 }, timeout: 2) { [self] in
                    let first = people[0].key
                    b.add(RecordingCamera(device: people[2].cameras[0].camera.device, channel: 2), to: first)
                    when({ b.dayIncidents.first { $0.key == first }?.cameras.count == 2 }, timeout: 2) { [self] in
                        b.remove(RecordingCamera(device: people[2].cameras[0].camera.device, channel: 2), from: first)
                        when({ b.dayIncidents.first { $0.key == first }?.cameras.count == 1 }, timeout: 2) { [self] in
                            b.revert(first)
                            when({ b.dayIncidents.allSatisfy { !$0.edited } && b.dayIncidents.count == 7 }, timeout: 2) {
                                [self] in
                                print("recordings: rename, merge, split, add, remove, revert done")
                                if b.dayIncidents.contains(where: \.edited) { problems.append("recordings: revert left an edit") }
                                recordingsLock(b, fake, rec, calls)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Locking queues the incident's download and says so; unlocking keeps it.
    private func recordingsLock(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-lock
        guard let vehicle = b.dayIncidents.first(where: { $0.kinds == ["vehicle"] }) else {
            problems.append("recordings: no vehicle incident to lock")
            return recordingsDownloads(b, fake, rec, calls)
        }
        b.setLocked(vehicle.key, true)
        when({ b.dayIncidents.first { $0.key == vehicle.key }?.locked == true && rec.downloads.count == 1 }, timeout: 2) {
            [self] in
            print("recordings: locked; \(rec.notice ?? "no message"); downloads \(rec.downloads.count)")
            if rec.notice?.contains("downloading") != true { problems.append("recordings: locking did not say the video is kept") }
            b.lockedOnly = true
            if b.incidents.map(\.key) != [vehicle.key] { problems.append("recordings: the Locked filter shows \(b.incidents.count)") }
            b.lockedOnly = false
            b.setLocked(vehicle.key, false)
            when({ b.dayIncidents.first { $0.key == vehicle.key }?.locked == false }, timeout: 2) { [self] in
                if rec.downloads.count != 1 { problems.append("recordings: unlocking touched the download") }
                recordingsDownloads(b, fake, rec, calls)
            }
        }
    }

    /// The downloads panel: pause, resume, done, Show in Finder, forget,
    /// and the Trash once the core has let go of the folder.
    private func recordingsDownloads(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls
    ) {
        // parity: recordings-downloads
        guard let first = rec.downloads.first, let other = b.dayIncidents.first(where: { $0.kinds == ["dog_cat"] }) else {
            problems.append("recordings: nothing to download")
            return recordingsGroups(b, fake, rec, calls)
        }
        b.center = .downloads
        rec.pause(first.download, true)
        when({ rec.downloads.first?.paused == true }, timeout: 2) { [self] in
            fake.completeRecordingWork()
            if rec.downloads.first?.state == "done" { problems.append("recordings: a paused download finished") }
            rec.pause(first.download, false)
            when({ rec.downloads.first?.paused == false }, timeout: 2) { [self] in
                fake.completeRecordingWork()
                when({ rec.downloads.first?.state == "done" }, timeout: 2) { [self] in
                    guard let done = rec.downloads.first else { return }
                    rec.reveal(done)
                    if calls.revealed.last?.map(\.path) != done.outputs { problems.append("recordings: Show in Finder showed \(calls.revealed)") }
                    rec.forget(done.download)
                    b.download(other.key)
                    when({ rec.downloads.count == 1 && rec.downloads.first?.download != done.download }, timeout: 2) { [self] in
                        guard let queued = rec.downloads.first, let folder = queued.folder else { return }
                        rec.trash(queued.download)
                        when({ !calls.trashed.isEmpty }, timeout: 2) { [self] in
                            print("recordings: downloads done, forgotten, trashed \(calls.trashed.count)")
                            if calls.trashed.map(\.path) != [folder] { problems.append("recordings: the Trash got \(calls.trashed)") }
                            if !rec.downloads.isEmpty { problems.append("recordings: \(rec.downloads.count) downloads left") }
                            b.center = .player
                            recordingsGroups(b, fake, rec, calls)
                        }
                    }
                }
            }
        }
    }

    /// A camera group joins its cameras' overlapping events; the window
    /// closing releases its hold.
    private func recordingsGroups(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-groups
        let two = Array(b.cameras.prefix(2))
        rec.putGroups(IncidentGroups(groups: [.init(name: "Front", cameras: two)]))
        when({ rec.groups.groups.count == 1 && b.dayIncidents.count == 5 }, timeout: 2) { [self] in
            print("recordings: with a group of two, \(b.dayIncidents.count) incidents")
            if b.dayIncidents.count != 5 { problems.append("recordings: a group of two gave \(b.dayIncidents.count) incidents") }
            b.scope = .group("Front")
            if b.incidents.count != 2 { problems.append("recordings: the group scope shows \(b.incidents.count)") }
            b.scope = .all
            rec.putGroups(IncidentGroups())
            when({ rec.groups.groups.isEmpty && b.dayIncidents.count == 7 }, timeout: 2) { [self] in
                recordingsMonth(b, fake, rec)
            }
        }
    }

    /// Paging the calendar to the previous month asks for that month's
    /// calendar alone (minor 7) and marks its days; camera names are the
    /// camera list's.
    private func recordingsMonth(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: recordings-calendar-month
        let asked = fake.recordingsMonthRequests
        let shown = b.month
        b.showMonth(adding: -1)
        let previous = b.month
        when({ fake.recordingsMonthRequests > asked && !b.recordedDays.isEmpty }, timeout: 2) { [self] in
            let inMonth = b.recordedDays.allSatisfy { DeviceTime.month(containing: $0).start == previous }
            print("recordings: previous month asked \(fake.recordingsMonthRequests - asked) time(s), \(b.recordedDays.count) days marked")
            if fake.recordingsMonthRequests != asked + 1 { problems.append("recordings: paging asked \(fake.recordingsMonthRequests - asked) months") }
            if b.recordedDays.isEmpty || !inMonth { problems.append("recordings: the previous month shows \(b.recordedDays.count) days") }
            // Paging back and forth asks nothing again.
            b.showMonth(adding: 1)
            b.showMonth(adding: -1)
            if fake.recordingsMonthRequests != asked + 1 { problems.append("recordings: a month already asked was asked again") }
            b.showMonth(adding: 1)
            if b.month != shown { problems.append("recordings: the calendar did not come back to the shown day's month") }
            // parity: recordings-camera-names
            let names = b.lanes.map(\.name)
            let listed = b.cameras.map { c in rec.app?.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens)) ?? "" }
            print("recordings: camera names \(names)")
            if names != listed { problems.append("recordings: lanes are named \(names), the camera list \(listed)") }
            recordingsPlayback(b, fake, rec)
        }
    }

    /// The player (milestone P, M2): selecting an incident plays its cameras
    /// from its start less the margin; the pictures decode and reach the
    /// screen; pause, skip, speed (keyframes only and no sound from 2×), the
    /// grid's large and single views with the sound following, a lane click
    /// that seeks, Remove from Incident and Split Here at the playhead
    /// without reopening the playback, and the playback closed with the
    /// window.
    private func recordingsPlayback(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-open
        let p = b.player
        // The three cameras' people incidents (a second apart) as one.
        let people = b.dayIncidents.filter { $0.kinds == ["people"] }.map(\.key)
        guard people.count == 3 else {
            problems.append("recordings: \(people.count) people incidents to play, expected 3")
            return recordingsClose(b, fake)
        }
        b.selection = Set(people)
        b.merge()
        let opens = p.opens
        whenPlayback(fake, {
            p.playback != nil && p.cells.count == 3 && p.cells.allSatisfy { $0.tile.latestPicture() != nil }
                && (self.app.recordingsWindows.first?.stage?.framesDrawn ?? 0) > 0
        }, timeout: 5) { [self] in
            guard let incident = b.selected else { return failPlayback("the merge selected nothing", b, fake) }
            let start = (incident.start - incident.margin) * 1000
            print("recordings: playback \(p.playback ?? 0) of \(p.cells.count) cameras, \(p.state.map { "\($0)" } ?? "no state"), opens \(p.opens - opens + 1), frames drawn \(app.recordingsWindows.first?.stage?.framesDrawn ?? 0)")
            if p.playback == nil || p.cells.count != 3 { return failPlayback("the incident did not play its three cameras", b, fake) }
            if p.cells.contains(where: { $0.tile.latestPicture() == nil }) {
                let detail = p.cells.map { "\($0.camera.channel): \($0.state) \($0.tile.snapshot.status) configures \($0.tile.snapshot.configures)" }
                problems.append("recordings: a playback tile decoded no picture (\(detail))")
            }
            if (app.recordingsWindows.first?.stage?.framesDrawn ?? 0) == 0 { problems.append("recordings: the player drew no frame") }
            if p.positionMs < start || p.positionMs > start + 5_000 { problems.append("recordings: the playback starts at \(p.positionMs), not \(start)") }
            if !p.transport.isPlaying { problems.append("recordings: the selected incident is not playing") }
            if p.incidentKey != incident.key { problems.append("recordings: the player plays another incident") }
            playbackTransport(b, fake, rec)
        }
    }

    /// `when` for the playback steps: the fake core sends its queued
    /// playback events and starts its tiles' streams only when told
    /// (`Options.recordingWorkIsManual`), so each poll tells it.
    private func whenPlayback(
        _ fake: FakeCore, _ condition: @escaping @MainActor () -> Bool, timeout: Double,
        _ body: @escaping @MainActor () -> Void
    ) {
        when({
            fake.completePlaybackWork()
            return condition()
        }, timeout: timeout, body)
    }

    private func failPlayback(_ why: String, _ b: RecordingsBrowser, _ fake: FakeCore) {
        problems.append("recordings: " + why)
        recordingsClose(b, fake)
    }

    private func playbackTransport(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-transport
        let p = b.player
        let opens = p.opens
        p.pause()
        whenPlayback(fake, { p.state == .paused }, timeout: 2) { [self] in
            let before = p.positionMs
            p.skip(by: TransportState.skipMs)
            if p.positionMs != min(before + TransportState.skipMs, p.endMs) { problems.append("recordings: +10 s moved to \(p.positionMs) from \(before)") }
            p.setRate(4)
            let sound = p.cells.compactMap { fake.audioSetting(tile: $0.tile.id)?.audible }
            if PlaybackSpeed.hint(p.rate) != "Keyframes only, no sound" || sound.contains(true) {
                problems.append("recordings: at 4× the hint is \(PlaybackSpeed.hint(p.rate) ?? "none") and sound \(sound)")
            }
            p.setRate(1)
            p.play()
            whenPlayback(fake, { p.state == .playing && p.rate == 1 }, timeout: 2) { [self] in
                let audible = p.cells.map { fake.audioSetting(tile: $0.tile.id)?.audible ?? false }
                print("recordings: paused, skipped 10 s, 4× and back, playing; audible \(audible)")
                if audible != [true, false, false] { problems.append("recordings: at 1× the sound is \(audible), expected the first camera") }
                if p.opens != opens { problems.append("recordings: the transport reopened the playback") }
                playbackGrid(b, fake, rec)
            }
        }
    }

    private func playbackGrid(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-grid
        let p = b.player
        let opens = p.opens
        p.click(1)
        let emphasis = p.mode
        let afterClick = p.cells.map { fake.audioSetting(tile: $0.tile.id)?.audible ?? false }
        p.doubleClick(1)
        let single = p.mode
        p.doubleClick(1)
        print("recordings: grid click \(emphasis), double click \(single), back \(p.mode); sound \(afterClick)")
        if emphasis != .emphasis(1) || single != .single(1) || p.mode != .grid {
            problems.append("recordings: the grid went \(emphasis), \(single), \(p.mode)")
        }
        if afterClick != [false, true, false] { problems.append("recordings: the large camera's sound did not play (\(afterClick))") }
        p.muted = true
        if p.cells.contains(where: { fake.audioSetting(tile: $0.tile.id)?.audible == true }) { problems.append("recordings: muted, a camera is audible") }
        p.muted = false
        // parity: playback-scrub
        guard let lane = b.lanes.first(where: { p.cameras.contains($0.camera) }), let incident = b.playingIncident else {
            return failPlayback("no lane of a playing camera", b, fake)
        }
        let target = incident.end + 2
        let seeks = p.seeks
        b.laneClick(lane.camera, at: target)
        if p.seeks != seeks + 1 || p.positionMs != target * 1000 || p.opens != opens {
            problems.append("recordings: a lane click went to \(p.positionMs) (seeks \(p.seeks - seeks), opens \(p.opens - opens))")
        }
        whenPlayback(fake, { p.state == .playing && p.positionMs >= target * 1000 }, timeout: 2) { [self] in
            print("recordings: a lane click seeked to \(TransportState.clock(p.positionMs)) and plays")
            playbackEdits(b, fake, rec)
        }
    }

    private func playbackEdits(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-remove-camera
        let p = b.player
        let opens = p.opens
        guard let incident = b.playingIncident, incident.cameras.count == 3, let third = p.cameras.last else {
            return failPlayback("the playing incident does not have three cameras", b, fake)
        }
        b.remove(third, from: incident.key)
        whenPlayback(fake, { b.playingIncident?.cameras.count == 2 && p.cells.count == 2 }, timeout: 2) { [self] in
            print("recordings: removed a camera; the player has \(p.cells.count), opens \(p.opens - opens)")
            if p.cells.count != 2 || p.opens != opens || p.cameras.contains(third) {
                problems.append("recordings: Remove from Incident left \(p.cells.count) cameras, \(p.opens - opens) opens")
            }
            // parity: playback-split-here
            guard let playing = b.playingIncident else { return failPlayback("nothing plays after the removal", b, fake) }
            p.seek(to: (playing.start + 1) * 1000)
            let at = b.splitSecond(playing)
            if at != playing.start + 1 { problems.append("recordings: Split Here would split at \(String(describing: at))") }
            let before = b.dayIncidents.count
            b.splitHere(playing)
            whenPlayback(fake, { b.dayIncidents.count == before + 1 && p.cells.count == 1 }, timeout: 2) { [self] in
                print("recordings: split at the playhead; \(b.dayIncidents.count - before) more incident, the player has \(p.cells.count), opens \(p.opens - opens)")
                if b.dayIncidents.count != before + 1 { problems.append("recordings: Split Here made \(b.dayIncidents.count - before) more incidents") }
                if p.opens != opens || b.playingIncident == nil || b.playingIncident?.key != b.selected?.key {
                    problems.append("recordings: after Split Here the player reopened or lost its incident")
                }
                recordingsLayout(b, fake)
            }
        }
    }

    /// The window at its smallest and at a large size: the side panes give
    /// way in a narrow window (inspector first) and come back in a wide
    /// one, and the window keeps the size it was given. With
    /// NVRVIEW_RECORDINGS_SNAPSHOTS=<folder>, writes what it looked like.
    private func recordingsLayout(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-layout
        guard let window = app.recordingsWindows.first?.window else { return recordingsClose(b, fake) }
        let saved = window.frame
        let sizes = [RecordingsPanes.windowMin, CGSize(width: 900, height: 640), CGSize(width: 1400, height: 900)]
        var results: [String] = []
        func next(_ i: Int) {
            guard i < sizes.count else {
                window.setFrame(saved, display: false)
                print("recordings: layout \(results.joined(separator: "; "))")
                return recordingsWheelZoom(b, fake)
            }
            let size = sizes[i]
            let want = RecordingsPanes.shown(width: size.width, sidebar: true, inspector: true)
            window.setContentSize(size)
            // The panes follow the width SwiftUI lays out (on its next pass).
            when({ b.shownPanes?.width == size.width }, timeout: 2) { [self] in
                let got = window.contentLayoutRect.size
                let panes = b.shownPanes
                results.append(
                    "\(Int(size.width))×\(Int(size.height)): \(Int(got.width))×\(Int(got.height)), sidebar \(panes?.sidebar == true), inspector \(panes?.inspector == true)")
                if abs(got.width - size.width) > 1 || abs(got.height - size.height) > 1 {
                    problems.append("recordings: the window is \(got) when given \(size)")
                }
                if panes?.width != size.width || panes?.sidebar != want.sidebar || panes?.inspector != want.inspector {
                    problems.append("recordings: at \(size) the panes are \(String(describing: panes)), expected \(want)")
                }
                if let dir = ProcessInfo.processInfo.environment["NVRVIEW_RECORDINGS_SNAPSHOTS"] {
                    // The window as on screen (pictures included); needs the
                    // screen-recording permission, so only on request.
                    let path = URL(fileURLWithPath: dir).appendingPathComponent("recordings-\(Int(size.width))x\(Int(size.height)).png").path
                    let shot = Process()
                    shot.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
                    shot.arguments = ["-x", "-o", "-l", String(window.windowNumber), path]
                    try? shot.run()
                    shot.waitUntilExit()
                }
                next(i + 1)
            }
        }
        next(0)
    }

    /// The timeline lanes' wheel and trackpad, through the lanes' own event
    /// catcher with real scroll events: a mouse wheel notch zooms around the
    /// pointer (its time stays under it), many clamp at 48x, a horizontal
    /// two-finger scroll pans, a vertical one is left to the lanes'
    /// scrolling, and notches the other way go back to the whole day.
    private func recordingsWheelZoom(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-timeline-wheel-zoom
        func catcher(in view: NSView?) -> TimelineInputCatcher.CatcherView? {
            guard let view else { return nil }
            if let c = view as? TimelineInputCatcher.CatcherView { return c }
            for v in view.subviews { if let c = catcher(in: v) { return c } }
            return nil
        }
        let window = app.recordingsWindows.first?.window
        // The lanes are laid out again after the layout step's last resize.
        when({ catcher(in: window?.contentView).map { !$0.visibleRect.isEmpty } == true }, timeout: 2) { [self] in
            guard let c = catcher(in: window?.contentView), !c.visibleRect.isEmpty else {
                problems.append("recordings: no timeline lanes to scroll")
                return recordingsClose(b, fake)
            }
            recordingsWheelZoom(b, c)
            recordingsListen(b, fake)
        }
    }

    /// Listening for sounds: a camera's switch reaches `nv_sound_watch` and
    /// its `sound_watch` state comes back; a fifth camera of one device is
    /// refused; switching off takes it off the core's list.
    private func recordingsListen(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-listen-for-sounds
        let rec = b.model
        guard let device = b.cameras.first?.device else { return recordingsClose(b, fake) }
        let cams = (0..<5).map { RecordingCamera(device: device, channel: $0) }
        for c in cams.prefix(4) where !rec.setListening(c, true) {
            problems.append("recordings: listening to \(c) was refused (\(rec.notice ?? ""))")
        }
        if rec.setListening(cams[4], true) { problems.append("recordings: a fifth camera of a device listens") }
        if fake.soundWatched != Array(cams.prefix(4)) { problems.append("recordings: the core listens to \(fake.soundWatched)") }
        when({ rec.listenStates[cams[0]]?.state == "watching" }, timeout: 2) { [self] in
            print("recordings: listening to \(rec.listeningCameras.count) cameras, the fifth refused: \(rec.notice ?? "")")
            if rec.listenStates[cams[0]]?.state != "watching" { problems.append("recordings: no sound_watch state for a listening camera") }
            for c in cams.dropFirst() { rec.setListening(c, false) }
            if fake.soundWatched != [cams[0]] { problems.append("recordings: switching off left \(fake.soundWatched) listening") }
            recordingsLoudness(b, fake, listening: cams[0])
        }
    }

    /// The loudness strips: a listening camera's lane has loudness (the fake
    /// answers a steady level), the others are blank; zooming in asks again
    /// with a finer bucket.
    private func recordingsLoudness(_ b: RecordingsBrowser, _ fake: FakeCore, listening: RecordingCamera) {
        // parity: recordings-loudness-lanes
        b.timeline = TimelineViewport()
        b.reloadLoudness(force: true)
        let whole = b.loudnessBucket
        let heard = b.lanes.filter { b.loudness(of: $0.camera)?.heard == true }.map(\.camera)
        print("recordings: loudness at \(whole) s buckets, heard on \(heard)")
        if heard != [listening] { problems.append("recordings: loudness heard on \(heard), expected \(listening)") }
        if b.loudness(of: listening)?.bucket != whole { problems.append("recordings: the loudness lane is not at the bucket asked") }
        let reads = b.loudnessReads
        b.timeline = TimelineViewport(zoom: 48)
        if b.loudnessReads != reads + 1 || b.loudness(of: listening)?.bucket != b.loudnessBucket || b.loudnessBucket >= whole {
            problems.append("recordings: zooming in did not ask for finer loudness (\(b.loudnessBucket) s)")
        }
        b.timeline = TimelineViewport()
        recordingsSoundLabels(b, fake, camera: listening)
    }

    /// Sound labels (as the app's classifier would put them): a label becomes
    /// an event from sound on its camera's lane and an incident of its own,
    /// with the sound badge, which the Sounds filter alone shows.
    private func recordingsSoundLabels(_ b: RecordingsBrowser, _ fake: FakeCore, camera: RecordingCamera) {
        // parity: recordings-sound-labels
        let at = b.day + 10 * 3600
        let json = #"{"camera":\#(camera.json),"analyzer":"selftest-v1","start":\#(at - 60),"end":\#(at + 60),"labels":[{"kind":"dog_bark","start":\#(at),"end":\#(at + 4),"confidence":0.9,"label":"dog_bark"}]}"#
        do throws(CoreStatus) {
            try fake.soundLabelsPut(json: json)
        } catch {
            problems.append("recordings: the sound labels were refused (status \(error.code))")
            return recordingsClose(b, fake)
        }
        when({ b.dayIncidents.contains { $0.kinds == ["dog_bark"] } }, timeout: 2) { [self] in
            let lane = b.lanes.first { $0.camera == camera }
            let onLane = lane?.events.contains { $0.kind == "dog_bark" && $0.source == SoundKinds.localAudio } == true
            b.kindFilters = [.sound]
            let shown = b.incidents.map(\.kinds)
            let badge = b.incidents.first.map { SoundKinds.names($0.events) } ?? []
            print("recordings: a sound label on the lane \(onLane), the Sounds filter shows \(shown), badge \(badge)")
            if !onLane { problems.append("recordings: the sound label is not on its camera's lane") }
            if shown != [["dog_bark"]] { problems.append("recordings: the Sounds filter shows \(shown)") }
            if badge != ["Dog barking"] { problems.append("recordings: the sound badge says \(badge)") }
            b.kindFilters = []
            b.model.setListening(camera, false)
            recordingsClose(b, fake)
        }
    }

    private func recordingsWheelZoom(_ b: RecordingsBrowser, _ c: TimelineInputCatcher.CatcherView) {
        func scroll(_ units: CGScrollEventUnit, dy: Int32, dx: Int32 = 0) -> NSEvent? {
            CGEvent(scrollWheelEvent2Source: nil, units: units, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0)
                .flatMap(NSEvent.init(cgEvent:))
        }
        b.timeline = TimelineViewport()
        let at = NSPoint(x: c.bounds.width * 0.25, y: c.visibleRect.midY)
        let before = b.timeline.fraction(atPointer: 0.25)
        guard let notch = scroll(.line, dy: 1), let input = TimelineInput(notch), c.handle(notch, at: at) else {
            problems.append("recordings: a wheel notch over the lanes was not taken")
            return
        }
        let one = b.timeline
        print("recordings: a wheel notch (\(notch.scrollingDeltaY) lines) zooms to \(one.zoom)x, input \(input)")
        if case .zoom(_, true) = input {} else { problems.append("recordings: a wheel notch did not zoom eased (\(input))") }
        if one.zoom <= 1 || abs(one.fraction(atPointer: 0.25) - before) > 1e-9 {
            problems.append("recordings: a wheel notch made \(one), the time under the pointer moved")
        }
        for _ in 0..<40 { if let e = scroll(.line, dy: 3) { _ = c.handle(e, at: at) } }
        if b.timeline.zoom != TimelineViewport.maxZoom { problems.append("recordings: the wheel zoomed to \(b.timeline.zoom)x, not 48x") }
        let zoomed = b.timeline
        if let pan = scroll(.pixel, dy: 0, dx: -40), c.handle(pan, at: at) {
            if b.timeline.start <= zoomed.start { problems.append("recordings: a two-finger scroll left did not pan later") }
        } else {
            problems.append("recordings: a horizontal two-finger scroll was not taken")
        }
        if let vertical = scroll(.pixel, dy: -40), c.handle(vertical, at: at) {
            problems.append("recordings: a vertical two-finger scroll was taken from the lanes' scrolling")
        }
        for _ in 0..<40 { if let e = scroll(.line, dy: -3) { _ = c.handle(e, at: at) } }
        print("recordings: wheel zoom to 48x panned from \(zoomed.start) and back to \(b.timeline)")
        if b.timeline != TimelineViewport() { problems.append("recordings: wheeling back left \(b.timeline)") }
    }

    private func recordingsClose(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-window-close
        let hold = b.holdID
        app.recordingsWindows.first?.window?.close()
        if fake.recordingsHolds[hold] != nil { problems.append("recordings: closing the window kept its hold") }
        if !app.recordingsWindows.isEmpty { problems.append("recordings: the window is still listed") }
        // parity: playback-close
        if !fake.openPlaybacks.isEmpty || b.player.playback != nil {
            problems.append("recordings: closing the window left playbacks \(fake.openPlaybacks) open")
        }
        checkLog()
    }

    /// The recordings window on the real core, against a device with
    /// recordings (tools/mock-nvr `--recordings 3`; never a real NVR in a
    /// self-test): every camera of the first device chosen, today asked for
    /// and held, a lane per camera with spans and the day's incidents from
    /// the device's search, a cover keyframe decoded with VideoToolbox and
    /// put back, yesterday asked for and not held, a lock that downloads an
    /// incident to the end, the Trash hand-over (recorded, not done), and the
    /// hold released when the window closes. Skipped for a device without
    /// recordings.
    private func recordingsRealStep() {
        // Only against a device on this computer (the mock): locking
        // downloads, and a self-test pointed at a real NVR must not.
        guard let model = app.model, let rec = model.recordings, rec.available,
            let device = model.devices.first(where: { $0.link.state == .connected && !$0.channels.isEmpty }),
            ["127.0.0.1", "localhost", "::1"].contains(device.config.host)
        else { return checkLog() }
        // parity: recordings-real-core
        let calls = SystemCalls()
        rec.trasher = { calls.trashed.append($0) }
        rec.revealer = { calls.revealed.append($0) }
        let cams = device.channels.map { RecordingCamera(device: device.key, channel: $0.channel) }
        rec.setEnabled(device: device.key, cameras: cams)
        guard let controller = app.openRecordingsWindow() else {
            problems.append("recordings: no window opened")
            return checkLog()
        }
        let b = controller.browser
        let ready = { b.lanes.count == cams.count && b.lanes.allSatisfy { !$0.spans.isEmpty } && !b.dayIncidents.isEmpty }
        when({ ready() && !rec.searching.busy }, timeout: 30) { [self] in
            print("recordings (real core): \(b.lanes.map(\.spans.count)) spans, \(b.dayIncidents.count) incidents, holding \(b.holding)")
            guard ready() else {
                problems.append("recordings: the device's day did not arrive (\(b.lanes.map(\.spans.count)) spans)")
                return recordingsRealClose(b)
            }
            if !b.holding { problems.append("recordings: today is not held") }
            guard let key = b.dayIncidents.first?.coverKeys.first else { return recordingsRealClose(b) }
            rec.requestThumbnail(key)
            when({
                switch rec.thumbnails[key] {
                case .image, .failed, .empty: true
                default: false
                }
            }, timeout: 30) { [self] in
                var putBack = false
                if case .ready(let t)? = try? model.bridge.recordings().thumbnailGet(key: key.json), t.kind == .image {
                    putBack = true
                }
                var bytes = 0
                if case .image(let jpeg) = rec.thumbnails[key] { bytes = jpeg.count }
                print("recordings (real core): thumbnail image \(bytes) bytes, put back \(putBack)")
                if !putBack { problems.append("recordings: the cover keyframe was not decoded and put back") }
                recordingsRealPlayback(b, rec, calls)
            }
        }
    }

    /// The player on the real core: the day's first finished incident plays
    /// through the playback engine against the mock, its pictures decode
    /// on the tile pipeline, the clock moves, and pausing holds it.
    private func recordingsRealPlayback(_ b: RecordingsBrowser, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: playback-real-core
        let p = b.player
        guard let incident = b.dayIncidents.first(where: { !$0.growing }) ?? b.dayIncidents.first else {
            problems.append("recordings: no incident to play on the real core")
            return recordingsRealDownload(b, rec, calls)
        }
        b.click(incident.key, extend: false)
        var first: Int64?
        when({
            guard p.playback != nil, p.cells.allSatisfy({ $0.tile.latestPicture() != nil }) else { return false }
            if first == nil { first = p.positionMs }
            return p.positionMs > (first ?? 0) + 500
        }, timeout: 30) { [self] in
            print("recordings (real core): playback of \(p.cells.count) camera(s), \(p.cells.map(\.state)), at \(TransportState.clock(p.positionMs)), moved \(p.positionMs - (first ?? p.positionMs)) ms")
            if p.playback == nil || p.cells.contains(where: { $0.tile.latestPicture() == nil }) {
                problems.append("recordings: the real core's playback showed no picture (\(p.message ?? "no message"))")
            } else if p.positionMs <= (first ?? 0) + 500 {
                problems.append("recordings: the real core's playback clock did not move")
            }
            p.pause()
            when({ p.state == .paused }, timeout: 10) { [self] in
                if p.state != .paused { problems.append("recordings: the real core's playback did not pause") }
                recordingsRealDownload(b, rec, calls)
            }
        }
    }

    private func recordingsRealDownload(_ b: RecordingsBrowser, _ rec: RecordingsModel, _ calls: SystemCalls) {
        b.previousDay()
        if b.player.playback != nil { problems.append("recordings: another day kept the playback open") }
        if b.holding { problems.append("recordings: yesterday is held") }
        when({ b.dayIncidents.contains { !$0.growing } && !rec.searching.busy }, timeout: 30) { [self] in
            guard let incident = b.dayIncidents.first(where: { !$0.growing }) else {
                problems.append("recordings: yesterday has no incidents")
                return recordingsRealClose(b)
            }
            b.setLocked(incident.key, true)
            when({ rec.downloads.contains { $0.state == "done" || $0.state == "failed" } }, timeout: 60) { [self] in
                guard let d = rec.downloads.first(where: { $0.incident == incident.key }) else {
                    problems.append("recordings: locking downloaded nothing")
                    return recordingsRealClose(b)
                }
                print("recordings (real core): lock \(rec.notice ?? ""), download \(d.stateText), \(d.outputs.count) files")
                if d.state != "done" || d.outputs.isEmpty { problems.append("recordings: the download ended \(d.stateText)") }
                rec.trash(d.download)
                when({ !calls.trashed.isEmpty }, timeout: 10) { [self] in
                    if calls.trashed.map(\.path) != [d.folder ?? ""] { problems.append("recordings: the Trash got \(calls.trashed)") }
                    b.setLocked(incident.key, false)
                    recordingsRealClose(b)
                }
            }
        }
    }

    private func recordingsRealClose(_ b: RecordingsBrowser) {
        app.recordingsWindows.first?.window?.close()
        if b.holding { problems.append("recordings: closing the window kept its hold") }
        checkLog()
    }

    private func checkLogNow() {
        // parity: updates-notes-scroll
        problems += UpdateNotesSelfTest.run()
        // parity: updates-bundled-defaults
        if let model = app.model { problems += UpdateDefaultsSelfTest.run(bridge: model.bridge) }
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
