import AppKit
import NVRCore
import QuartzCore

/// What an alert inset shows; rebuilt from the model, applied cheaply.
struct InsetContent: Equatable {
    /// "Doorbell · 14:02:11 · 2 times, last 14:03:40".
    var header: String
    var cameraName: String
    var critical: Bool
    /// The return inset after a swap ("Back to …").
    var isReturn: Bool
    var call: CallContent?
}

/// The doorbell call's part of an inset (or of the stage, when the doorbell
/// fills it).
struct CallContent: Equatable {
    var showsUnmute: Bool
    /// Opened by Start Conversation rather than a press.
    var isConversation: Bool
    var talkText: String
    var talking: Bool
    /// Talk is held or toggled on (the microphone runs).
    var microphoneOn: Bool
    var transcript: [String]
    /// "Preparing speech recognition", or why there is no transcript.
    var transcriptNote: String?
    var note: String?
    /// "Keep talking" is on (kept with the call, not the view).
    var latched: Bool
    /// The reply typed and not sent yet (kept with the call).
    var draft: String

    init(call: DoorbellCall, microphoneOn: Bool, note: String?) {
        latched = call.latched
        draft = call.draft
        showsUnmute = call.showsUnmute
        isConversation = call.origin == .conversation
        talkText = call.talk.text
        talking = call.talk.isTalking
        self.microphoneOn = microphoneOn
        transcript = call.transcript.lines.suffix(4).map { "\($0.source.label): \($0.text)\($0.isFinal ? "" : "…")" }
        switch call.transcription {
        case .preparing: transcriptNote = "Preparing speech recognition…"
        case .unavailable(let why): transcriptNote = "No transcript: \(why)"
        case .running, .finished: transcriptNote = call.transcript.isEmpty ? "Listening for speech…" : nil
        }
        self.note = note
    }
}

/// What the call's controls ask for.
@MainActor
protocol CallControlsDelegate: AnyObject {
    func callUnmute()
    func callTalk(_ down: Bool)
    /// "Keep talking" turned on or off.
    func callLatch(_ on: Bool)
    /// The reply field's text changed.
    func callDraft(_ text: String)
    /// `accepted(false)` when nothing was sent: the text stays in the field.
    func callSend(_ text: String, accepted: @escaping (Bool) -> Void)
}

/// One alert inset above the stage (docs/UX.md "Alert insets"): a border in
/// the severity's color, a header (reason, time, count; the close control),
/// a transparent hole where the stage's Metal grid draws the camera's live
/// sub-stream, and for a doorbell call its controls below the picture.
///
/// It takes clicks (a click swaps it with the stage) but never the keyboard
/// focus, except its reply field when the user clicks into it.
///
/// The same view hosts a call in its own window (`Host.window`, docs/UX.md
/// "A call in its own window"): it fills the window around the call's tile
/// (a `StageView` below it, which takes the clicks on the picture), its
/// header is the call bar with the pin (on top), Pop In and close, and the
/// captions come before the Talk row.
@MainActor
final class InsetView: NSView, CallControlsDelegate {
    static let callHeight: CGFloat = 118

    /// Where the view lives.
    enum Host {
        /// Over a viewer window's stage.
        case stage
        /// Filling a call window.
        case window
    }

    let host: Host
    var onSwap: () -> Void = {}
    var onClose: () -> Void = {}
    var onHover: (Bool) -> Void = { _ in }
    /// Pop Out (stage) for a call's inset; nil hides the button and item.
    var onPopOut: (() -> Void)?
    /// Pop In (window).
    var onPopIn: () -> Void = {}
    /// The pin turned on or off (window).
    var onPin: (Bool) -> Void = { _ in }
    /// A drag of the inset (stage): the whole column moves. The point is in
    /// the superview's coordinates; `.began` carries where the button went
    /// down.
    var onDrag: ((DragPhase, CGPoint) -> Void)?
    /// "Reset Position" in the inset's menu (stage).
    var onResetPosition: (() -> Void)?

