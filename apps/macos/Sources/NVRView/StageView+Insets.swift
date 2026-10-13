import AppKit
import NVRCore

// Alert insets over the stage (docs/UX.md "Alert insets") and the doorbell
// call's controls (docs/UX.md "Doorbell calls"). The model is `InsetBoard`;
// this wires it to the window: events in, leases, the Metal grid (an inset is
// one more tile drawn on top, from the same decoded pictures), the inset
// views, the swap, and the keys.

extension StageView {
    /// The swapped-in camera's stream demand, as the leases know it.
    private struct TakeoverOwner: Hashable { let stage: ObjectIdentifier }

    /// Routes one inset's call controls to the doorbell calls.
    private final class CallTarget: CallControlsDelegate {
        weak var stage: StageView?
        let camera: CameraRef
        init(stage: StageView, camera: CameraRef) {
            self.stage = stage
            self.camera = camera
        }
        func callUnmute() { stage?.unmuteCall(camera) }
        func callTalk(_ down: Bool) { stage?.talk(camera, down) }
        func callLatch(_ on: Bool) { stage?.latch(camera, on) }
        func callDraft(_ text: String) { stage?.draft(text, camera) }
        func callSend(_ text: String, accepted: @escaping (Bool) -> Void) { stage?.sendReply(text, camera, accepted: accepted) }
    }

    func installInsets() {
        insetLayer.frame = bounds
        insetLayer.autoresizingMask = [.width, .height]
        addSubview(insetLayer)
        stageCall.frame = insetLayer.bounds
        stageCall.autoresizingMask = [.width, .height]
        stageCall.isHidden = true
        insetLayer.addSubview(stageCall)
        moreChip.font = .systemFont(ofSize: 11, weight: .semibold)
        moreChip.textColor = .white
        moreChip.drawsBackground = true
        moreChip.backgroundColor = NSColor.black.withAlphaComponent(0.7)
        moreChip.alignment = .center
        moreChip.isHidden = true
        insetLayer.addSubview(moreChip)
        stageCall.onClose = { [weak self] in
            guard let self else { return }
            if let shown = self.insetBoard.swap?.shown {
                self.dismissInset(shown)
            } else if let filling = self.stageFillingCamera {
                self.endConversation(filling)
            }
        }
        // The call window's tile has no insets of its own.
        guard !fixedTile else { return }
        detectionToken = model.subscribeDetections { [weak self] event, camera, date in
            self?.detected(event, camera: camera, at: date)
        }
        // A window opened while calls are open shows them too (detached for
        // a call in its own window, so Pop In shows it here as well).
        seenCallPresentations = currentCallPresentations
        DispatchQueue.main.async { [weak self] in
            guard let self, self.adoptOpenCalls(Set(self.model.calls.cameras)) else { return }
            self.syncInsets()
        }
    }

    func tearDownInsets() {
        if let detectionToken { model.unsubscribeDetections(detectionToken) }
        detectionToken = nil
        model.leases.clearDemands(owner: TakeoverOwner(stage: ObjectIdentifier(self)))
        takeoverCamera = nil
        model.leases.update(from: insetHeld, to: [:])
        insetHeld = [:]
    }

    // MARK: What the window shows

    /// Whether the stage shows `camera` now (the swapped-in camera, the
    /// focused tile, or any tile of the layout).
    func showsCamera(_ camera: CameraRef) -> Bool {
        if let s = insetBoard.swap { return s.shown == camera }
        if let f = state.focused { return state.tree[f]?.camera == camera }
        return state.tree.cameras.contains(camera)
    }

    /// The same for any lens of a channel (a sound alarm belongs to the
    /// channel's microphone).
    func showsChannel(_ camera: CameraRef) -> Bool {
        func same(_ c: CameraRef?) -> Bool { c?.device == camera.device && c?.channel == camera.channel }
        if let s = insetBoard.swap { return same(s.shown) }
        if let f = state.focused { return same(state.tree[f]?.camera) }
        return state.tree.cameras.contains(where: same)
    }

