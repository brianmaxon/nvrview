import AppKit
import NVRCore
import VideoKit

// A doorbell call in its own window (docs/UX.md "A call in its own window").
// The call stays in the model (`DoorbellCalls`); its `presentation` says
// where it shows. `CallWindows` opens and closes the windows from it; each
// `CallWindowController` shows the doorbell's shared tile (the same decoded
// pictures as every viewer window) as a regular tile: the viewer's own
// `StageView` in its single fixed tile mode, with the call bar above and the
// call's controls below (docs/CAPTURE.md "The call window is a regular tile").

/// The app's call windows, one per call shown in a window. Not viewer
/// windows: not saved in windows.json, not counted when the last viewer
/// window closes, and closed when their call ends (closed, timed out, the
/// device removed, the app quitting).
@MainActor
final class CallWindows {
    static let frameKey = "callWindowFrame"
    static let onTopKey = "callWindowOnTop"

    private unowned let model: AppModel
    /// Where the frame, the pin and the placement choice are remembered (a
    /// throwaway suite in the self-test).
    let defaults: UserDefaults
    /// Self-test: windows stay behind the others, never take the focus and
    /// never float.
    private let keepsBehind: Bool
    /// The screen holding the active viewer window (nil: the first screen).
    var activeScreen: () -> NSScreen? = { nil }
    private var controllers: [CameraRef: CallWindowController] = [:]
    private var token: UUID?

    init(model: AppModel, defaults: UserDefaults, keepsBehind: Bool) {
        self.model = model
        self.defaults = defaults
        self.keepsBehind = keepsBehind
        token = model.subscribe { [weak self] in self?.sync() }
    }

    /// The window showing a camera's call, if it has one.
    func controller(for camera: CameraRef) -> CallWindowController? { controllers[camera.firstLens] }
    var count: Int { controllers.count }

    // MARK: Remembered settings

    /// The pin: on top of other windows (on by default).
    var pinned: Bool {
        get { defaults.object(forKey: Self.onTopKey) as? Bool ?? true }
        set { defaults.set(newValue, forKey: Self.onTopKey) }
    }

    /// "Open call windows" (Settings > Alerts, kept by the model).
    var placementMode: CallWindowPlacement.Mode {
        get { model.callWindowPlacement }
        set { model.callWindowPlacement = newValue }
    }

    /// Whether the last window opened took the focus (self-test): a ring
    /// never does; Pop Out and Start Conversation, the user's own actions, do.
    private(set) var lastOpenActivated: Bool?

    /// The frame the user last left a call window at (top-left origin).
    var rememberedFrame: WindowRect? {
        get { defaults.data(forKey: Self.frameKey).flatMap { try? JSONDecoder().decode(WindowRect.self, from: $0) } }
        set {
            if let newValue, let data = try? JSONEncoder().encode(newValue) {
                defaults.set(data, forKey: Self.frameKey)
            } else {
                defaults.removeObject(forKey: Self.frameKey)
            }
        }
    }

    // MARK: Following the calls

    /// After every model change: a call that ended closes its window; a
    /// call shown in a window without one gets it (a press opens it without
    /// taking the focus from what the user is doing; Start Conversation,
    /// which the user chose, brings it forward); the windows show the calls
    /// as they are now.
    func sync() {
        for (camera, c) in Array(controllers) where model.calls.call(for: camera) == nil {
            controllers[camera] = nil
            c.close()
        }
        for camera in model.calls.cameras where controllers[camera] == nil {
            guard let call = model.calls.call(for: camera), call.presentation == .window else { continue }
            open(camera, activate: call.origin == .conversation)
        }
        for c in controllers.values { c.refresh() }
    }

    /// Pop Out: the window opens first (its lease taken), then the call's
    /// presentation changes and the viewer windows let their insets go, so
    /// the doorbell's tile is never without a holder. An explicit Pop Out
    /// brings the window forward and gives it the focus.
    func popOut(_ camera: CameraRef) {
        let camera = camera.firstLens
        guard let call = model.calls.call(for: camera) else { return }
        if let c = controllers[camera] {
            show(c, activate: true)
        } else {
            open(camera, activate: true)
        }
        if call.presentation != .window { model.calls.setPresentation(camera, .window) }
    }

    /// Pop In: the viewer windows take their insets back (and their leases)
    /// before the window closes and lets its own go.
    func popIn(_ camera: CameraRef) {
        let camera = camera.firstLens
        let c = controllers.removeValue(forKey: camera)
        model.calls.setPresentation(camera, .inset)
        c?.close()
    }