    enum DragPhase {
        case began, moved, ended
    }
    /// Holds the target strongly (it refers to the stage weakly).
    var callDelegate: CallControlsDelegate?

    private let border = CALayer()
    private let header = NSView()
    private let title = NSTextField(labelWithString: "")
    private let name = NSTextField(labelWithString: "")
    private let close = NSButton()
    /// Pop Out (stage) or Pop In (window).
    private let pop = NSButton()
    /// Keep the call window on top (window).
    private let pin = NSButton()
    private let unmute = NSButton()
    let callPanel = CallPanelView()
    private var content: InsetContent?
    private var downAt: CGPoint?
    /// Where the button went down, in the superview's coordinates.
    private var downInSuperview: CGPoint?
    private var downOnHeader = false
    private var dragging = false
    private var tracking: NSTrackingArea?

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    init(host: Host = .stage) {
        self.host = host
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = host == .stage ? 8 : 0
        layer?.masksToBounds = true
        border.borderWidth = host == .stage ? 3 : 0
        border.cornerRadius = 8
        header.wantsLayer = true
        header.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.72).cgColor
        title.font = .systemFont(ofSize: 11, weight: .semibold)
        title.textColor = .white
        title.lineBreakMode = .byTruncatingTail
        name.font = .systemFont(ofSize: 10)
        name.textColor = NSColor.white.withAlphaComponent(0.7)
        name.lineBreakMode = .byTruncatingTail
        name.alignment = .right
        close.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Close")
        close.isBordered = false
        close.imagePosition = .imageOnly
        close.contentTintColor = NSColor.white.withAlphaComponent(0.85)
        close.target = self
        close.action = #selector(closeClicked)
        close.toolTip = "Close (Esc)"
        close.refusesFirstResponder = true
        let popSymbol = host == .stage ? "arrow.up.forward.app" : "arrow.down.backward.square"
        let popTitle = host == .stage ? "Pop Out Call" : "Pop In Call"
        pop.image = NSImage(systemSymbolName: popSymbol, accessibilityDescription: popTitle)
        pop.isBordered = false
        pop.imagePosition = .imageOnly
        pop.contentTintColor = NSColor.white.withAlphaComponent(0.85)
        pop.target = self
        pop.action = #selector(popClicked)
        pop.toolTip = host == .stage ? "Pop Out Call: open it in its own window" : "Pop In Call: back to the viewer windows"
        pop.refusesFirstResponder = true
        pop.isHidden = host == .stage
        pin.isBordered = false
        pin.imagePosition = .imageOnly
        pin.setButtonType(.toggle)
        pin.contentTintColor = NSColor.white.withAlphaComponent(0.85)
        pin.target = self
        pin.action = #selector(pinClicked)
        pin.refusesFirstResponder = true
        pin.isHidden = host == .stage
        setPinned(true)
        header.addSubview(title)
        header.addSubview(name)
        header.addSubview(pin)
        header.addSubview(pop)
        header.addSubview(close)
        addSubview(header)

        unmute.title = "Unmute"
        unmute.image = NSImage(systemSymbolName: "speaker.wave.2.fill", accessibilityDescription: "Unmute")
        unmute.imagePosition = .imageLeading
        unmute.font = .systemFont(ofSize: 18, weight: .semibold)
        unmute.isBordered = false
        unmute.wantsLayer = true
        unmute.layer?.backgroundColor = NSColor.black.withAlphaComponent(0.55).cgColor
        unmute.contentTintColor = .white
        unmute.target = self
        unmute.action = #selector(unmuteClicked)
        unmute.refusesFirstResponder = true
        unmute.setAccessibilityLabel("Unmute the doorbell")
        unmute.isHidden = true
        addSubview(unmute)

