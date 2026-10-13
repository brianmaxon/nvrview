import AppKit
import AVFoundation
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
/// 14g. the on-device detector (docs/VISION.md, "Implementation status"): with
///    the model present (`NVRVIEW_DETECTOR_MODEL`), the benchmark runs on the
///    Neural Engine and the CPU and a detection goes through the queue off the
///    main thread; without the model the step says so and passes;
/// 15. AppKit logged none of the warnings in `forbiddenLog` during the run.
///
/// The viewer's clock is a manual one here (`AppDelegate.testClock`):
/// animations, easing and the idle delay finish when the test advances it,
/// not after real time. What really takes time (frames decoding, tier
/// switches, display refreshes, drawable resizes) is waited for by polling
/// for the condition (or a count of pictures), with a timeout. The one fixed
/// wait left is the launch step's 1.5 s: it shows that AppKit's own hover
/// timer, which the test cannot advance, does not peek the sidebar.
///
/// The steps are in areas (scripts/macos-selftest-areas.txt; `--areas a,b`
/// runs only those and the startup smoke, so the test result cache reruns
/// what a change touches). This file has the order and the shared helpers;
/// the larger areas' steps are in SelfTest+Area.swift files.
///
/// Every step carries a parity marker (a comment naming a kebab-case ID) above its code, shared with
/// the Windows self-test where it checks the same thing (docs/PARITY.md;
/// `scripts/check-parity.py` compares them).
///
/// Prints what it measured, each line with the seconds since the start, and
/// exits 0 or 1.

/// When the app started (host seconds): every line the self-test prints
/// starts with the seconds since, so a run shows where its time went.
nonisolated(unsafe) var selfTestStarted = CACurrentMediaTime()

/// The self-test's own `print`: the elapsed seconds in front of the line
/// (the verdict lines at the end use `Swift.print`, unprefixed).
func selfTestPrint(_ line: String) {
    Swift.print(String(format: "[%6.2f] ", CACurrentMediaTime() - selfTestStarted) + line)
}

private func print(_ line: String) { selfTestPrint(line) }

@MainActor
final class SelfTest {
    unowned let app: AppDelegate
    var problems: [String] = []
    /// Viewer windows a step opened besides the second (closed with it).
    var extraWindows: [ViewerWindowController] = []
    let began = CACurrentMediaTime()
    /// The areas asked for with `--areas` (nil: all).
    let areas: Set<String>?

    /// The self-test's areas: scripts/macos-selftest-areas.txt puts every
    /// step in exactly one, and scripts/suite-cache.py fingerprints each.
    /// "startup" is the smoke that always runs.
    static let areaNames = [
        "startup", "viewer", "tiles", "layouts", "header", "settings", "calls", "recordings", "capture", "ptz", "alerts",
    ]

    init(app: AppDelegate, areas: Set<String>? = nil) {
        self.app = app
        self.areas = areas
    }

