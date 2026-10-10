import AppKit
import NVRCore
import QuartzCore

/// What one tile's chrome shows; rebuilt from the model, applied cheaply.
struct TileChromeContent: Equatable {
    var title: String
    var subtitle: String?
    /// The frame-rate readout, when the user turned it on.
    var rates: String?
    var badges: [Badge] = []
    var veil: TileVeil?
    var hasCamera: Bool
    var audible: Bool
    var selected: Bool
    var editing: Bool
    var dropTarget: Bool

    struct Badge: Equatable {
        var text: String
        var color: NSColor
    }
}

/// The lightweight layer above one tile of the Metal surface (docs/UX.md
/// "Chrome"): a header with the camera's name and a speaker button that
/// fades in at once on hover and out a few seconds after the pointer stops
/// or leaves; detection badges; a thin level meter along the top
/// (`LevelMeterMode`: green and always shown while the camera is audible,
/// monochrome with the header while it is muted); and words over the
/// picture while the device is reconnecting.
///
/// It never takes clicks except on its button, so the stage below handles
/// selection, dragging and focus.
@MainActor
final class TileChromeView: NSView {
    static let fadeOutDelay: TimeInterval = 2.5
    static let fadeOutAfterExit: TimeInterval = 0.8

    var onSpeaker: (_ additive: Bool) -> Void = { _ in }
    /// The header's reset-zoom control (shown while the tile is zoomed).
    var onResetZoom: () -> Void = {}

    private let header = NSView()
    private let headerGradient = CAGradientLayer()
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private let rates = NSTextField(labelWithString: "")
    private let speaker = NSButton()
    private let resetZoom = NSButton()
    private let badges = NSStackView()
    private let veil = NSView()
    private let veilTitle = NSTextField(labelWithString: "")
    private let veilDetail = NSTextField(labelWithString: "")
    /// The audible meter (green, on the tile, stays when the header fades).
    private let meter = CALayer()
    private let meterTrack = CALayer()
    /// The muted meter (monochrome, in the header, fades with it).
    private let hoverMeter = CALayer()
    private let hoverTrack = CALayer()
    private let outline = CALayer()
    private let drop = NSTextField(labelWithString: "Swap here")
    private let emptyHint = NSTextField(labelWithString: "Drag a camera here")
    /// The clock the header's fade-out delay counts on (the stage's).
    private let clock: any ViewerClock
    /// When the header fades out, on `clock` (stepped by `tick`).
    private var fadeAt: Double?
    private var tracking: NSTrackingArea?
    /// Where the pointer was (screen coordinates) at the last hover event.
    private var lastPointer: NSPoint?
    private var content: TileChromeContent?
    private var level: Float = 0
    /// The header stays shown (an active alert; docs/UX.md "Alerts").
    private var headerPinned = false
    /// The selection outline is shown (it fades with the chrome).
    private var outlineShown = true

    override var isFlipped: Bool { true }