        callPanel.delegate = self
        callPanel.captionsFirst = host == .window
        callPanel.isHidden = true
        addSubview(callPanel)
        layer?.addSublayer(border)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// Where the picture goes, in this view's coordinates.
    var videoRect: CGRect {
        let top = CGFloat(InsetGeometry.headerHeight)
        let bottom = content?.call == nil ? 0 : Self.callHeight
        return CGRect(x: 0, y: top, width: bounds.width, height: max(bounds.height - top - bottom, 0))
    }

    func apply(_ c: InsetContent) {
        guard c != content else { return }
        content = c
        title.stringValue = c.isReturn ? "Back to the view (click or Tab)" : c.header
        name.stringValue = c.cameraName
        close.toolTip = c.isReturn ? "Swap back" : "Close (Esc)"
        let color: NSColor = c.isReturn ? .secondaryLabelColor : c.critical ? .systemRed : .systemTeal
        border.borderColor = color.cgColor
        setAccessibilityLabel("\(c.isReturn ? "Back to the view" : c.header), \(c.cameraName)")
        if let call = c.call {
            callPanel.isHidden = false
            callPanel.apply(call)
            unmute.isHidden = !call.showsUnmute
        } else {
            callPanel.isHidden = true
            unmute.isHidden = true
        }
        if host == .stage { pop.isHidden = c.call == nil || onPopOut == nil }
        needsLayout = true
    }

    /// The pin's state (window): on top of other windows.
    func setPinned(_ on: Bool) {
        pin.state = on ? .on : .off
        pin.image = NSImage(
            systemSymbolName: on ? "pin.fill" : "pin.slash", accessibilityDescription: on ? "Keep on top: on" : "Keep on top: off")
        pin.toolTip = on ? "On top of other windows (click to let it go behind)" : "Not on top (click to keep it on top)"
    }

    var isPinned: Bool { pin.state == .on }

    override func layout() {
        super.layout()
        let h = CGFloat(InsetGeometry.headerHeight)
        header.frame = CGRect(x: 0, y: 0, width: bounds.width, height: h)
        // The header's buttons from the right: close (stage) or the pin
        // (window), then Pop Out or Pop In.
        var right = bounds.width - 6
        for button in [close, pin, pop] where !button.isHidden {
            button.frame = CGRect(x: right - 16, y: 3, width: 16, height: 16)
            right -= 22
        }
        let nameWidth = min(ceil(name.intrinsicContentSize.width) + 8, bounds.width * 0.35)
        name.frame = CGRect(x: right - nameWidth, y: 4, width: nameWidth, height: 14)
        title.frame = CGRect(x: 8, y: 3, width: max(right - nameWidth - 12, 0), height: 16)
        unmute.frame = videoRect
        callPanel.frame = CGRect(x: 0, y: bounds.height - Self.callHeight, width: bounds.width, height: Self.callHeight)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        border.frame = bounds
        CATransaction.commit()
    }