    /// True when the run covers the area (without --areas, all of them).
    func runs(_ area: String) -> Bool { area == "startup" || areas?.contains(area) ?? true }

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
        selfTestStarted = began
        if let unknown = areas?.subtracting(Self.areaNames), !unknown.isEmpty {
            finish(["unknown self-test areas: \(unknown.sorted().joined(separator: ", ")) (known: \(Self.areaNames.joined(separator: ", ")))"])
        }
        print("areas: \(areas == nil ? "all" : Self.areaNames.filter(runs).joined(separator: ", "))")
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
    func launchPeekStep(then next: @escaping @MainActor () -> Void) {
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
    func advance(_ seconds: Double) {
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
    var gridReady: Bool {
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

    func after(_ seconds: Double, _ body: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated { body() } }
    }

    /// Runs `body` after everything already queued on the main queue (the
    /// hops a fake's events and callbacks make), instead of after a
    /// guessed delay: the queue is first in, first out.
    func afterQueued(_ body: @escaping @MainActor () -> Void) {
        DispatchQueue.main.async { MainActor.assumeIsolated { body() } }
    }

    var window: ViewerWindowController? { app.windows.first }

    /// Runs `body` once `condition` holds or `timeout` seconds have passed
    /// (a window behind others gets few display refreshes, and animations
    /// advance only on refreshes).
    func when(
        _ condition: @escaping @MainActor () -> Bool, timeout: Double, _ body: @escaping @MainActor () -> Void
    ) {
        let deadline = CACurrentMediaTime() + timeout
        func poll() {
            if condition() || CACurrentMediaTime() >= deadline { return body() }
            after(0.02) { poll() }
        }
        poll()
    }

    func checkGrid() {
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
        capture("grid") { [self] in
            if runs("viewer") { livePacing { [self] in resizeStep() } } else { resizeStep() }
        }
    }

    /// Live pacing (docs/RENDERING.md "Playback pacing"): until every live
    /// tile of the first window has been delivered `livePacingPictures`
    /// pictures (a count, not a time), each tile notes when each picture
    /// reaches the app, and the window's renderer counts the pictures it
    /// draws for the first time; together the tiles must draw at least 90%
    /// of the pictures delivered (what the header's "Device" and "Shown"
    /// rates compare). The pictures come in at the stream's own pace, so this
    /// takes a second or two; the release pacing itself is proven on a
    /// simulated clock in the core's tests (tests/core.rs, live pictures
    /// delivered in groups) and the bounds in PacingStatsTests.
    func livePacing(then next: @escaping @MainActor () -> Void) {
        // parity: live-pacing
        guard let model = app.model, let window else { return next() }
        let tiles = window.state.tree.cameras.compactMap { model.leases.tile(for: $0) }
        let renderer = window.stage.grid.renderer
        var shown = 0
        renderer.onNewPicture = { _, _, _ in shown += 1 }
        for t in tiles {
            t.pacing.reset()
            t.pacing.enabled = true
        }
        // A window behind others gets few display refreshes: in front (not
        // activated) while measuring, then behind again.
        window.window?.orderFrontRegardless()
        let refreshes = window.stage.grid.refreshes
        // How busy the main thread is (the display link runs on it): a 100 Hz timer's longest gap.
        var lastTick = CACurrentMediaTime(), longest = 0.0, ticks = 0
        let probe = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { _ in
            MainActor.assumeIsolated {
                let now = CACurrentMediaTime()
                longest = max(longest, now - lastTick)
                lastTick = now
                ticks += 1
            }
        }
        RunLoop.main.add(probe, forMode: .common)
        let began = CACurrentMediaTime()
        when({ tiles.allSatisfy { $0.pacing.report().released.pictures >= Self.livePacingPictures } }, timeout: 15) { [self] in
            probe.invalidate()
            let seconds = CACurrentMediaTime() - began
            print(String(format: "live pacing: main thread %d timer ticks in %.2f s, longest gap %.0f ms; window visible %@", ticks, seconds, longest * 1000,
                         window.window?.occlusionState.contains(.visible) == true ? "yes" : "no"))
            window.window?.orderBack(nil)
            renderer.onNewPicture = nil
            let reports = tiles.map { t in
                t.pacing.enabled = false
                return t.pacing.report()
            }
            let arrived = reports.map(\.released.pictures).reduce(0, +)
            let ratio = arrived > 0 ? Double(shown) / Double(arrived) : 0
            let made = window.stage.grid.refreshes - refreshes
            let first = reports.first.map { String(describing: $0) } ?? "none"
            print("live pacing: \(tiles.count) tiles, \(arrived) pictures delivered, \(shown) drawn (\(String(format: "%.2f", ratio))), \(made) display refreshes in \(String(format: "%.2f", seconds)) s; first tile: \(first)")
            if ratio < Self.pacingShownRatio {
                problems.append("live pacing: \(shown) of \(arrived) delivered pictures were drawn")
            }
            next()
        }
    }

    /// Resizes the window in a burst of steps, one per run-loop turn (each
    /// costs a layout pass), like a full-screen transition: the grid keeps drawing while the drawable
    /// size follows only now and then, and exactly once it settles.
    func resizeStep() {
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
    static func pixelSize(of view: NSView) -> CGSize {
        let scale = view.window?.backingScaleFactor ?? 1
        return CGSize(width: (view.bounds.width * scale).rounded(), height: (view.bounds.height * scale).rounded())
    }

    func focusStep() {
        guard let model = app.model, let window else { finish(problems) }
        guard let leaf = window.state.tree.leaves.first(where: { $0.camera != nil }), let camera = leaf.camera else {
            finish(problems + ["no camera to focus"])
        }
        guard runs("viewer") else { return qualityStep(camera) }
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

    func listenStep(_ camera: CameraRef) {
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

    func unfocusStep(_ camera: CameraRef) {
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
            qualityStep(camera)
        }
    }

    /// Clear on a tile switches its camera to the main stream in the grid;
    /// back to Auto returns it to the sub stream.
    func qualityStep(_ camera: CameraRef) {
        guard let model = app.model, let window else { finish(problems) }
        guard let leaf = window.state.tree.leaves.first(where: { $0.camera == camera }) else { return sidebarStep() }
        guard runs("tiles") else { return sidebarStep() }
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
    func sidebarStep() {
        guard let model = app.model else { finish(problems) }
        // Only with `--fake`: never touch the user's stored groups.
        guard model.fake != nil else { return checkLog() }
        guard runs("layouts") else { return lensStep() }
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
    func revealStep() {
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
    func lensStep() {
        guard let model = app.model else { finish(problems) }
        guard runs("tiles") else { return keyStep() }
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
    func press(_ characters: String, keyCode: UInt16) {
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

    func keyStep() {
        guard let window else { finish(problems) }
        guard runs("viewer") else { return headerStep() }
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

    func zoomStep(_ path: NodePath) {
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
            let z = stage.zoom(atTile: path)
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

    func chromeStep(_ path: NodePath, _ camera: CameraRef) {
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
                let z = stage.zoom(atTile: path)
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
    func fillPanStep(_ path: NodePath, _ camera: CameraRef) {
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
        let single = stage.fills(atTile: path) && !stage.fills(atTile: other.path) && fillsMenu.state == .mixed
        let checked = stage.contextMenu(for: tile).items.first { $0.title == "Fill Tile" }?.state == .on
        print("fill per tile: this tile fills \(stage.fills(atTile: path)), another \(stage.fills(atTile: other.path)), Fill Tiles \(fillsMenu.state == .mixed ? "mixed" : "not mixed"), item checked \(checked)")
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
        let z = stage.zoom(atTile: path)
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
        let edge = stage.zoom(atTile: path).visible(in: base)
        let stopped = wide ? abs(edge.minX) < 1e-6 : abs(edge.minY) < 1e-6
        let panned = stage.isPanned(path) && !stage.zoom(atTile: path).isZoomed
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
    func fillPanHeaderSwapStep(_ path: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        // parity: fill-pan-header-swap
        guard let tile = stage.frames.leaf(path), let target = stage.frames.leaves.first(where: { $0.camera == otherCamera })
        else {
            problems.append("no tiles to swap")
            return headerStep()
        }
        let zoom = stage.zoom(atTile: path)
        stage.showHeader(path)
        let onHeader = CGPoint(x: tile.rect.midX, y: tile.rect.minY + 10)
        let kind = stage.dragKind(at: onHeader)
        stage.simulateDrag(from: onHeader, to: CGPoint(x: target.rect.midX, y: target.rect.midY))
        let swapped = window.state.tree[path]?.camera == otherCamera && window.state.tree[target.path]?.camera == camera
        let kept = stage.zoom(atTile: target.path) == zoom && stage.fills(atTile: target.path)
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

    /// The window's record keeps the tile's crop, zoom and Fill choice
    /// under the tile's identity; a window restored from it shows the same
    /// part of the picture in that tile. Then the window's switch sets
    /// every tile, and Reset Position recenters.
    func fillPanRestoreStep(_ path: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window, let saved = window.savedWindow,
            let otherPath = window.state.tree.leaves.first(where: { $0.camera == otherCamera })?.path,
            let key = window.state.viewKey(path), let otherKey = window.state.viewKey(otherPath)
        else {
            problems.append("no saved window or tiles for the restore step")
            return headerStep()
        }
        let stage = window.stage
        // parity: fill-pan-restore
        let record = (try? JSONEncoder().encode([saved])).flatMap { try? JSONDecoder().decode([SavedWindow].self, from: $0) }?.first
        let zoom = stage.zoom(atTile: path)
        let readsBack = record.map {
            $0.views.zoom(key) == zoom && $0.views.fills(key) && !$0.views.fills(otherKey)
                && $0.tileViews.contains { $0.key.tile == key.tile && $0.key.layout == window.state.viewsKey }
        } == true
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
            let same = rs.zoom(atTile: path) == zoom && rs.fills(atTile: path) && !rs.fills(atTile: otherPath)
            let shown = rs.renderedSource(of: path)
            let sameCrop = source.flatMap { a in shown.map { b in abs(a.minX - b.minX) < 2 && abs(a.minY - b.minY) < 2 && abs(a.width - b.width) < 2 } } == true
            restored.close()
            print("fill pan restore: record reads back per tile \(readsBack), restored window same settings \(same), same crop \(sameCrop)")
            if !readsBack { problems.append("the window's saved record lost the tile's crop or Fill choice") }
            if !same { problems.append("a window restored from the record did not get the tile's crop and Fill choice back") }
            if !sameCrop { problems.append("a restored window does not show the same part of the picture") }
            // parity: fill-window-switch
            // The window's switch: every tile fills, then every tile fits.
            window.toggleFill(nil)
            let allFill = stage.fills(atTile: path) && stage.fills(atTile: otherPath) && !stage.fillMixed
            window.toggleFill(nil)
            let allFit = !stage.fills(atTile: path) && !stage.fills(atTile: otherPath) && !stage.fillMixed
            // The crop stays with the tile; Reset Position recenters it.
            stage.setFill(true, atTile: path)
            var reset = false
            if let tile = stage.frames.leaf(path) {
                let menu = stage.contextMenu(for: tile)
                if let i = menu.items.firstIndex(where: { $0.title == "Reset Position" }) {
                    menu.performActionForItem(at: i)
                    reset = stage.zoom(atTile: path) == .none
                }
            }
            print("fill per tile: Fill Tiles fills every tile \(allFill), then fits every tile \(allFit); Reset Position \(reset)")
            if !allFill || !allFit { problems.append("Fill Tiles did not set every tile of the window") }
            if !reset { problems.append("Reset Position did not recenter the filled crop") }
            stage.setFill(false, atTile: path)
            zoomPerTileStep(path, otherPath, camera, otherCamera)
        }
    }

    // MARK: Zoom per tile (docs/UX.md "Digital zoom")

    /// Two tiles of one camera are independent views: Show Camera puts the
    /// camera in a second tile at the default view; each tile zooms on its
    /// own, focus keeps a tile's zoom, and resetting one leaves the other.
    func zoomPerTileStep(_ path: NodePath, _ second: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        let grid = stage.grid
        func middle(_ p: NodePath) -> CGPoint? { stage.frames.leaf(p).map { CGPoint(x: $0.rect.midX, y: $0.rect.midY) } }
        // parity: zoom-per-tile
        stage.assign(camera, at: second)
        let fresh = window.state.tree[second]?.camera == camera && stage.zoom(atTile: second) == .none
        guard let a = middle(path), let b = middle(second) else {
            problems.append("no tiles to zoom one by one")
            return headerStep()
        }
        stage.zoom(at: a, by: 2, eased: false)
        stage.zoom(at: b, by: 3, eased: false)
        grid.prepare?(grid.renderer, grid.drawableSize)
        let za = stage.zoom(atTile: path).scale, zb = stage.zoom(atTile: second).scale
        let sa = stage.renderedSource(of: path), sb = stage.renderedSource(of: second)
        let independent = abs(za - 2) < 0.01 && abs(zb - 3) < 0.01 && sa != nil && sb != nil && sa != sb
        stage.focus(second)
        advance(StageView.focusDuration)
        let focusKeeps = abs(stage.zoom(atTile: second).scale - 3) < 0.01
        stage.unfocus()
        advance(StageView.focusDuration)
        let unfocusKeeps = abs(stage.zoom(atTile: second).scale - 3) < 0.01 && abs(stage.zoom(atTile: path).scale - 2) < 0.01
        stage.resetZoom(at: second, eased: false)
        let resetOne = stage.zoom(atTile: second) == .none && abs(stage.zoom(atTile: path).scale - 2) < 0.01
        print(
            String(format: "zoom per tile: one camera in two tiles at %.2fx and %.2fx, ", za, zb)
                + "Show Camera starts at 1x \(fresh), sources differ \(sa != sb), focus keeps \(focusKeeps), "
                + "unfocus keeps \(unfocusKeeps), reset one tile only \(resetOne)")
        if !fresh { problems.append("Show Camera did not start the tile at the default view") }
        if !independent { problems.append("two tiles of one camera did not zoom independently") }
        if !focusKeeps || !unfocusKeeps { problems.append("focus lost a tile's own zoom") }
        if !resetOne { problems.append("resetting one tile's zoom changed another tile of the same camera") }
        zoomFollowsSwapStep(path, second, camera, otherCamera)
    }

    /// Swapping two tiles moves each view with its camera; the tiles keep
    /// their identities. Then the layout is put back as it was.
    func zoomFollowsSwapStep(_ path: NodePath, _ second: NodePath, _ camera: CameraRef, _ otherCamera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        // parity: zoom-follows-swap
        stage.assign(otherCamera, at: second)
        let showReset = stage.zoom(atTile: second) == .none
        if let b = stage.frames.leaf(second) {
            stage.zoom(at: CGPoint(x: b.rect.midX, y: b.rect.midY), by: 3, eased: false)
        }
        let ids = (window.state.tree[path]?.tileID, window.state.tree[second]?.tileID)
        stage.swapTiles(path, second)
        let moved = window.state.tree[second]?.camera == camera && abs(stage.zoom(atTile: second).scale - 2) < 0.01
            && window.state.tree[path]?.camera == otherCamera && abs(stage.zoom(atTile: path).scale - 3) < 0.01
        let idsStay = window.state.tree[path]?.tileID == ids.0 && window.state.tree[second]?.tileID == ids.1 && ids.0 != nil
        stage.swapTiles(second, path)
        let back = window.state.tree[path]?.camera == camera && abs(stage.zoom(atTile: path).scale - 2) < 0.01
            && abs(stage.zoom(atTile: second).scale - 3) < 0.01
        stage.resetZoom(at: path, eased: false)
        stage.resetZoom(at: second, eased: false)
        let clean = stage.zoom(atTile: path) == .none && stage.zoom(atTile: second) == .none
        print("zoom follows swap: Show Camera resets \(showReset), views moved with the cameras \(moved), tiles kept their identities \(idsStay), swapped back \(back), reset \(clean)")
        if !showReset { problems.append("Show Camera did not reset the tile's view") }
        if !moved { problems.append("a swap did not move each tile's zoom with its camera") }
        if !idsStay { problems.append("a swap moved the tiles' identities") }
        if !back || !clean { problems.append("swapping back did not bring the zooms back") }
        // The zooms and the focus asked for main streams: wait until the
        // tiles show their grid streams again before the next steps
        // compare pictures.
        guard let model = app.model else { return headerStep() }
        when({
            [camera, otherCamera].allSatisfy { c in
                model.leases.tile(for: c).map { $0.tierState.shown == model.leases.tiers(for: c).grid } ?? true
            }
        }, timeout: 5) { [self] in panAndFrameStep(path, camera) }
    }

    /// A long drag on a zoomed picture pans it 1:1 with the pointer to the
    /// release (through the mouse handlers, many small moves, the header
    /// shown); the tile's frame (its chrome: outline and header) is the
    /// tile's whole area at 1x, zoomed, and focused.
    func panAndFrameStep(_ path: NodePath, _ camera: CameraRef) {
        guard let window else { return headerStep() }
        let stage = window.stage
        func frameIsTile(_ rect: CGRect?) -> Bool {
            guard let rect, let f = stage.chromes[path]?.frame else { return false }
            return abs(f.minX - rect.minX) < 1 && abs(f.minY - rect.minY) < 1 && abs(f.width - rect.width) < 1
                && abs(f.height - rect.height) < 1
        }
        guard let leaf = stage.frames.leaf(path), let picture = stage.pictureSizes[camera] else {
            problems.append("no tile for the pan and frame steps")
            return headerStep()
        }
        // parity: tile-frame-zoom
        stage.relayoutChrome()
        let at1x = frameIsTile(leaf.rect)
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 4, eased: false)
        stage.relayoutChrome()
        let zoomed = stage.zoom(atTile: path).scale > 3.9 && frameIsTile(leaf.rect)
        print("tile frame zoom: frame is the tile at 1x \(at1x) and at 4x \(zoomed)")
        if !at1x || !zoomed { problems.append("the tile's frame is not the tile's area") }

        // parity: pan-continuous
        let from = stage.zoom(atTile: path)
        let fill = stage.fills(atTile: path)
        // The whole picture's length in points at this zoom (it overflows
        // the tile), so a change of center is a distance the picture moved.
        let base = ZoomBase(picture: picture, tile: leaf.rect.size, fill: fill)
        let length = CGSize(
            width: from.scale * base.width * leaf.rect.width, height: from.scale * base.height * leaf.rect.height)
        stage.showHeader(path)
        let start = CGPoint(x: leaf.rect.midX, y: leaf.rect.midY)
        let kind = stage.dragKind(at: start)
        let travel = CGVector(dx: 30, dy: 15)
        stage.simulateDrag(from: start, to: CGPoint(x: start.x + travel.dx, y: start.y + travel.dy), steps: 60)
        let to = stage.zoom(atTile: path)
        let moved = CGVector(
            dx: (from.center.x - to.center.x) * length.width, dy: (from.center.y - to.center.y) * length.height)
        let oneToOne = abs(moved.dx - travel.dx) < 0.5 && abs(abs(moved.dy) - travel.dy) < 0.5
        print(String(format: "pan continuous: kind %@, 60 moves of %.1f,%.1f in all moved the picture %.2f,%.2f",
            kind ?? "none", travel.dx, travel.dy, moved.dx, moved.dy))
        if kind != "pan" || !oneToOne { problems.append("a long drag did not pan the picture 1:1 with the pointer") }

        // parity: tile-frame-focus
        stage.focus(path)
        advance(StageView.focusDuration)
        stage.relayoutChrome()
        let focusFrame = frameIsTile(stage.bounds)
        stage.unfocus()
        advance(StageView.focusDuration)
        stage.relayoutChrome()
        let back = frameIsTile(stage.frames.leaf(path)?.rect)
        stage.resetZoom(at: path, eased: false)
        print("tile frame focus: frame is the whole stage \(focusFrame), back to the tile \(back)")
        if !focusFrame { problems.append("a focused tile's frame is not the whole stage") }
        if !back { problems.append("the tile's frame did not come back to its cell after focus") }
        // The zoom and the focus asked for the main stream: back on the
        // grid stream before the next steps compare pictures.
        guard let model = app.model else { return headerStep() }
        when({
            model.leases.tile(for: camera).map { $0.tierState.shown == model.leases.tiers(for: camera).grid } ?? true
        }, timeout: 5) { [self] in headerStep() }
    }

    // MARK: The tile header (docs/UX.md "Tile header")

    /// A tile's header laid out by the shared model at a narrow, a middle
    /// and a wide width: the buttons on screen are the model's, in its
    /// order, inside the tile and clear of the title; "…" holds the rest
    /// and the right-click menu is the model's list. Capabilities from the
    /// `channels` event put Start Conversation in a doorbell's header and
    /// the PTZ buttons in a moving camera's. Then the recording steps
    /// (`recordStep`).
    func headerStep() {
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
        guard runs("header") else { return recordStep(leaf.path, camera) }
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
        chrome.frame = saved
        stage.relayoutChrome()
        recordStep(leaf.path, camera)
    }

    // MARK: Recording (docs/CAPTURE.md "Live recording")

    /// How often the windows asked to be saved (they save after a delay).
    var savesCount: Int { app.windowSaveRequests }

    /// The zoomed tile's letterbox bar at 1x, in drawable pixels (the
    /// zoom-fills-tile check reads it at 2x and back at 1x).
    var zoomBar: CGRect?

    /// One of the tile's bars at 1x (drawable pixels, a pixel in from its
    /// edges), or nil if the picture fills the tile.
    static func bar(cell: CGRect, picture: CGRect) -> CGRect? {
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
    func litShare(_ rect: CGRect) -> Double? {
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

    // MARK: Camera motion (docs/UX.md "Pan, tilt, and zoom")

    var ptzChecked = false

    var discoveryChecked = false
    var retryChecked = false
    var captureRealChecked = false

    func checkLog() {
        if !ptzChecked, runs("ptz") {
            ptzChecked = true
            return ptzStep()
        }
        if !retryChecked, app.model?.fake != nil {
            retryChecked = true
            return retryStep()
        }
        if !captureRealChecked, runs("capture"), app.model.map({ $0.fake == nil }) == true {
            captureRealChecked = true
            return captureRealStep()
        }
        if !recordingsChecked, runs("recordings"), app.model?.recordings.available == true {
            recordingsChecked = true
            return app.model?.fake != nil ? recordingsStep() : recordingsRealStep()
        }
        if !smartAlertsChecked, runs("alerts"), app.model?.fake != nil {
            smartAlertsChecked = true
            return smartAlertsSettingsStep()
        }
        guard !discoveryChecked, runs("settings"), app.model?.fake != nil else { return checkLogNow() }
        discoveryChecked = true
        discoveryStep()
    }

    // MARK: Smart alerts (docs/UX.md "Smart alerts")

    var smartAlertsChecked = false
    /// The camera of the first scripted check (the event log step filters by it).
    var smartTileCamera: CameraRef?

    static let personBoxes = [
        AlertDetection(label: "person", score: 0.82, x: 0.4, y: 0.3, w: 0.12, h: 0.45),
        AlertDetection(label: "person", score: 0.31, x: 0.1, y: 0.4, w: 0.08, h: 0.3),
    ]

    /// The Retry button (docs/UX.md, "Tiles without a picture"): a device that
    /// has been reconnecting for a long time offers it under the message, a
    /// click reaches the core, and reconnecting takes it away.
    func retryStep() {
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

    var recordingsChecked = false

    /// Share of the delivered pictures the live tiles must draw.
    static let pacingShownRatio = PacingVerdict.shownRatio
    /// Pictures every live tile is delivered while live pacing measures.
    static let livePacingPictures = 30
    /// Share of the released pictures the real display must draw in the
    /// playback pacing step (a wiring check; the verdict is the simulated
    /// display's).
    static let playbackDrawnRatio = 0.8
    /// Shown pairs the playback pacing verdict needs.
    static let playbackPacingPairs = 40

    /// A live tile and a call recorded by the real core against the mock
    /// (never a real device: the step runs only for a device on this
    /// computer), each file read back with AVFoundation: the live one has
    /// the video and the camera's AAC, starts on a keyframe and lasts what
    /// the events said; the call has the video and the three audio tracks,
    /// only Conversation enabled, every one decoding, and it ends with the
    /// call.
    /// Media time (ms) the real-core recordings run before they are stopped:
    /// enough for a keyframe start, a duration check, and decoding sound in
    /// every track (the core's own tests cover caps, gaps and long files).
    static let realRecordingMs: UInt64 = 1200

    func checkLogRest() {
        if runs("settings") {
            // parity: updates-notes-scroll
            problems += UpdateNotesSelfTest.run()
            // parity: updates-bundled-defaults
            if let model = app.model { problems += UpdateDefaultsSelfTest.run(bridge: model.bridge) }
        }
        // parity: core-contract
        // At the end, so it covers every step that ran.
        if let fake = app.model?.fake {
            problems += fake.violations.map { "core contract: \($0)" }
        }
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

    func capture(_ what: String, then next: @escaping @MainActor () -> Void) {
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

    static func spread(_ image: RenderedImage) -> Double {
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

    func finish(_ problems: [String]) -> Never {
        app.model?.shutdown()
        print(String(format: "selftest took %.2f s", CACurrentMediaTime() - began))
        if problems.isEmpty {
            Swift.print("selftest: PASS")
            exit(0)
        }
        for p in problems { Swift.print("  \(p)") }
        Swift.print("selftest: FAIL")
        exit(1)
    }
}