    init(clock: any ViewerClock) {
        self.clock = clock
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = true

        header.wantsLayer = true
        headerGradient.colors = [NSColor.black.withAlphaComponent(0.6).cgColor, NSColor.clear.cgColor]
        header.layer?.addSublayer(headerGradient)
        header.alphaValue = 0
        addSubview(header)

        for label in [title, subtitle, rates, veilTitle, veilDetail, drop, emptyHint] {
            label.textColor = .white
            label.lineBreakMode = .byTruncatingTail
            label.shadow = {
                let s = NSShadow()
                s.shadowColor = NSColor.black.withAlphaComponent(0.8)
                s.shadowOffset = NSSize(width: 0, height: -1)
                s.shadowBlurRadius = 2
                return s
            }()
        }
        title.font = .systemFont(ofSize: 12, weight: .semibold)
        subtitle.font = .systemFont(ofSize: 10)
        subtitle.textColor = NSColor.white.withAlphaComponent(0.75)
        rates.font = .monospacedDigitSystemFont(ofSize: 10, weight: .regular)
        rates.textColor = NSColor.white.withAlphaComponent(0.75)
        rates.isHidden = true
        header.addSubview(title)
        header.addSubview(subtitle)
        header.addSubview(rates)

        speaker.isBordered = false
        speaker.bezelStyle = .shadowlessSquare
        speaker.imagePosition = .imageOnly
        speaker.contentTintColor = .white
        speaker.target = self
        speaker.action = #selector(speakerClicked(_:))
        speaker.toolTip = "Listen (Option-click to listen to several)"
        speaker.setAccessibilityLabel("Listen")
        header.addSubview(speaker)

        resetZoom.isBordered = false
        resetZoom.bezelStyle = .shadowlessSquare
        resetZoom.imagePosition = .imageOnly
        resetZoom.contentTintColor = .white
        resetZoom.image = NSImage(systemSymbolName: "1.magnifyingglass", accessibilityDescription: "Reset Zoom")
            ?? NSImage(systemSymbolName: "minus.magnifyingglass", accessibilityDescription: "Reset Zoom")
        resetZoom.target = self
        resetZoom.action = #selector(resetZoomClicked(_:))
        resetZoom.toolTip = "Reset Zoom (0)"
        resetZoom.setAccessibilityLabel("Reset zoom")
        resetZoom.isHidden = true
        header.addSubview(resetZoom)

        badges.orientation = .horizontal
        badges.spacing = 4
        addSubview(badges)

        veil.wantsLayer = true
        veil.layer?.backgroundColor = NSColor(calibratedRed: 0.05, green: 0.055, blue: 0.063, alpha: 0.78).cgColor
        veilTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        veilTitle.alignment = .center
        veilDetail.font = .systemFont(ofSize: 11)
        veilDetail.textColor = NSColor.white.withAlphaComponent(0.7)
        veilDetail.alignment = .center
        veil.addSubview(veilTitle)
        veil.addSubview(veilDetail)
        veil.isHidden = true
        addSubview(veil, positioned: .below, relativeTo: header)

        emptyHint.font = .systemFont(ofSize: 12)
        emptyHint.textColor = NSColor.white.withAlphaComponent(0.5)
        emptyHint.alignment = .center
        emptyHint.isHidden = true
        addSubview(emptyHint)

        drop.font = .systemFont(ofSize: 13, weight: .semibold)
        drop.alignment = .center
        drop.drawsBackground = false
        drop.wantsLayer = true
        drop.isHidden = true
        addSubview(drop)

        meterTrack.backgroundColor = NSColor.white.withAlphaComponent(0.12).cgColor
        meter.backgroundColor = NSColor.systemGreen.withAlphaComponent(0.85).cgColor
        meterTrack.isHidden = true
        meter.isHidden = true
        layer?.addSublayer(meterTrack)
        layer?.addSublayer(meter)
        hoverTrack.backgroundColor = NSColor.white.withAlphaComponent(0.12).cgColor
        hoverMeter.backgroundColor = NSColor.white.withAlphaComponent(0.6).cgColor
        hoverTrack.isHidden = true
        hoverMeter.isHidden = true
        header.layer?.addSublayer(hoverTrack)
        header.layer?.addSublayer(hoverMeter)

        outline.borderColor = NSColor.controlAccentColor.cgColor
        outline.borderWidth = 0
        layer?.addSublayer(outline)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // Clicks reach the stage, except on the header's buttons.
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard header.alphaValue > 0.05, !header.isHidden else { return nil }
        let p = convert(point, from: superview)
        for button in [speaker, resetZoom] where !button.isHidden {
            if button.convert(button.bounds, to: self).contains(p) { return button }
        }
        return nil
    }

    // MARK: Content

    func apply(_ c: TileChromeContent) {
        guard c != content else { return }
        let old = content
        content = c
        title.stringValue = c.title
        subtitle.stringValue = c.subtitle ?? ""
        subtitle.isHidden = c.subtitle == nil
        rates.stringValue = c.rates ?? ""
        if rates.isHidden != (c.rates == nil) {
            rates.isHidden = c.rates == nil
            needsLayout = true
        }
        speaker.isHidden = !c.hasCamera
        speaker.image = NSImage(
            systemSymbolName: c.audible ? "speaker.wave.2.fill" : "speaker.slash.fill",
            accessibilityDescription: c.audible ? "Listening" : "Muted")
        speaker.setAccessibilityLabel(c.audible ? "Stop listening" : "Listen")
        if old?.badges != c.badges {
            for v in badges.arrangedSubviews { v.removeFromSuperview() }
            for b in c.badges { badges.addArrangedSubview(Self.badge(b)) }
        }
        veil.isHidden = c.veil == nil || !c.hasCamera
        veilTitle.stringValue = c.veil?.title ?? ""
        veilDetail.stringValue = c.veil?.detail ?? ""
        veilDetail.isHidden = c.veil?.detail == nil
        emptyHint.isHidden = c.hasCamera
        emptyHint.stringValue = c.editing ? "Drag a camera here" : "No camera"
        drop.isHidden = !c.dropTarget
        drop.layer?.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(0.28).cgColor
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        outline.borderWidth = c.selected ? (c.editing ? 2 : 1.5) : (c.editing ? 1 : 0)
        outline.borderColor = (c.selected ? NSColor.controlAccentColor : NSColor.white.withAlphaComponent(0.12)).cgColor
        outline.opacity = outlineShown || c.editing ? 1 : 0
        let mode = LevelMeterMode(hasCamera: c.hasCamera, audible: c.audible)
        meterTrack.isHidden = mode != .audible
        meter.isHidden = mode != .audible
        hoverTrack.isHidden = mode != .muted
        hoverMeter.isHidden = mode != .muted
        if old.map({ LevelMeterMode(hasCamera: $0.hasCamera, audible: $0.audible) }) != mode { level = 0 }
        CATransaction.commit()
        if !c.hasCamera { layer?.backgroundColor = NSColor(white: 0.08, alpha: 1).cgColor } else { layer?.backgroundColor = nil }
        needsLayout = true
    }