    /// The camera that fills the whole stage, if one does.
    var stageFillingCamera: CameraRef? {
        if let s = insetBoard.swap { return s.shown }
        if let f = state.focused { return state.tree[f]?.camera }
        return state.tree.leafCount == 1 ? state.tree.cameras.first : nil
    }

    // MARK: Events

    private func detected(_ event: DetectionEvent, camera: CameraRef, at date: Date) {
        guard let kind = InsetTrigger.kind(for: event, entrance: model.cameraOptions(camera).entrance) else { return }
        let now = clock.now
        let target = kind.isCall ? camera.firstLens : camera
        if event.active {
            let visible: Bool
            switch kind {
            // A press opens the call inset unless the doorbell fills the
            // stage (its controls then show over the stage).
            case .doorbell, .conversation: visible = stageFillingCamera?.firstLens == target
            case .soundAlarm: visible = showsChannel(target)
            default: visible = showsCamera(target)
            }
            // A call shown in its own window keeps its item here detached.
            insetBoard.started(
                kind, camera: target, visible: visible, now: now, at: date, detached: kind.isCall && isPoppedOut(target))
        } else {
            insetBoard.stopped(kind, camera: target, now: now)
        }
        syncInsets()
    }

    // MARK: Actions

    /// A click on an inset (or Tab): its camera takes the stage; the camera
    /// that was there goes into the return inset.
    func swapInset(_ camera: CameraRef) {
        let displaced =
            state.focused.flatMap { state.tree[$0]?.camera } ?? state.selected.flatMap { state.tree[$0]?.camera }
            ?? state.tree.cameras.first
        insetBoard.swap(camera, displaced: displaced, now: clock.now)
        // Clicking a call's inset is using the call.
        model.calls.use(camera)
        syncInsets()
    }

    /// Esc or a close control. Dismissing a smoke or CO inset dismisses its
    /// alert too; closing a doorbell's inset ends its call.
    func dismissInset(_ camera: CameraRef) {
        if let item = insetBoard.dismiss(camera) {
            if item.kind.isCritical { dismissAlert(item.camera) }
            if item.kind.isCall { model.calls.end(item.camera, reason: "closed") }
            if item.kind == .verification { verificationInsetDismissed(item.camera) }
        }
        syncInsets()
        refreshChrome()
    }

    /// "Start Conversation" on a camera's tile: opens the call (captions,
    /// Talk, typed reply) in the camera's inset, or over the stage when the
    /// camera fills it, as a press does, but with no press. The inset behaves
    /// like a doorbell's that ended: it closes a minute after its last use.
    func startConversation(_ camera: CameraRef) {
        let target = camera.firstLens
        let now = clock.now
        let date = Date()
        model.calls.startConversation(target, at: date)
        guard model.calls.call(for: target) != nil else { return }
        let visible = stageFillingCamera?.firstLens == target
        insetBoard.started(.conversation, camera: target, visible: visible, now: now, at: date, detached: isPoppedOut(target))
        insetBoard.stopped(.conversation, camera: target, now: now)
        syncInsets()
        refreshChrome()
    }

    /// The camera's call is shown in its own window.
    func isPoppedOut(_ camera: CameraRef) -> Bool { model.calls.call(for: camera)?.presentation == .window }

