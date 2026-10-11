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

    init(call: DoorbellCall, microphoneOn: Bool, note: String?) {
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
@MainActor
final class InsetView: NSView, CallControlsDelegate {
    static let callHeight: CGFloat = 118

    var onSwap: () -> Void = {}
    var onClose: () -> Void = {}
    var onHover: (Bool) -> Void = { _ in }
    /// Holds the target strongly (it refers to the stage weakly).
    var callDelegate: CallControlsDelegate?

    private let border = CALayer()
    private let header = NSView()
    private let title = NSTextField(labelWithString: "")
    private let name = NSTextField(labelWithString: "")
    private let close = NSButton()
    private let unmute = NSButton()
    let callPanel = CallPanelView()
    private var content: InsetContent?
    private var downAt: CGPoint?
    private var tracking: NSTrackingArea?

    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.masksToBounds = true
        border.borderWidth = 3
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
        header.addSubview(title)
        header.addSubview(name)
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
        needsLayout = true
    }

    override func layout() {
        super.layout()
        let h = CGFloat(InsetGeometry.headerHeight)
        header.frame = CGRect(x: 0, y: 0, width: bounds.width, height: h)
        close.frame = CGRect(x: bounds.width - 22, y: 3, width: 16, height: 16)
        let nameWidth = min(ceil(name.intrinsicContentSize.width) + 8, bounds.width * 0.35)
        name.frame = CGRect(x: bounds.width - 26 - nameWidth, y: 4, width: nameWidth, height: 14)
        title.frame = CGRect(x: 8, y: 3, width: max(bounds.width - 40 - nameWidth, 0), height: 16)
        unmute.frame = videoRect
        callPanel.frame = CGRect(x: 0, y: bounds.height - Self.callHeight, width: bounds.width, height: Self.callHeight)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        border.frame = bounds
        CATransaction.commit()
    }

    // A click anywhere but the controls swaps the inset with the stage.
    override func mouseDown(with event: NSEvent) { downAt = convert(event.locationInWindow, from: nil) }

    override func mouseUp(with event: NSEvent) {
        let p = convert(event.locationInWindow, from: nil)
        defer { downAt = nil }
        guard let d = downAt, hypot(p.x - d.x, p.y - d.y) < 6, bounds.contains(p) else { return }
        onSwap()
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

    /// The self-test clicks the Unmute control.
    var unmuteShown: Bool { !unmute.isHidden }

    func callUnmute() { callDelegate?.callUnmute() }
    func callTalk(_ down: Bool) { callDelegate?.callTalk(down) }
    func callSend(_ text: String, accepted: @escaping (Bool) -> Void) { callDelegate?.callSend(text, accepted: accepted) }
}

/// The call's controls: Talk (held, or kept on with the toggle), the
/// talk-back state, the live transcript, and a typed reply.
@MainActor
final class CallPanelView: NSView, NSTextFieldDelegate {
    weak var delegate: CallControlsDelegate?

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
        if !c.microphoneOn, keep.state == .on, !c.talking { keep.state = .off }
    }

    override func layout() {
        super.layout()
        let w = bounds.width, pad: CGFloat = 8
        talk.frame = CGRect(x: pad, y: 6, width: 70, height: 22)
        keep.frame = CGRect(x: pad + 76, y: 8, width: 104, height: 18)
        status.frame = CGRect(x: pad + 184, y: 9, width: max(w - pad * 2 - 184, 0), height: 14)
        transcript.frame = CGRect(x: pad, y: 32, width: w - pad * 2, height: 50)
        reply.frame = CGRect(x: pad, y: 88, width: max(w - pad * 3 - 52, 0), height: 22)
        send.frame = CGRect(x: w - pad - 52, y: 88, width: 52, height: 22)
    }

    @objc private func keepChanged() { delegate?.callTalk(keep.state == .on) }

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