    /// The header's labels take no clicks of their own: a press on them is
    /// a press on the inset (a click swaps, a drag moves the column). In a
    /// call window the picture area is the tile's, below this view.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        if host == .window, hit === self { return nil }
        return hit === header || hit === title || hit === name ? self : hit
    }

    /// Moving this far from where the button went down makes a press on the
    /// picture a drag instead of a click; on the header a drag starts sooner.
    static let clickSlop: CGFloat = 6
    static let headerSlop: CGFloat = 2

    // A click anywhere but the controls swaps the inset with the stage; a
    // drag on the header, or on the picture past the click's slop, moves
    // the column.
    override func mouseDown(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        downAt = p
        downInSuperview = superview?.convert(event.locationInWindow, from: nil)
        downOnHeader = p.y < CGFloat(InsetGeometry.headerHeight)
        dragging = false
    }

    override func mouseDragged(with event: NSEvent) {
        guard host == .stage, let onDrag, let d = downAt, let start = downInSuperview,
            let p = superview?.convert(event.locationInWindow, from: nil)
        else { return }
        if !dragging {
            let here = convert(event.locationInWindow, from: nil)
            guard hypot(here.x - d.x, here.y - d.y) >= (downOnHeader ? Self.headerSlop : Self.clickSlop) else { return }
            dragging = true
            onDrag(.began, start)
        }
        onDrag(.moved, p)
    }

    override func mouseUp(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        defer {
            downAt = nil
            downInSuperview = nil
            dragging = false
        }
        if dragging {
            if let q = superview?.convert(event.locationInWindow, from: nil) { onDrag?(.ended, q) }
            return
        }
        guard host == .stage, let d = downAt, hypot(p.x - d.x, p.y - d.y) < Self.clickSlop, bounds.contains(p) else { return }
        onSwap()
    }

    /// The inset's menu: Pop Out Call for a call's inset and Reset Position
    /// over the stage, Pop In Call in a call window.
    override func menu(for event: NSEvent) -> NSMenu? {
        let menu = NSMenu()
        switch host {
        case .stage:
            if content?.call != nil, let popOut = onPopOut {
                menu.addItem(ClosureMenuItem(title: "Pop Out Call", action: popOut))
            }
            if let reset = onResetPosition {
                menu.addItem(ClosureMenuItem(title: "Reset Position", action: reset))
            }
            guard !menu.items.isEmpty else { return nil }
        case .window:
            menu.addItem(ClosureMenuItem(title: "Pop In Call") { [weak self] in self?.onPopIn() })
        }
        return menu
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        let t = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self)
        addTrackingArea(t)
        tracking = t
    }

    override func mouseEntered(with event: NSEvent) { onHover(true) }
    override func mouseExited(with event: NSEvent) { onHover(false) }

    @objc private func closeClicked() { onClose() }
    @objc private func unmuteClicked() { callDelegate?.callUnmute() }

    @objc private func popClicked() {
        switch host {
        case .stage: onPopOut?()
        case .window: onPopIn()
        }
    }

    @objc private func pinClicked() {
        setPinned(pin.state == .on)
        onPin(pin.state == .on)
    }

    /// The self-test clicks the Unmute control.
    var unmuteShown: Bool { !unmute.isHidden }
    /// The self-test reads whether the header offers Pop Out or Pop In.
    var popShown: Bool { !pop.isHidden }

    func callUnmute() { callDelegate?.callUnmute() }
    func callTalk(_ down: Bool) { callDelegate?.callTalk(down) }
    func callLatch(_ on: Bool) { callDelegate?.callLatch(on) }
    func callDraft(_ text: String) { callDelegate?.callDraft(text) }
    func callSend(_ text: String, accepted: @escaping (Bool) -> Void) { callDelegate?.callSend(text, accepted: accepted) }
}

/// The call's controls: Talk (held, or kept on with the toggle), the
/// talk-back state, the live transcript, and a typed reply.
@MainActor
final class CallPanelView: NSView, NSTextFieldDelegate {
    weak var delegate: CallControlsDelegate?
    /// The captions above the Talk row (a call window: docs/CAPTURE.md);
    /// an inset keeps Talk first.
    var captionsFirst = false {
        didSet { needsLayout = true }
    }

    private let talk = PushToTalkButton()
    private let keep = NSButton(checkboxWithTitle: "Keep talking", target: nil, action: nil)
    private let status = NSTextField(labelWithString: "")
    private let transcript = NSTextField(wrappingLabelWithString: "")
    private let reply = NSTextField()
    private let send = NSButton(title: "Send", target: nil, action: nil)
    private var content: CallContent?