    /// Follows each call's presentation (one state for the whole app): a
    /// call popped out detaches its item here (ending a swap that shows
    /// it), one popped in comes back on top with a fresh minute, and the
    /// detached item of a call that ended goes. Returns whether the board
    /// changed.
    func followCallPresentations() -> Bool {
        var changed = false
        let now = clock.now
        if let s = insetBoard.swap, s.item.kind.isCall, isPoppedOut(s.shown) {
            changed = insetBoard.setDetached(s.shown, true, now: now) || changed
        }
        for item in insetBoard.items where item.kind.isCall && !item.isReturn {
            guard let call = model.calls.call(for: item.camera) else {
                if item.detached {
                    insetBoard.remove { $0.camera == item.camera && $0.detached }
                    changed = true
                }
                continue
            }
            let detached = call.presentation == .window
            guard item.detached != detached else { continue }
            if !detached, stageFillingCamera?.firstLens == item.camera {
                // Popped in while the doorbell fills this stage: its
                // controls show over the stage, as for a press then.
                insetBoard.remove { $0.camera == item.camera && !$0.isReturn }
                changed = true
            } else {
                changed = insetBoard.setDetached(item.camera, detached, now: now) || changed
            }
        }
        // Calls this window has no item for: one that popped in gets its
        // inset now; one seen for the first time gets one after the event
        // that opened it had its turn (a press or Start Conversation records
        // its own item, with its count, right after the call opens).
        let current = currentCallPresentations
        var poppedIn: Set<CameraRef> = []
        var fresh: Set<CameraRef> = []
        for (camera, presentation) in current {
            switch seenCallPresentations[camera] {
            case nil: fresh.insert(camera)
            case .window? where presentation == .inset: poppedIn.insert(camera)
            default: break
            }
        }
        seenCallPresentations = current
        changed = adoptOpenCalls(poppedIn) || changed
        if !fresh.isEmpty {
            DispatchQueue.main.async { [weak self] in
                guard let self, self.adoptOpenCalls(fresh) else { return }
                self.syncInsets()
            }
        }
        return changed
    }

    /// Every open call's presentation now.
    private var currentCallPresentations: [CameraRef: CallPresentation] {
        var out: [CameraRef: CallPresentation] = [:]
        for camera in model.calls.cameras { out[camera] = model.calls.call(for: camera)?.presentation }
        return out
    }

    /// Gives each of `cameras`' open calls an item on this window's board if
    /// it has none: detached for a call in its own window, an inset for one
    /// in the insets (none when the doorbell fills the stage: its controls
    /// show there). The item counts as ended, so it closes a minute after
    /// the call's last activity like a conversation's. Returns whether the
    /// board changed.
    @discardableResult
    func adoptOpenCalls(_ cameras: Set<CameraRef>) -> Bool {
        var changed = false
        let now = clock.now
        for camera in cameras {
            guard let call = model.calls.call(for: camera), insetBoard.item(for: camera) == nil else { continue }
            let detached = call.presentation == .window
            if !detached, stageFillingCamera?.firstLens == camera { continue }
            let kind: InsetKind = call.origin == .press ? .doorbell : .conversation
            changed = insetBoard.started(kind, camera: camera, visible: false, now: now, at: call.startedAt, detached: detached) || changed
            insetBoard.stopped(kind, camera: camera, now: now)
        }
        return changed
    }

    /// Ends a camera's call whichever way it is shown (its inset, or over
    /// the stage).
    func endConversation(_ camera: CameraRef) {
        if insetBoard.item(for: camera.firstLens) != nil { dismissInset(camera.firstLens) }
        model.calls.end(camera, reason: "closed")
        syncInsets()
        refreshChrome()
    }

    /// Tab and Esc while the stage has the keyboard: true when an inset used
    /// the key (otherwise it goes on as before).
    func handleInsetKey(_ key: InsetKey) -> Bool {
        guard !insetBoard.isEmpty else { return false }
        switch key {
        case .swap:
            let displaced = state.focused.flatMap { state.tree[$0]?.camera } ?? state.tree.cameras.first
            insetBoard.swapTop(displaced: displaced, now: clock.now)
            syncInsets()
        case .dismiss:
            if let shown = insetBoard.swap?.shown {
                dismissInset(shown)
            } else if let top = insetBoard.shown.first {
                if top.isReturn {
                    insetBoard.swapBack(now: nil)
                    syncInsets()
                } else {
                    dismissInset(top.camera)
                }
            }
        }
        return true
    }

    fileprivate func unmuteCall(_ camera: CameraRef) {
        model.calls.unmute(camera)
        insetBoard.use(camera, now: clock.now)
    }

    fileprivate func talk(_ camera: CameraRef, _ down: Bool) {
        if down { model.calls.startTalking(camera) } else { model.calls.stopTalking(camera) }
        insetBoard.use(camera, now: clock.now)
    }

    fileprivate func latch(_ camera: CameraRef, _ on: Bool) {
        model.calls.setLatched(camera, on)
        insetBoard.use(camera, now: clock.now)
    }

