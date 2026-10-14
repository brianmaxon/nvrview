import AppKit
import NVRCore
import QuartzCore
import AppBase

/// What one tile's chrome shows; rebuilt from the model, applied cheaply.
struct TileChromeContent: Equatable {
    var title: String
    var subtitle: String?
    /// The frame-rate readout, when the user turned it on.
    var rates: String?
    /// What is detected now for the camera (corner indicators).
    var indicators: [IndicatorItem] = []
    var corner: ChromeCorner = .fallback
    /// The camera's raised alert, as the header's lines (docs/UX.md "Alerts").
    var alertLines: [String]?
    var veil: TileVeil?
    /// The veil offers a Retry button (it has been up for a while).
    var retry = false
    var hasCamera: Bool
    var audible: Bool
    /// What the hover header shows (docs/UX.md "Tile header"); nil for a
    /// tile without a camera.
    var header: TileHeaderTile?
    /// "● REC 1:23" while the camera is recorded (always shown, not hover
    /// chrome; docs/CAPTURE.md "Indicators").
    var recordingPill: String? = nil
    var selected: Bool
    var editing: Bool
    var dropTarget: Bool
}

/// The lightweight layer above one tile of the Metal surface (docs/UX.md
/// "Chrome"): a header with the camera's name and its buttons, laid out by
/// `TileHeaderLayout` (what does not fit goes into "…"), that fades in at
/// once on hover and out a few seconds after the pointer stops
/// or leaves; corner indicators; a thin level meter along the top
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

    /// A header button was clicked: the item and the button (menus pop up
    /// under it).
    var onItem: (TileHeaderItem, NSView) -> Void = { _, _ in }
    var onDismissAlert: () -> Void = {}
    /// Items whose action is not built yet (drawn dimmed; a click says so).
    var isPending: (TileHeaderItem) -> Bool = { _ in false }
    /// The veil's Retry button.
    var onRetry: () -> Void = {}

    private let header = NSView()
    private let headerGradient = CAGradientLayer()
    private let title = NSTextField(labelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private let rates = NSTextField(labelWithString: "")
    /// The header's buttons, made when first shown.
    private var buttons: [TileHeaderItem: NSButton] = [:]
    /// The header as last laid out (kept for the model's hysteresis).
    private(set) var headerLayout: TileHeaderLayout?
    /// The title's natural width, for the string it was measured for.
    private var titleMeasure: (text: String, width: Double)?
    private let cornerView = CornerIndicatorView()
    /// The recording pill, outside the header so it stays when the header
    /// fades.
    private let pill = NSTextField(labelWithString: "")
    /// The alert's red border (blinks, or stays steady with Reduce Motion).
    private let alertBorder = CALayer()
    private let alertStrip = NSView()
    private let alertText = NSTextField(labelWithString: "")
    private let alertDismiss = NSButton()
    private var reduceMotionObserver: NSObjectProtocol?
    private static let alertLineHeight: CGFloat = 14
    private let veil = NSView()
    private let veilTitle = NSTextField(labelWithString: "")
    private let veilDetail = NSTextField(labelWithString: "")
    /// Under the message of a tile that has had none for a long time.
    private let retryButton = NSButton(title: "Retry", target: nil, action: nil)
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

        addSubview(cornerView)

        pill.font = .monospacedDigitSystemFont(ofSize: 10, weight: .bold)
        pill.textColor = .white
        pill.alignment = .center
        pill.wantsLayer = true
        pill.drawsBackground = false
        pill.layer?.backgroundColor = NSColor.systemRed.withAlphaComponent(0.85).cgColor
        pill.layer?.cornerRadius = 8
        pill.isHidden = true
        pill.setAccessibilityLabel("Recording")
        addSubview(pill)

        veil.wantsLayer = true
        veil.layer?.backgroundColor = NSColor(calibratedRed: 0.05, green: 0.055, blue: 0.063, alpha: 0.78).cgColor
        veilTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        veilTitle.alignment = .center
        veilDetail.font = .systemFont(ofSize: 11)
        veilDetail.textColor = NSColor.white.withAlphaComponent(0.7)
        veilDetail.alignment = .center
        veil.addSubview(veilTitle)
        veil.addSubview(veilDetail)
        retryButton.bezelStyle = .rounded
        retryButton.controlSize = .small
        retryButton.font = .systemFont(ofSize: 11)
        retryButton.target = self
        retryButton.action = #selector(retryClicked(_:))
        retryButton.refusesFirstResponder = false
        retryButton.toolTip = "Try this camera again now"
        retryButton.setAccessibilityLabel("Retry")
        retryButton.setAccessibilityHelp("Tries this camera's stream again now")
        retryButton.isHidden = true
        veil.addSubview(retryButton)
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

        alertBorder.borderColor = NSColor.systemRed.cgColor
        alertBorder.borderWidth = 4
        alertBorder.isHidden = true
        layer?.addSublayer(alertBorder)
        alertStrip.wantsLayer = true
        alertStrip.layer?.backgroundColor = NSColor(calibratedRed: 0.55, green: 0.05, blue: 0.05, alpha: 0.85).cgColor
        alertStrip.layer?.cornerRadius = 6
        alertText.font = .systemFont(ofSize: 11, weight: .medium)
        alertText.textColor = .white
        alertText.maximumNumberOfLines = 0
        alertText.lineBreakMode = .byTruncatingTail
        alertDismiss.title = "Dismiss"
        alertDismiss.bezelStyle = .rounded
        alertDismiss.controlSize = .small
        alertDismiss.target = self
        alertDismiss.action = #selector(dismissAlertClicked(_:))
        alertDismiss.toolTip = "Dismiss this alert (Return)"
        alertStrip.addSubview(alertText)
        alertStrip.addSubview(alertDismiss)
        alertStrip.isHidden = true
        header.addSubview(alertStrip)
        reduceMotionObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil, queue: .main
        ) { [weak self] _ in MainActor.assumeIsolated { self?.styleAlertBorder() } }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // Clicks reach the stage, except on the header's buttons.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let p = convert(point, from: superview)
        // The veil's Retry button takes clicks whether or not the header shows.
        if !retryButton.isHidden, !veil.isHidden, retryButton.convert(retryButton.bounds, to: self).contains(p) {
            return retryButton
        }
        guard header.alphaValue > 0.05, !header.isHidden else { return nil }
        for button in Array(buttons.values) + [alertDismiss]
        where !button.isHidden && (button !== alertDismiss || !alertStrip.isHidden) {
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
        cornerView.corner = c.corner
        pill.stringValue = c.recordingPill ?? ""
        pill.isHidden = c.recordingPill == nil || !c.hasCamera
        pill.setAccessibilityValue(c.recordingPill)
        setAlert(c.alertLines)
        cornerView.set(active: c.indicators)
        veil.isHidden = c.veil == nil || !c.hasCamera
        veilTitle.stringValue = c.veil?.title ?? ""
        veilDetail.stringValue = c.veil?.detail ?? ""
        veilDetail.isHidden = c.veil?.detail == nil
        retryButton.isHidden = !(c.retry && c.veil != nil && c.hasCamera)
        needsLayout = true
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

    // MARK: Layout

    override func layout() {
        super.layout()
        let w = bounds.width, h = bounds.height
        let alertHeight: CGFloat = alertStrip.isHidden ? 0 : CGFloat(alertLineCount) * Self.alertLineHeight + 12
        let headerHeight: CGFloat = (subtitle.isHidden ? 30 : 40) + (rates.isHidden ? 0 : 14) + alertHeight
        header.frame = CGRect(x: 0, y: 0, width: w, height: min(headerHeight, h))
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        headerGradient.frame = header.bounds
        meterTrack.frame = CGRect(x: 0, y: 0, width: w, height: 3)
        hoverTrack.frame = CGRect(x: 0, y: 0, width: w, height: 3)
        layoutMeters()
        outline.frame = bounds
        CATransaction.commit()
        cornerView.frame = bounds
        let buttonsLeft = layoutHeaderButtons(width: w)
        // The title keeps clear of the indicators when they share its edge.
        let taken = cornerView.occupiedWidth()
        let pillCorner = Self.pillCorner(cornerView.corner)
        let pillWidth = CGFloat(headerLayout?.metrics.pill ?? 64)
        if !pill.isHidden {
            let x = pillCorner == .bottomRight ? w - pillWidth - 8 : 8
            let y = pillCorner == .topLeft ? 6 : h - 24
            pill.frame = CGRect(x: x, y: y, width: pillWidth, height: 17)
        }
        let pillLeft: CGFloat = !pill.isHidden && pillCorner == .topLeft ? pillWidth + 4 : 0
        let titleLeft: CGFloat = 10 + pillLeft + (cornerView.corner == .topLeft ? taken : 0)
        let titleRight = buttonsLeft - 4 - (cornerView.corner == .topRight ? taken : 0)
        title.frame = CGRect(x: titleLeft, y: 7, width: max(titleRight - titleLeft, 0), height: 16)
        subtitle.frame = CGRect(x: 10, y: 22, width: max(w - 20, 0), height: 14)
        rates.frame = CGRect(x: 10, y: subtitle.isHidden ? 22 : 36, width: max(w - 20, 0), height: 14)
        veil.frame = bounds
        alertBorder.frame = bounds
        if !alertStrip.isHidden {
            let top = header.bounds.height - alertHeight + 2
            alertStrip.frame = CGRect(x: 6, y: top, width: max(w - 12, 0), height: alertHeight - 4)
            let dismissWidth: CGFloat = 74
            alertDismiss.frame = CGRect(x: alertStrip.bounds.width - dismissWidth - 6, y: 6, width: dismissWidth, height: 20)
            alertText.frame = CGRect(
                x: 8, y: 6, width: max(alertStrip.bounds.width - dismissWidth - 22, 0),
                height: CGFloat(alertLineCount) * Self.alertLineHeight)
        }
        veilTitle.frame = CGRect(x: 8, y: h / 2 - 18, width: max(w - 16, 0), height: 18)
        veilDetail.frame = CGRect(x: 8, y: h / 2 + 2, width: max(w - 16, 0), height: 16)
        let retryWidth: CGFloat = 64
        retryButton.frame = CGRect(
            x: (w - retryWidth) / 2, y: h / 2 + (veilDetail.isHidden ? 6 : 24), width: retryWidth, height: 22)
        emptyHint.frame = CGRect(x: 8, y: h / 2 - 8, width: max(w - 16, 0), height: 16)
        drop.frame = bounds
    }

    // MARK: Header buttons (docs/UX.md "Tile header")

    /// Lays the header out with the shared model at width `w` and places
    /// the visible buttons at the right edge, in display order. Returns
    /// where the buttons begin (the title ends before it).
    private func layoutHeaderButtons(width w: CGFloat) -> CGFloat {
        guard let tile = content?.header else {
            headerLayout = nil
            for b in buttons.values { b.isHidden = true }
            return w - 4
        }
        let text = title.stringValue
        if titleMeasure?.text != text {
            titleMeasure = (text, Double(ceil(title.attributedStringValue.size().width)))
        }
        let layout = TileHeaderLayout.compute(
            width: Double(w), titleWidth: titleMeasure?.width ?? 0, tile: tile, previous: headerLayout)
        headerLayout = layout
        let shown = layout.visible.filter(Self.isButton)
        let menu = TileHeaderLayout.menu(for: tile)
        let icon = CGFloat(layout.metrics.icon)
        var x = w - CGFloat(layout.metrics.padding) - icon * CGFloat(shown.count)
        let left = x
        for item in shown {
            let b = button(for: item)
            style(b, item, tile, menu.first { $0.item == item })
            b.frame = CGRect(x: x, y: 4, width: icon, height: 22)
            b.isHidden = false
            x += icon
        }
        for (item, b) in buttons where !shown.contains(item) { b.isHidden = true }
        return left
    }

    /// Where the recording pill goes: the corner opposite the corner
    /// indicators, except that the top right (the header's buttons) moves
    /// to the top left.
    static func pillCorner(_ indicators: ChromeCorner) -> ChromeCorner {
        let opposite = indicators.opposite
        return opposite == .topRight ? .topLeft : opposite
    }

    /// The pill's text while it shows (self-test).
    var pillText: String? { pill.isHidden ? nil : pill.stringValue }

    /// The pill, the title and Dismiss are not buttons of the row: the
    /// alert's Dismiss stays in its strip below the title, and the pill is
    /// drawn on the tile itself (it stays when the header fades).
    private static func isButton(_ item: TileHeaderItem) -> Bool {
        switch item {
        case .recordingPill, .title, .dismissAlert: false
        default: true
        }
    }

    private func button(for item: TileHeaderItem) -> NSButton {
        if let b = buttons[item] { return b }
        let b = HeaderButton(item: item)
        b.isBordered = false
        b.bezelStyle = .shadowlessSquare
        b.imagePosition = .imageOnly
        b.contentTintColor = .white
        b.target = self
        b.action = #selector(headerButtonClicked(_:))
        b.isHidden = true
        header.addSubview(b)
        buttons[item] = b
        return b
    }

    /// The symbol, label and tooltip of a header button in the tile's state.
    private func style(_ b: NSButton, _ item: TileHeaderItem, _ tile: TileHeaderTile, _ entry: TileHeaderMenuEntry?) {
        let label = entry?.label ?? (item == .more ? "More" : "")
        let symbol: String
        switch item {
        case .speaker: symbol = tile.listening ? "speaker.wave.2.fill" : "speaker.slash.fill"
        case .record: symbol = tile.recording ? "stop.circle" : "record.circle"
        case .savePicture: symbol = "camera"
        case .conversation: symbol = tile.conversation ? "phone.down.fill" : "phone"
        case .panTilt: symbol = "arrow.up.and.down.and.arrow.left.and.right"
        case .presets: symbol = "list.number"
        case .lens: symbol = "camera.aperture"
        case .resetZoom: symbol = "1.magnifyingglass"
        case .quality: symbol = "slider.horizontal.3"
        case .fisheye: symbol = "pano"
        default: symbol = "ellipsis.circle"
        }
        let pending = isPending(item)
        let hint = entry?.macShortcut ?? ""
        var tip = hint.isEmpty ? label : "\(label) (\(hint))"
        if item == .speaker { tip += " · Option-click to listen to several" }
        if pending { tip += " · coming soon" }
        guard b.toolTip != tip || (b as? HeaderButton)?.symbol != symbol else { return }
        (b as? HeaderButton)?.symbol = symbol
        b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
            ?? NSImage(systemSymbolName: "circle", accessibilityDescription: label)
        b.alphaValue = pending ? 0.45 : 1
        b.toolTip = tip
        b.setAccessibilityLabel(item == .speaker ? (tile.listening ? "Stop listening" : "Listen") : label)
    }

    @objc private func headerButtonClicked(_ sender: NSButton) {
        guard let item = (sender as? HeaderButton)?.item else { return }
        onItem(item, sender)
        showChrome()
    }

    /// The header items shown as buttons, left to right (self-test).
    var shownHeaderButtons: [TileHeaderItem] {
        buttons.filter { !$0.value.isHidden }.sorted { $0.value.frame.minX < $1.value.frame.minX }.map(\.key)
    }

    /// The buttons' frames, in this view's coordinates (self-test).
    var headerButtonFrames: [TileHeaderItem: CGRect] {
        buttons.filter { !$0.value.isHidden }.mapValues { header.convert($0.frame, to: self) }
    }

    /// The title's frame, in this view's coordinates (self-test).
    var titleFrame: CGRect { header.convert(title.frame, to: self) }

    /// Clicks a header button as the pointer would (self-test).
    func pressHeader(_ item: TileHeaderItem) {
        guard let b = buttons[item], !b.isHidden else { return }
        b.performClick(nil)
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

    /// The header's area in the superview's coordinates while it is shown
    /// (a drag there swaps the tile), else nil.
    var shownHeaderFrame: CGRect? {
        guard header.alphaValue > 0.05, !header.isHidden, !isHidden else { return nil }
        return convert(header.frame, to: superview)
    }

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

    private var alertLineCount = 0

    /// Shows or clears the alert: the red border and the header's strip.
    private func setAlert(_ lines: [String]?) {
        let wasShown = !alertStrip.isHidden
        alertStrip.isHidden = lines == nil
        alertBorder.isHidden = lines == nil
        if let lines {
            alertLineCount = lines.count
            alertText.stringValue = lines.joined(separator: "\n")
            alertText.setAccessibilityLabel("Alert: " + lines.joined(separator: ", "))
            if !wasShown { styleAlertBorder() }
        } else {
            alertBorder.removeAnimation(forKey: "blink")
        }
        needsLayout = true
    }

    /// Blinking (a layer animation: no main-thread work) or steady.
    private func styleAlertBorder() {
        alertBorder.removeAnimation(forKey: "blink")
        guard !alertBorder.isHidden else { return }
        let mode = AlertBorder.mode(reduceMotion: NSWorkspace.shared.accessibilityDisplayShouldReduceMotion)
        alertBorder.opacity = 1
        if mode == .blinking {
            let blink = CAKeyframeAnimation(keyPath: "opacity")
            blink.values = [1.0, 0.0]
            blink.keyTimes = [0, 0.5]
            blink.calculationMode = .discrete
            blink.duration = AlertBorder.blinkPeriod
            blink.repeatCount = .infinity
            alertBorder.add(blink, forKey: "blink")
        }
    }

    @objc private func retryClicked(_ sender: NSButton) { onRetry() }

    /// Whether the veil's Retry button is showing (for the self-test).
    var isRetryShown: Bool { !retryButton.isHidden }

    /// Clicks the veil's Retry button, as the pointer or keyboard would (for
    /// the self-test).
    func pressRetry() { if !retryButton.isHidden { retryButton.performClick(nil) } }

    @objc private func dismissAlertClicked(_ sender: NSButton) { onDismissAlert() }

    /// Whether the alert strip is showing (for the self-test).
    var isAlertShown: Bool { !alertStrip.isHidden }

}

/// A header button that knows its item.
@MainActor
private final class HeaderButton: NSButton {
    let item: TileHeaderItem
    /// The symbol it shows now.
    var symbol = ""

    init(item: TileHeaderItem) {
        self.item = item
        super.init(frame: .zero)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }
}