    @discardableResult
    private func open(_ camera: CameraRef, activate: Bool) -> CallWindowController? {
        let renderer: GridRenderer
        do {
            renderer = try GridRenderer()
        } catch {
            model.logEvent("Call window: cannot set up Metal (\(error))")
            return nil
        }
        let c = CallWindowController(model: model, camera: camera, renderer: renderer, pinned: pinned, keepsBehind: keepsBehind)
        c.onCloseRequest = { [weak self] in
            // Closing the window ends the call; the window closes with it.
            self?.model.calls.end(camera, reason: "closed")
        }
        c.onPopIn = { [weak self] in self?.popIn(camera) }
        c.onPin = { [weak self] on in
            guard let self else { return }
            self.pinned = on
            for other in self.controllers.values { other.applyPinned(on) }
        }
        c.onMoved = { [weak self] frame in self?.rememberedFrame = Self.topLeft(frame) }
        c.place(frame(index: controllers.count))
        controllers[camera] = c
        lastOpenActivated = activate
        c.refresh()
        show(c, activate: activate)
        // AppKit puts the keyboard in the reply field when the window comes
        // up; it waits for a click or Tab instead (and Esc ends the call).
        c.window?.makeFirstResponder(nil)
        return c
    }

    private func show(_ c: CallWindowController, activate: Bool) {
        guard let window = c.window else { return }
        if keepsBehind {
            window.orderBack(nil)
        } else if activate {
            NSApp.activate()
            window.makeKeyAndOrderFront(nil)
        } else {
            // Shown without activating the app or taking the key window.
            window.orderFrontRegardless()
        }
    }

    // MARK: Placement

    /// The primary screen's height: macOS frames have their origin at its
    /// bottom left, the placement rule at its top left.
    private static var primaryHeight: Double { Double(NSScreen.screens.first?.frame.height ?? 0) }

    static func topLeft(_ r: NSRect) -> WindowRect {
        WindowRect(x: r.minX, y: r.minY, width: r.width, height: r.height).flipped(height: primaryHeight)
    }

    static func appKit(_ r: WindowRect) -> NSRect {
        let f = r.flipped(height: primaryHeight)
        return NSRect(x: f.x, y: f.y, width: f.width, height: f.height)
    }

    /// The frame for a new call window with `index` windows open already.
    func frame(index: Int) -> NSRect {
        let screens = NSScreen.screens.map { Self.topLeft($0.visibleFrame) }
        let active = activeScreen().map { Self.topLeft($0.visibleFrame) }
        return Self.appKit(
            CallWindowPlacement.frame(
                mode: placementMode, remembered: rememberedFrame, screens: screens, active: active, index: index))
    }
}

/// One call's window: an `NSPanel` that floats while pinned. Top to bottom:
/// the call bar (the call, its elapsed time, the pin, Pop In, close), the
/// doorbell's tile (a `StageView` in single fixed tile mode: its hover
/// header, menus, digital zoom and zoom map, fisheye views and Quality work
/// as in the grid), the captions, the Talk row, and the reply. It holds its
/// own lease on the doorbell; the tile asks for its stream like a tile on
/// Auto (the main stream when the picture is drawn, zoom included, wider
/// than the sub-stream's: `CallWindowQuality`).
@MainActor
final class CallWindowController: NSWindowController, NSWindowDelegate, StageDelegate {
    let camera: CameraRef
    private let model: AppModel
    /// The doorbell's tile.
    let stage: StageView
    /// The call bar, the Unmute cover and the call's controls around the tile.
    let inset = InsetView(host: .window)
    private let keepsBehind: Bool
    /// The call bar's elapsed time, the tile's veils and countdowns, and
    /// its time step while the window gets few display refreshes.
    private var timer: Timer?

    var onCloseRequest: () -> Void = {}
    var onPopIn: () -> Void = {}
    var onPin: (Bool) -> Void = { _ in }
    /// The user moved or resized the window (its AppKit frame).
    var onMoved: (NSRect) -> Void = { _ in }

    /// Set while the window is moved by the app (not remembered).
    private var placing = false
    private var released = false
    /// The call bar's text, made again only when the call's start, presses
    /// or elapsed seconds change (the model notifies often).
    private var header: (key: [Double], text: String)?
    private let controls: Controls