    /// Typing a reply is using the call.
    fileprivate func draft(_ text: String, _ camera: CameraRef) {
        model.calls.setDraft(camera, text)
        insetBoard.use(camera, now: clock.now)
    }

    fileprivate func sendReply(_ text: String, _ camera: CameraRef, accepted: @escaping (Bool) -> Void) {
        model.calls.sendReply(text, to: camera, accepted: accepted)
        insetBoard.use(camera, now: clock.now)
    }

    // MARK: Time

    /// From the stage's time step: insets close on the viewer clock; a
    /// call's activity counts as using its inset.
    func tickInsets(now: Double) {
        guard !insetBoard.isEmpty else { return }
        for camera in model.calls.cameras {
            if let call = model.calls.call(for: camera), insetBoard.item(for: camera) != nil {
                insetBoard.use(camera, now: call.lastActivity)
            }
        }
        guard let deadline = insetBoard.deadline, now >= deadline else { return }
        if !insetBoard.tick(now: now).isEmpty { syncInsets() }
    }

    // MARK: Keeping things in step

    /// After any change of the board: alerts dismissed elsewhere, leases,
    /// the swapped-in camera's stream, the views and their places.
    func syncInsets() {
        guard !fixedTile else { return }
        insetBoard.dropSoundAlarms { [alertActive] in !alertActive($0) }
        insetBoard.remove { [model] in model.runtime($0.camera.device) == nil }
        var wanted: [CameraRef: Int] = [:]
        for item in insetBoard.shown { wanted[item.camera, default: 0] += 1 }
        if let shown = insetBoard.swap?.shown { wanted[shown, default: 0] += 1 }
        model.leases.update(from: insetHeld, to: wanted)
        insetHeld = wanted
        let owner = TakeoverOwner(stage: ObjectIdentifier(self))
        let shown = insetBoard.swap?.shown
        if shown != takeoverCamera {
            takeoverCamera = shown
            if let shown {
                // Full screen: the main stream, sharpening in place.
                model.leases.setDemand(owner: owner, camera: shown, quality: .auto, focused: true, zoomScale: 1)
            } else {
                model.leases.clearDemands(owner: owner)
            }
        }
        let cameras = Set(insetBoard.shown.map(\.camera))
        for (camera, view) in alertInsetViews where !cameras.contains(camera) {
            view.removeFromSuperview()
            alertInsetViews[camera] = nil
        }
        for camera in cameras where alertInsetViews[camera] == nil {
            let view = InsetView()
            view.onSwap = { [weak self] in self?.swapInset(camera) }
            view.onClose = { [weak self] in
                guard let self else { return }
                if self.insetBoard.items.first(where: { $0.camera == camera })?.isReturn == true {
                    self.insetBoard.swapBack(now: self.clock.now)
                    self.syncInsets()
                } else {
                    self.dismissInset(camera)
                }
            }
            view.onHover = { [weak self] inside in
                guard let self else { return }
                self.insetBoard.hover(camera, inside, now: self.clock.now)
                // Hovering a call's inset is using the call.
                self.model.calls.use(camera)
            }
            view.callDelegate = CallTarget(stage: self, camera: camera)
            view.onPopOut = { [weak self] in self?.model.popOutCall(camera) }
            view.onDrag = { [weak self] phase, point in self?.dragInsets(phase, at: point) }
            view.onResetPosition = { [weak self] in self?.resetInsetPosition() }
            insetLayer.addSubview(view)
            alertInsetViews[camera] = view
        }
        refreshInsetContents()
        layoutInsetViews()
        relayoutChrome()
        delegate?.stageDidChangeState(self)
    }

    /// From `refreshChrome` (model changes, the window's slow timer).
    func refreshInsets() {
        guard !fixedTile else { return }
        let before = insetBoard
        insetBoard.dropSoundAlarms { [alertActive] in !alertActive($0) }
        if followCallPresentations() || insetBoard != before {
            syncInsets()
            return
        }
        let hadCall = alertInsetViews.mapValues { $0.callPanel.isHidden }
        refreshInsetContents()
        if alertInsetViews.mapValues({ $0.callPanel.isHidden }) != hadCall { layoutInsetViews() }
    }