    override var isFlipped: Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor(calibratedWhite: 0.08, alpha: 0.92).cgColor
        talk.title = "Talk"
        talk.image = NSImage(systemSymbolName: "mic.fill", accessibilityDescription: "Talk")
        talk.imagePosition = .imageLeading
        talk.bezelStyle = .rounded
        talk.controlSize = .small
        talk.toolTip = "Hold to talk through the doorbell"
        talk.onPress = { [weak self] down in
            guard let self, self.keep.state == .off else { return }
            self.delegate?.callTalk(down)
        }
        keep.controlSize = .small
        keep.target = self
        keep.action = #selector(keepChanged)
        keep.toolTip = "Talk until this is turned off"
        status.font = .systemFont(ofSize: 10)
        status.textColor = NSColor.white.withAlphaComponent(0.75)
        status.lineBreakMode = .byTruncatingTail
        transcript.font = .systemFont(ofSize: 11)
        transcript.textColor = .white
        transcript.maximumNumberOfLines = 3
        transcript.lineBreakMode = .byTruncatingHead
        reply.placeholderString = "Type a reply to speak"
        reply.controlSize = .small
        reply.font = .systemFont(ofSize: 11)
        reply.delegate = self
        reply.target = self
        reply.action = #selector(sendClicked)
        send.bezelStyle = .rounded
        send.controlSize = .small
        send.target = self
        send.action = #selector(sendClicked)
        send.toolTip = "Speak the typed text through the doorbell"
        for v in [talk, keep, status, transcript, reply, send] as [NSView] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func apply(_ c: CallContent) {
        guard c != content else { return }
        content = c
        var line = c.talkText
        if let note = c.note { line = note }
        status.stringValue = line
        let lines = c.transcript
        transcript.stringValue = lines.isEmpty ? (c.transcriptNote ?? "") : lines.joined(separator: "\n")
        transcript.textColor = lines.isEmpty ? NSColor.white.withAlphaComponent(0.55) : .white
        talk.state = c.microphoneOn ? .on : .off
        // The latch and the unsent reply belong to the call, so a view built
        // again shows them; the field is left alone while it is edited.
        keep.state = c.latched ? .on : .off
        if reply.currentEditor() == nil, reply.stringValue != c.draft { reply.stringValue = c.draft }
    }

    func controlTextDidChange(_ obj: Notification) { delegate?.callDraft(reply.stringValue) }

    /// "Keep talking" shows on (self-test).
    var keepTalkingShown: Bool { keep.state == .on }
    /// What the reply field shows (self-test).
    var replyText: String { reply.stringValue }
    /// The reply field is being edited (Esc leaves it rather than ending
    /// the call).
    var isEditingReply: Bool { reply.currentEditor() != nil }

    override func layout() {
        super.layout()
        let w = bounds.width, pad: CGFloat = 8
        let talkY: CGFloat = captionsFirst ? 60 : 6
        talk.frame = CGRect(x: pad, y: talkY, width: 70, height: 22)
        keep.frame = CGRect(x: pad + 76, y: talkY + 2, width: 104, height: 18)
        status.frame = CGRect(x: pad + 184, y: talkY + 3, width: max(w - pad * 2 - 184, 0), height: 14)
        transcript.frame = CGRect(x: pad, y: captionsFirst ? 6 : 32, width: w - pad * 2, height: 50)
        reply.frame = CGRect(x: pad, y: 88, width: max(w - pad * 3 - 52, 0), height: 22)
        send.frame = CGRect(x: w - pad - 52, y: 88, width: 52, height: 22)
    }

    @objc private func keepChanged() { delegate?.callLatch(keep.state == .on) }

    @objc private func sendClicked() {
        let text = reply.stringValue
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        // The field is cleared once the reply is on its way; if the user
        // declines the talk-back prompt it keeps what was typed.
        delegate?.callSend(text) { [weak self] sent in
            if sent, self?.reply.stringValue == text { self?.reply.stringValue = "" }
        }
    }
}

/// A button that reports press and release (push to talk).
@MainActor
final class PushToTalkButton: NSButton {
    var onPress: (Bool) -> Void = { _ in }

    override func mouseDown(with event: NSEvent) {
        highlight(true)
        onPress(true)
        // Tracks until the button is released, wherever the pointer goes.
        while let next = window?.nextEvent(matching: [.leftMouseUp, .leftMouseDragged]), next.type != .leftMouseUp {}
        highlight(false)
        onPress(false)
    }
}