    private var meterMode: LevelMeterMode {
        content.map { LevelMeterMode(hasCamera: $0.hasCamera, audible: $0.audible) } ?? .hidden
    }

    /// Whether the stage should read this tile's level now: only while its
    /// meter is on screen (audible, or muted with the header shown).
    var wantsLevel: Bool { meterMode.polls(headerShown: header.alphaValue > 0) }

    /// The level meter (0...1), called at display rate while `wantsLevel`.
    func setLevel(_ value: Float) {
        guard wantsLevel else { return }
        // Fast attack, slower release, so it reads as a level, not a flicker.
        level = value > level ? value : max(value, level * 0.85)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layoutMeters()
        CATransaction.commit()
    }

    private func layoutMeters() {
        let w = bounds.width * CGFloat(min(max(level, 0), 1))
        meter.frame = CGRect(x: 0, y: 0, width: w, height: 3)
        hoverMeter.frame = CGRect(x: 0, y: 0, width: w, height: 3)
    }

    private static func badge(_ b: TileChromeContent.Badge) -> NSView {
        let label = NSTextField(labelWithString: b.text)
        label.font = .systemFont(ofSize: 10, weight: .semibold)
        label.textColor = .white
        label.wantsLayer = true
        label.drawsBackground = false
        let box = NSView()
        box.wantsLayer = true
        box.layer?.backgroundColor = b.color.cgColor
        box.layer?.cornerRadius = 8
        box.addSubview(label)
        label.translatesAutoresizingMaskIntoConstraints = false
        box.translatesAutoresizingMaskIntoConstraints = false
        // The badges' stack is placed by frame, and has a zero frame until
        // the tile's first layout; the far-side insets are strong preferences
        // so that a zero-size stack never makes the set unsatisfiable (which
        // AppKit logs).
        let trailing = label.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -6)
        let bottom = label.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -1)
        trailing.priority = .defaultHigh
        bottom.priority = .defaultHigh
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 6),
            trailing,
            label.topAnchor.constraint(equalTo: box.topAnchor, constant: 1),
            bottom,
        ])
        return box
    }

    // MARK: Layout

    override func layout() {
        super.layout()
        let w = bounds.width, h = bounds.height
        let headerHeight: CGFloat = (subtitle.isHidden ? 30 : 40) + (rates.isHidden ? 0 : 14)
        header.frame = CGRect(x: 0, y: 0, width: w, height: min(headerHeight, h))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        headerGradient.frame = header.bounds
        meterTrack.frame = CGRect(x: 0, y: 0, width: w, height: 3)
        hoverTrack.frame = CGRect(x: 0, y: 0, width: w, height: 3)
        layoutMeters()
        outline.frame = bounds
        CATransaction.commit()
        let button: CGFloat = 26
        speaker.frame = CGRect(x: w - button - 6, y: 4, width: button, height: 22)
        let buttonsLeft = speaker.isHidden ? w - 4 : speaker.frame.minX
        resetZoom.frame = CGRect(x: buttonsLeft - button - 2, y: 4, width: button, height: 22)
        let badgeSize = badges.fittingSize
        let badgeRight = (resetZoom.isHidden ? buttonsLeft : resetZoom.frame.minX) - 4
        badges.frame = CGRect(x: max(badgeRight - badgeSize.width, 8), y: 7, width: badgeSize.width, height: badgeSize.height)
        let titleRight = (badges.arrangedSubviews.isEmpty ? badgeRight : badges.frame.minX) - 6
        title.frame = CGRect(x: 10, y: 7, width: max(titleRight - 10, 0), height: 16)
        subtitle.frame = CGRect(x: 10, y: 22, width: max(w - 20, 0), height: 14)
        rates.frame = CGRect(x: 10, y: subtitle.isHidden ? 22 : 36, width: max(w - 20, 0), height: 14)
        veil.frame = bounds
        veilTitle.frame = CGRect(x: 8, y: h / 2 - 18, width: max(w - 16, 0), height: 18)
        veilDetail.frame = CGRect(x: 8, y: h / 2 + 2, width: max(w - 16, 0), height: 16)
        emptyHint.frame = CGRect(x: 8, y: h / 2 - 8, width: max(w - 16, 0), height: 16)
        drop.frame = bounds
    }

    /// One tracking area for the view's life: `.inVisibleRect` follows the
    /// view's size by itself. Recreating it on every layout (the header
    /// relayouts about twice a second) made AppKit send fresh entered and
    /// moved events with the pointer standing still, so the header came
    /// back on its own after fading.
    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        guard tracking == nil else { return }
        let area = NSTrackingArea(
            rect: .zero, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect],
            owner: self)
        addTrackingArea(area)
        tracking = area
    }

    // MARK: Hover

    override func mouseEntered(with event: NSEvent) {
        lastPointer = NSEvent.mouseLocation
        showChrome()
    }

    /// Only a real movement shows the header: a moved event with the
    /// pointer where it was (cursor unhiding, window or layer updates) is
    /// ignored.
    override func mouseMoved(with event: NSEvent) {
        let now = NSEvent.mouseLocation
        if let last = lastPointer, abs(now.x - last.x) < 0.5, abs(now.y - last.y) < 0.5 { return }
        lastPointer = now
        showChrome()
    }
    override func mouseExited(with event: NSEvent) { scheduleFade(after: Self.fadeOutAfterExit) }

    /// Fades in at once; fades out a few seconds after the last movement.
    func showChrome() {
        fadeAt = nil
        if header.alphaValue < 1 {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0
                header.animator().alphaValue = 1
            }
            header.alphaValue = 1
        }
        scheduleFade(after: Self.fadeOutDelay)
    }

    private func scheduleFade(after delay: TimeInterval) {
        fadeAt = clock.now + delay
    }

    /// The stage's time step: fades the header out once its delay is up.
    func tick(now: Double) {
        guard let at = fadeAt, now >= at else { return }
        fadeOut()
    }

    func fadeOut() {
        fadeAt = nil
        guard !headerPinned else { return }
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.35
            header.animator().alphaValue = 0
        }
    }

    // MARK: Stage-driven state (zoom, alerts, auto-hide)

    /// Shows the reset-zoom control while the tile is zoomed.
    func setZoomed(_ zoomed: Bool) {
        guard resetZoom.isHidden == zoomed else { return }
        resetZoom.isHidden = !zoomed
        needsLayout = true
    }

    /// Keeps the header shown whatever the hover and auto-hide say (a tile
    /// with an active alert); releasing it lets it fade as usual.
    func setHeaderPinned(_ pinned: Bool) {
        guard pinned != headerPinned else { return }
        headerPinned = pinned
        if pinned {
            fadeAt = nil
            header.alphaValue = 1
        } else {
            scheduleFade(after: Self.fadeOutAfterExit)
        }
    }

    var isHeaderPinned: Bool { headerPinned }

    /// Whether the header is on screen now (for the self-test).
    var isHeaderShown: Bool { header.alphaValue > 0.01 }

    /// Fades the selection outline with the rest of the chrome (it stays
    /// while editing the layout).
    func setOutlineShown(_ shown: Bool) {
        guard shown != outlineShown else { return }
        outlineShown = shown
        let editing = content?.editing == true
        let animation = CABasicAnimation(keyPath: "opacity")
        animation.duration = shown ? 0.12 : 0.35
        outline.add(animation, forKey: "opacity")
        outline.opacity = shown || editing ? 1 : 0
    }

    @objc private func resetZoomClicked(_ sender: NSButton) {
        onResetZoom()
        showChrome()
    }

    @objc private func speakerClicked(_ sender: NSButton) {
        let additive = NSEvent.modifierFlags.contains(.option) || NSEvent.modifierFlags.contains(.command)
        onSpeaker(additive)
        showChrome()
    }
}