    /// A call's controls; none for a call shown in its own window.
    private func callContent(_ camera: CameraRef) -> CallContent? {
        model.calls.call(for: camera).flatMap { $0.presentation == .window ? nil : $0 }.map {
            CallContent(
                call: $0, microphoneOn: model.calls.isTalkingWithMicrophone(camera), note: model.calls.note(for: camera))
        }
    }

    private func refreshInsetContents() {
        let now = Date()
        for item in insetBoard.shown {
            guard let view = alertInsetViews[item.camera] else { continue }
            view.apply(
                InsetContent(
                    header: item.header(now: now), cameraName: model.name(of: item.camera), critical: item.kind.isCritical,
                    isReturn: item.isReturn, call: item.kind.isCall ? callContent(item.camera) : nil))
        }
        // Over the stage: the swapped-in camera's header, and the call of a
        // doorbell that fills the stage.
        var banner = insetBoard.swap.map { "\($0.item.header(now: now)) · \(model.name(of: $0.shown)) · click the inset or Tab to swap back" }
        let call = stageFillingCamera.flatMap { callContent($0) }
        if banner == nil, let filling = stageFillingCamera, call?.isConversation == true {
            // A conversation over the stage has no inset to close: a header with a close control.
            banner = "Conversation · \(model.name(of: filling)) · close to end"
        }
        stageCall.apply(banner: banner, call: call)
        stageCall.callTarget = stageFillingCamera.map { CallTarget(stage: self, camera: $0.firstLens) }
        stageCall.onPopOut = stageFillingCamera.map { camera in { [weak self] in self?.model.popOutCall(camera.firstLens) } }
    }

    // MARK: Moving the column (docs/UX.md "Alert insets", "Moving them")

    /// Where the column sits now: being dragged, where the user left it, or
    /// the default corner (the Settings corner, else opposite the
    /// indicators). The 12 pt margin keeps it off the sidebar's 6 pt
    /// edge-reveal strip and the full-screen toolbar's reveal edge at the top.
    var effectiveInsetAnchor: InsetAnchor { insetDragAnchor ?? insetAnchor ?? InsetAnchor(corner: model.insetCorner) }

    /// A drag on an inset moves the whole column; each axis snaps to its
    /// edge within 24 pt. The column keeps its position as a fraction of the
    /// free space (InsetPlacement), so resizing keeps it in place.
    func dragInsets(_ phase: InsetView.DragPhase, at point: CGPoint) {
        switch phase {
        case .began:
            guard let column = insetColumn else { return }
            insetDragStart = (point, column)
            insetsUsedByDrag()
        case .moved, .ended:
            guard let start = insetDragStart else { return }
            let b = insetLayer.bounds
            let origin = (
                x: Double(start.column.minX + point.x - start.pointer.x),
                y: Double(start.column.minY + point.y - start.pointer.y))
            let anchor = InsetPlacement.anchor(
                origin: origin, column: (Double(start.column.width), Double(start.column.height)),
                stage: (Double(b.width), Double(b.height)))
            if phase == .moved {
                insetDragAnchor = anchor
                layoutInsetViews()
                return
            }
            insetDragStart = nil
            insetDragAnchor = nil
            insetsUsedByDrag()
            if insetAnchor == anchor {
                layoutInsetViews()
            } else {
                insetAnchor = anchor
            }
            onInsetAnchorChange()
        }
    }

    /// Moving the column is using its insets (their minute starts again).
    private func insetsUsedByDrag() {
        let now = clock.now
        for item in insetBoard.shown {
            insetBoard.use(item.camera, now: now)
            model.calls.use(item.camera)
        }
    }

    /// "Reset Position": back to the default corner.
    func resetInsetPosition() {
        insetDragStart = nil
        insetDragAnchor = nil
        insetAnchor = nil
        onInsetAnchorChange()
    }

