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
/// or leaves; detection badges; a thin level meter along the top while the
/// camera is audible (it stays when the header fades); and words over the
/// picture while the device is reconnecting.
///
/// It never takes clicks except on its button, so the stage below handles
/// selection, dragging and focus.
@MainActor
final class TileChromeView: NSView {
    static let fadeOutDelay: TimeInterval = 2.5
    static let fadeOutAfterExit: TimeInterval = 0.8

    var onSpeaker: (_ additive: Bool) -> Void = { _ in }

    private let header = NSView()
    private let headerGradient = CAGradientLayer()
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private let rates = NSTextField(labelWithString: "")
    private let speaker = NSButton()
    private let badges = NSStackView()
    private let veil = NSView()
    private let veilTitle = NSTextField(labelWithString: "")
    private let veilDetail = NSTextField(labelWithString: "")
    private let meter = CALayer()
    private let meterTrack = CALayer()
    private let outline = CALayer()
    private let drop = NSTextField(labelWithString: "Swap here")
    private let emptyHint = NSTextField(labelWithString: "Drag a camera here")
    private var fadeTimer: Timer?
    private var tracking: NSTrackingArea?
    private var content: TileChromeContent?
    private var level: Float = 0

    override var isFlipped: Bool { true }

    init() {
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
        meter.backgroundColor = NSColor.white.withAlphaComponent(0.7).cgColor
        meterTrack.isHidden = true
        meter.isHidden = true
        layer?.addSublayer(meterTrack)
        layer?.addSublayer(meter)

        outline.borderColor = NSColor.controlAccentColor.cgColor
        outline.borderWidth = 0
        layer?.addSublayer(outline)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // Clicks reach the stage, except on the speaker button.
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard header.alphaValue > 0.05, !header.isHidden else { return nil }
        let p = convert(point, from: superview)
        let inSpeaker = speaker.convert(speaker.bounds, to: self).contains(p)
        return inSpeaker ? speaker : nil
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
        outline.borderWidth = c.selected ? 2 : (c.editing ? 1 : 0)
        outline.borderColor = (c.selected ? NSColor.controlAccentColor : NSColor.white.withAlphaComponent(0.12)).cgColor
        meterTrack.isHidden = !c.audible
        meter.isHidden = !c.audible
        if !c.audible { level = 0 }
        CATransaction.commit()
        if !c.hasCamera { layer?.backgroundColor = NSColor(white: 0.08, alpha: 1).cgColor } else { layer?.backgroundColor = nil }
        needsLayout = true
    }

    /// The level meter (0...1), called at display rate while audible.
    func setLevel(_ value: Float) {
        guard content?.audible == true else { return }
        // Fast attack, slower release, so it reads as a level, not a flicker.
        level = value > level ? value : max(value, level * 0.85)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        meter.frame = CGRect(x: 0, y: 0, width: bounds.width * CGFloat(min(max(level, 0), 1)), height: 3)
        CATransaction.commit()
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
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -6),
            label.topAnchor.constraint(equalTo: box.topAnchor, constant: 1),
            label.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -1),
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
        meter.frame = CGRect(x: 0, y: 0, width: w * CGFloat(min(max(level, 0), 1)), height: 3)
        outline.frame = bounds
        CATransaction.commit()
        let button: CGFloat = 26
        speaker.frame = CGRect(x: w - button - 6, y: 4, width: button, height: 22)
        let badgeSize = badges.fittingSize
        let badgeRight = speaker.isHidden ? w - 8 : speaker.frame.minX - 4
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
        if let tracking { removeTrackingArea(tracking) }
        let area = NSTrackingArea(
            rect: bounds, options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways, .inVisibleRect],
            owner: self)
        addTrackingArea(area)
        tracking = area
    }

    // MARK: Hover

    override func mouseEntered(with event: NSEvent) { showChrome() }
    override func mouseMoved(with event: NSEvent) { showChrome() }
    override func mouseExited(with event: NSEvent) { scheduleFade(after: Self.fadeOutAfterExit) }

    /// Fades in at once; fades out a few seconds after the last movement.
    func showChrome() {
        fadeTimer?.invalidate()
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
        fadeTimer?.invalidate()
        fadeTimer = Timer.scheduledTimer(withTimeInterval: delay, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.fadeOut() }
        }
    }

    func fadeOut() {
        fadeTimer?.invalidate()
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.35
            header.animator().alphaValue = 0
        }
    }

    @objc private func speakerClicked(_ sender: NSButton) {
        let additive = NSEvent.modifierFlags.contains(.option) || NSEvent.modifierFlags.contains(.command)
        onSpeaker(additive)
        showChrome()
    }
}