    /// Routes the call controls to the model's call.
    private final class Controls: CallControlsDelegate {
        weak var model: AppModel?
        let camera: CameraRef
        init(model: AppModel, camera: CameraRef) {
            self.model = model
            self.camera = camera
        }
        func callUnmute() { model?.calls.unmute(camera) }
        func callTalk(_ down: Bool) {
            if down { model?.calls.startTalking(camera) } else { model?.calls.stopTalking(camera) }
        }
        func callLatch(_ on: Bool) { model?.calls.setLatched(camera, on) }
        func callDraft(_ text: String) { model?.calls.setDraft(camera, text) }
        func callSend(_ text: String, accepted: @escaping (Bool) -> Void) {
            model?.calls.sendReply(text, to: camera, accepted: accepted)
        }
    }

    static let minSize = NSSize(width: 320, height: 280)

    init(model: AppModel, camera: CameraRef, renderer: GridRenderer, pinned: Bool, keepsBehind: Bool) {
        self.model = model
        self.camera = camera
        self.keepsBehind = keepsBehind
        let name = model.name(of: camera)
        stage = StageView(
            model: model,
            state: WindowState(content: .scratch(name: name, root: .leaf(camera)), name: name, tree: .leaf(camera)),
            renderer: renderer, fixedTile: true)
        controls = Controls(model: model, camera: camera)
        let panel = CallPanel(
            contentRect: NSRect(x: 0, y: 0, width: CallWindowPlacement.defaultWidth, height: CallWindowPlacement.defaultHeight),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isReleasedWhenClosed = false
        // A panel hides when the app is inactive unless told otherwise; a
        // call window stays.
        panel.hidesOnDeactivate = false
        panel.becomesKeyOnlyIfNeeded = false
        panel.autorecalculatesKeyViewLoop = true
        panel.contentMinSize = Self.minSize
        panel.tabbingMode = .disallowed
        super.init(window: panel)
        panel.delegate = self
        panel.onEscape = { [weak self] in self?.onCloseRequest() }
        panel.onCloseRequest = { [weak self] in self?.onCloseRequest() }
        panel.onInteraction = { [weak self] in self?.stage.noteInteraction() }

        let content = CallWindowContent(frame: panel.contentLayoutRect, stage: stage, inset: inset)
        content.wantsLayer = true
        content.layer?.backgroundColor = NSColor.black.cgColor
        inset.frame = content.bounds
        inset.autoresizingMask = [.width, .height]
        stage.frame = inset.videoRect
        // The tile under the call bar and controls; the inset lets clicks
        // on the picture through to it.
        content.addSubview(stage)
        content.addSubview(inset)
        panel.contentView = content
        stage.delegate = self

        inset.callDelegate = controls
        inset.onClose = { [weak self] in self?.onCloseRequest() }
        inset.onHover = { [weak self] _ in
            // Using the window is using the call.
            guard let self else { return }
            self.model.calls.use(self.camera)
        }
        inset.onPopIn = { [weak self] in self?.onPopIn() }
        inset.onPin = { [weak self] on in
            self?.applyPinned(on)
            self?.onPin(on)
        }
        applyPinned(pinned)

        // The doorbell's tile stays open while the window shows it; the
        // stage then asks for its stream (Auto).
        model.leases.acquire(camera)
        stage.reload()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, !self.released else { return }
                self.refresh()
                self.stage.tick()
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// On top of other windows (and over full-screen apps, on every Space)
    /// while pinned; an ordinary window otherwise. In the self-test it never
    /// floats (it stays behind the user's windows).
    func applyPinned(_ on: Bool) {
        inset.setPinned(on)
        guard let window, !keepsBehind else { return }
        window.level = on ? .floating : .normal
        window.collectionBehavior = on ? [.canJoinAllSpaces, .fullScreenAuxiliary] : [.fullScreenAuxiliary]
    }

    /// Moves the window without remembering the frame.
    func place(_ frame: NSRect) {
        placing = true
        window?.setFrame(frame, display: false)
        placing = false
    }

    /// Shows the call as it is now (and the tile's chrome).
    func refresh() {
        guard let call = model.calls.call(for: camera), !released else { return }
        let name = model.name(of: camera)
        let now = Date()
        let elapsed = max(0, now.timeIntervalSince(call.startedAt)).rounded(.down)
        let key = [call.startedAt.timeIntervalSince1970, Double(call.presses), elapsed]
        if header?.key != key { header = (key, Self.header(call, now: now)) }
        let header = header?.text ?? ""
        let hadCall = inset.videoRect
        inset.apply(
            InsetContent(
                header: header, cameraName: name, critical: false, isReturn: false,
                call: CallContent(
                    call: call, microphoneOn: model.calls.isTalkingWithMicrophone(camera), note: model.calls.note(for: camera)),
                recording: model.capture.callRecording(camera).map { RecordingText.callPill(ms: $0.durationMs) }))
        if inset.videoRect != hadCall { window?.contentView?.needsLayout = true }
        let title = "\(call.origin == .press ? "Doorbell" : "Conversation") · \(name)"
        if window?.title != title { window?.title = title }
        stage.refreshChrome()
    }

    /// The call bar: "Doorbell · 14:02:11 · 2 presses · 0:42".
    static func header(_ call: DoorbellCall, now: Date) -> String {
        let f = DateFormatter()
        f.timeStyle = .medium
        f.dateStyle = Calendar.current.isDate(call.startedAt, inSameDayAs: now) ? .none : .medium
        var text = "\(call.origin == .press ? "Doorbell" : "Conversation") · \(f.string(from: call.startedAt))"
        if call.presses > 1 { text += " · \(call.presses) presses" }
        return text + " · " + elapsedText(now.timeIntervalSince(call.startedAt))
    }

    /// "0:42", "12:05", "1:02:03".
    static func elapsedText(_ seconds: Double) -> String {
        let s = Int(max(0, seconds))
        let (h, m, r) = (s / 3600, s / 60 % 60, s % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, r) : String(format: "%d:%02d", m, r)
    }

    /// Whether the tile asks for the main stream now (self-test).
    var asksForMainStream: Bool { stage.fixedAsksMain }

    // MARK: The tile

    /// Quality and fisheye views chosen on the tile stay with this window
    /// (it is not a layout).
    func stage(_ stage: StageView, didEdit tree: LayoutNode) {}
    func stageDidChangeState(_ stage: StageView) {}

    /// ⇧⌘P and ⇧⌘R (the Camera menu's hidden items): Save Picture and
    /// Record Call on the call's tile.
    @objc func captureSavePicture(_ sender: Any?) { stage.performOnSelected(.savePicture) }
    @objc func captureRecord(_ sender: Any?) { stage.performOnSelected(.record) }

    // MARK: Window

    func windowDidMove(_ notification: Notification) {
        if !placing, let frame = window?.frame { onMoved(frame) }
    }

    func windowDidEndLiveResize(_ notification: Notification) {
        if !placing, let frame = window?.frame { onMoved(frame) }
    }

    func windowWillClose(_ notification: Notification) {
        guard !released else { return }
        released = true
        timer?.invalidate()
        timer = nil
        // Withdraws the tile's stream demand and stops the display link.
        stage.tearDown()
        stage.grid.removeFromSuperview()
        model.leases.release(camera)
    }
}

/// The call window's content: the tile placed in the inset's picture area.
@MainActor
final class CallWindowContent: FlippedView {
    private unowned let stage: StageView
    private unowned let inset: InsetView

    init(frame: NSRect, stage: StageView, inset: InsetView) {
        self.stage = stage
        self.inset = inset
        super.init(frame: frame)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override func layout() {
        super.layout()
        let rect = inset.convert(inset.videoRect, to: self)
        if stage.frame != rect { stage.frame = rect }
    }
}

/// The call window: does not activate the app when it opens, but becomes the
/// key window when clicked (the reply field takes typing). Esc ends the call
/// unless the reply field is being edited (then it leaves the field); the
/// close button and Cmd-W end the call too.
@MainActor
final class CallPanel: NSPanel {
    var onEscape: () -> Void = {}
    var onCloseRequest: () -> Void = {}
    /// A pointer move, click, scroll or key (the tile's chrome comes back).
    var onInteraction: () -> Void = {}

    override var canBecomeKey: Bool { true }

    /// The close button and Cmd-W: the call ends, and its window with it.
    override func performClose(_ sender: Any?) { onCloseRequest() }

    override func sendEvent(_ event: NSEvent) {
        switch event.type {
        case .mouseMoved, .leftMouseDown, .rightMouseDown, .leftMouseDragged, .scrollWheel, .magnify, .keyDown:
            onInteraction()
        default:
            break
        }
        if event.type == .keyDown {
            let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
            if event.keyCode == ViewerKeys.escape, flags.isEmpty {
                if firstResponder is NSText {
                    makeFirstResponder(nil)
                } else {
                    onEscape()
                }
                return
            }
            if flags == .command, event.charactersIgnoringModifiers == "w" {
                performClose(nil)
                return
            }
        }
        super.sendEvent(event)
    }
}

/// A plain view with a top-left origin.
@MainActor
class FlippedView: NSView {
    override var isFlipped: Bool { true }
}