    /// Places the insets in their column (where the user moved it, else the
    /// default corner), newest nearest its nearer horizontal edge, and notes
    /// where each picture goes for the renderer.
    func layoutInsetViews() {
        let b = insetLayer.bounds
        guard b.width > 0, b.height > 0 else { return }
        let width = InsetGeometry.width(stage: Double(b.width))
        let shown = insetBoard.shown
        let heights = shown.map { item -> Double in
            let aspect = pictureSizes[item.camera].map { $0.width > 0 ? Double($0.height / $0.width) : 0.5625 } ?? 0.5625
            let call = alertInsetViews[item.camera]?.callPanel.isHidden == false ? Double(InsetView.callHeight) : 0
            return InsetGeometry.headerHeight + width * min(max(aspect, 0.3), 1.2) + call
        }
        let anchor = effectiveInsetAnchor
        let frames = InsetGeometry.frames(
            count: shown.count, width: width, heights: heights, anchor: anchor,
            stage: (Double(b.width), Double(b.height)))
        var rects: [CameraRef: CGRect] = [:]
        for (i, item) in shown.enumerated() {
            guard let view = alertInsetViews[item.camera] else { continue }
            guard i < frames.count else {
                view.isHidden = true
                continue
            }
            let f = frames[i]
            let frame = CGRect(x: f.x, y: f.y, width: f.width, height: f.height)
            if view.frame != frame { view.frame = frame }
            view.isHidden = false
            view.layoutSubtreeIfNeeded()
            rects[item.camera] = view.videoRect.offsetBy(dx: frame.minX, dy: frame.minY)
        }
        insetVideoRects = rects
        let column = frames.map { CGRect(x: $0.x, y: $0.y, width: $0.width, height: $0.height) }.reduce(CGRect.null) { $0.union($1) }
        insetColumn = column.isNull ? nil : column
        stageCall.avoid = insetColumn
        let more = insetBoard.more + max(shown.count - frames.count, 0)
        moreChip.isHidden = more == 0
        if more > 0, let last = frames.last {
            moreChip.stringValue = "+\(more) more"
            let y = anchor.growsUpward ? last.y - 6 - 20 : last.y + last.height + 6
            moreChip.frame = CGRect(x: last.x, y: y, width: 90, height: 20)
        }
        // A live check's boxes and steps ride on its inset (this also keeps
        // the insets, each with its overlay, above the stage's call controls
        // in a stable order).
        layoutVerificationInsets()
        restackInsetLayer()
    }

    // MARK: Self-test

    /// The inset views drawn now, top of the stack first.
    var shownInsetViews: [InsetView] { insetBoard.shown.compactMap { alertInsetViews[$0.camera] }.filter { !$0.isHidden } }
}

/// A transparent view whose own area lets clicks through to what is below;
/// only its subviews take them.
@MainActor
class PassThroughView: NSView {
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit === self ? nil : hit
    }
}

/// Over the whole stage: the swapped-in camera's header (with its close
/// control), and the call of a doorbell that fills the stage (its Unmute
/// control over the picture, its controls along the bottom).
@MainActor
final class StageCallView: PassThroughView, CallControlsDelegate {
    var onClose: () -> Void = {}
    /// Pop Out for the call over the stage.
    var onPopOut: (() -> Void)?
    /// Holds the target strongly (it refers to the stage weakly).
    var callTarget: CallControlsDelegate?

    private let banner = NSView()
    private let popOut = NSButton()
    private let bannerText = NSTextField(labelWithString: "")
    private let close = NSButton()
    private let unmute = NSButton()
    private let panel = CallPanelView()

    override init(frame: NSRect) {
        super.init(frame: frame)
        banner.wantsLayer = true
        banner.layer?.backgroundColor = NSColor(calibratedRed: 0.1, green: 0.1, blue: 0.12, alpha: 0.85).cgColor
        banner.layer?.cornerRadius = 6
        bannerText.font = .systemFont(ofSize: 12, weight: .semibold)
        bannerText.textColor = .white
        bannerText.lineBreakMode = .byTruncatingTail
        close.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Close")
        close.isBordered = false
        close.contentTintColor = .white
        close.target = self
        close.action = #selector(closeClicked)
        close.toolTip = "Close (Esc)"
        close.refusesFirstResponder = true
        banner.addSubview(bannerText)
        banner.addSubview(close)
        unmute.title = "Unmute"
        unmute.image = NSImage(systemSymbolName: "speaker.wave.2.fill", accessibilityDescription: "Unmute")
        unmute.imagePosition = .imageLeading
        unmute.font = .systemFont(ofSize: 28, weight: .semibold)
        unmute.isBordered = false
        unmute.wantsLayer = true
        unmute.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.45).cgColor
        unmute.contentTintColor = .white
        unmute.target = self
        unmute.action = #selector(unmuteClicked)
        unmute.refusesFirstResponder = true
        unmute.setAccessibilityLabel("Unmute the doorbell")
        addSubview(unmute)
        addSubview(banner)
        panel.delegate = self
        panel.wantsLayer = true
        panel.layer?.cornerRadius = 8
        addSubview(panel)
        popOut.image = NSImage(systemSymbolName: "arrow.up.forward.app", accessibilityDescription: "Pop Out Call")
        popOut.isBordered = false
        popOut.imagePosition = .imageOnly
        popOut.contentTintColor = .white
        popOut.wantsLayer = true
        popOut.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.6).cgColor
        popOut.layer?.cornerRadius = 4
        popOut.target = self
        popOut.action = #selector(popOutClicked)
        popOut.toolTip = "Pop Out Call: open it in its own window"
        popOut.refusesFirstResponder = true
        addSubview(popOut)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// The insets' column, which the call controls keep clear of.
    var avoid: CGRect? {
        didSet { if avoid != oldValue { needsLayout = true } }
    }

    func apply(banner text: String?, call: CallContent?) {
        banner.isHidden = text == nil
        bannerText.stringValue = text ?? ""
        unmute.isHidden = call?.showsUnmute != true
        panel.isHidden = call == nil
        popOut.isHidden = call == nil
        if let call { panel.apply(call) }
        isHidden = text == nil && call == nil
        needsLayout = true
    }

    /// The self-test reads whether the stage's Unmute control shows.
    var unmuteShown: Bool { !isHidden && !unmute.isHidden }

    override func layout() {
        super.layout()
        let b = bounds
        banner.frame = CGRect(x: 12, y: 12, width: min(b.width - 24, 620), height: 28)
        close.frame = CGRect(x: banner.frame.width - 24, y: 6, width: 16, height: 16)
        bannerText.frame = CGRect(x: 10, y: 6, width: max(banner.frame.width - 40, 0), height: 16)
        let panelWidth = min(b.width - 24, 480)
        var x = (b.width - panelWidth) / 2
        let y = b.height - InsetView.callHeight - 12
        if let avoid, avoid.intersects(CGRect(x: x, y: y, width: panelWidth, height: InsetView.callHeight)) {
            // Beside the insets' column, on the side with room.
            x = avoid.midX < b.midX ? max(avoid.maxX + 12, x) : min(avoid.minX - 12 - panelWidth, x)
            x = min(max(x, 12), max(b.width - 12 - panelWidth, 12))
        }
        panel.frame = CGRect(x: x, y: y, width: panelWidth, height: InsetView.callHeight)
        popOut.frame = CGRect(x: panel.frame.maxX - 26, y: y - 26, width: 22, height: 22)
        unmute.frame = CGRect(x: 0, y: 0, width: b.width, height: max(panel.frame.minY - 8, 0))
    }

    @objc private func closeClicked() { onClose() }
    @objc private func unmuteClicked() { callTarget?.callUnmute() }
    @objc private func popOutClicked() { onPopOut?() }

    func callUnmute() { callTarget?.callUnmute() }
    func callTalk(_ down: Bool) { callTarget?.callTalk(down) }
    func callLatch(_ on: Bool) { callTarget?.callLatch(on) }
    func callDraft(_ text: String) { callTarget?.callDraft(text) }
    func callSend(_ text: String, accepted: @escaping (Bool) -> Void) { callTarget?.callSend(text, accepted: accepted) }
}
