import AppKit
import NVRCore

/// The collapsed sidebar's edge reveal (docs/UX.md "Sidebar"): a thin
/// tracking strip at the stage's left edge and a panel that slides over the
/// stage when the pointer rests there. The panel is a sibling above the
/// stage, so the stage and its drawable are never resized. Timing lives in
/// `SidebarReveal`; this class feeds it pointer changes and arms one timer
/// for its next deadline (no per-frame work while idle).
@MainActor
final class SidebarOverlay: NSObject {
    static let stripWidth: CGFloat = 6
    static let panelWidth: CGFloat = 260
    static let animationDuration: TimeInterval = 0.18

    private(set) var reveal = SidebarReveal()
    /// The time source: the viewer's clock (the system clock in the app, the
    /// self-test's manual one there).
    var clock: () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    /// When false, no timer is armed for the next deadline: whoever moves the
    /// clock calls `tick()` (the self-test, after advancing its manual clock;
    /// a real timer could never see a manual clock move).
    var armsTimer = true
    /// Docks the sidebar (the pin control).
    var onPin: () -> Void = {}
    /// Whether the overlay's content has a text field being edited.
    var isEditing: () -> Bool = { false }
    /// Whether the primary mouse button is down (a drag from the overlay).
    var isButtonDown: () -> Bool = { NSEvent.pressedMouseButtons & 1 != 0 }

    let panel = OverlayPanelView()
    private let strip = EdgeStripView()
    private let content: NSView
    private weak var root: NSView?
    private var leading: NSLayoutConstraint!
    private var timer: Timer?
    private var menuOpen = false
    private var observers: [NSObjectProtocol] = []

    var isShown: Bool { reveal.isShown }

    init(content: NSView) {
        self.content = content
        super.init()
        panel.build(content: content, pin: self, action: #selector(pinPressed))
        panel.isHidden = true
        strip.owner = self
        panel.owner = self
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: NSMenu.didBeginTrackingNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { if let self { self.menuOpen = self.isShown } }
            })
        observers.append(
            center.addObserver(forName: NSMenu.didEndTrackingNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.menuOpen = false
                    // The pointer may have left while the menu was open.
                    self.evaluatePointer()
                }
            })
    }

    isolated deinit {
        timer?.invalidate()
        for o in observers { NotificationCenter.default.removeObserver(o) }
    }

    /// Adds the strip and the panel above the stage inside `root`.
    func install(in root: NSView) {
        self.root = root
        strip.translatesAutoresizingMaskIntoConstraints = false
        panel.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(strip)
        root.addSubview(panel)
        leading = panel.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: -Self.panelWidth - 24)
        NSLayoutConstraint.activate([
            strip.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            strip.topAnchor.constraint(equalTo: root.topAnchor),
            strip.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            strip.widthAnchor.constraint(equalToConstant: Self.stripWidth),
            leading,
            panel.topAnchor.constraint(equalTo: root.safeAreaLayoutGuide.topAnchor),
            panel.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            panel.widthAnchor.constraint(equalToConstant: Self.panelWidth),
        ])
        strip.isHidden = true
    }

    /// The sidebar is collapsed (the overlay can reveal) or docked.
    func setCollapsed(_ collapsed: Bool) {
        reveal.setEnabled(collapsed)
        strip.isHidden = !collapsed
        if !collapsed { applyVisibility(animated: false) }
        rearm()
    }

    /// Slides away at once without animation (entering full screen, docking).
    func dismissNow() {
        reveal.setEnabled(false)
        reveal.setEnabled(strip.isHidden == false)
        applyVisibility(animated: false)
        rearm()
    }

    /// Where the pointer is now, from the window (so a missed enter or exit
    /// event cannot leave the state wrong).
    func evaluatePointer() {
        guard let root, let window = root.window else { return }
        pointerAt(simulatedPointer ?? root.convert(window.mouseLocationOutsideOfEventStream, from: nil))
    }

    /// The self-test's pointer (in the stage container's coordinates), used
    /// in place of the real one while set.
    var simulatedPointer: CGPoint?

    /// The pointer at `p` in the stage container's coordinates.
    func pointerAt(_ p: CGPoint) {
        guard let root else { return }
        let inRoot = root.bounds.contains(p)
        let inEdge = inRoot && p.x < Self.stripWidth && !strip.isHidden
        var inOverlay = false
        if isShown {
            var f = panel.frame
            f.origin.x = 0
            inOverlay = f.contains(p)
        }
        reveal.pointer(inEdge: inEdge, inOverlay: inOverlay, now: clock())
        rearm()
    }

    // MARK: Timer

    /// Runs what is due at the clock's time now (also called by the window's
    /// slow timer and by the self-test after moving its clock).
    func tick() {
        guard let deadline = reveal.deadline, clock() >= deadline else { return }
        fired()
    }

    private func rearm() {
        timer?.invalidate()
        timer = nil
        guard armsTimer, let deadline = reveal.deadline else { return }
        let t = Timer(timeInterval: max(0, deadline - clock()), repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.fired() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func fired() {
        timer = nil
        var holds: Set<SidebarReveal.Hold> = []
        if isButtonDown() { holds.insert(.drag) }
        if menuOpen { holds.insert(.menu) }
        if isEditing() { holds.insert(.editing) }
        let was = reveal.isShown
        reveal.tick(now: clock(), holds: holds)
        if reveal.isShown != was { applyVisibility(animated: true) }
        // A reveal under the pointer starts the hide clock if it is elsewhere.
        if reveal.isShown, !was { evaluatePointer() }
        rearm()
    }

    // MARK: Showing

    private func applyVisibility(animated: Bool) {
        let show = reveal.isShown
        let reduce = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        let hiddenX = -Self.panelWidth - 24
        if show { panel.isHidden = false }
        guard animated else {
            leading.constant = show ? 0 : hiddenX
            panel.alphaValue = 1
            panel.isHidden = !show
            return
        }
        if reduce {
            // Fade in place instead of sliding.
            leading.constant = 0
            if show { panel.alphaValue = 0 }
        }
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = Self.animationDuration
            ctx.allowsImplicitAnimation = true
            if reduce {
                panel.animator().alphaValue = show ? 1 : 0
            } else {
                leading.animator().constant = show ? 0 : hiddenX
            }
        } completionHandler: { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.reveal.isShown == show else { return }
                if !show { self.panel.isHidden = true }
                self.panel.alphaValue = 1
                if !show { self.leading.constant = hiddenX }
            }
        }
    }

    @objc private func pinPressed() { onPin() }

    // MARK: Tracking callbacks

    fileprivate func trackingChanged() { evaluatePointer() }
}

/// Transparent strip at the stage's left edge; tracks the pointer but never
/// takes a click (the stage under it gets them).
@MainActor
final class EdgeStripView: NSView {
    weak var owner: SidebarOverlay?

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for a in trackingAreas { removeTrackingArea(a) }
        addTrackingArea(
            NSTrackingArea(
                rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { owner?.trackingChanged() }
    override func mouseExited(with event: NSEvent) { owner?.trackingChanged() }
}

/// The overlay panel: material background, shadow, a pin control above the
/// sidebar's own content.
@MainActor
final class OverlayPanelView: NSView {
    weak var owner: SidebarOverlay?

    func build(content: NSView, pin target: AnyObject, action: Selector) {
        wantsLayer = true
        layer?.masksToBounds = false
        layer?.shadowColor = NSColor.black.cgColor
        layer?.shadowOpacity = 0.4
        layer?.shadowRadius = 10
        layer?.shadowOffset = CGSize(width: 2, height: 0)
        layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor

        let material = NSVisualEffectView()
        material.material = .sidebar
        material.blendingMode = .withinWindow
        material.state = .active
        material.translatesAutoresizingMaskIntoConstraints = false
        addSubview(material)

        let pinButton = NSButton(
            image: NSImage(systemSymbolName: "pin", accessibilityDescription: "Dock Sidebar")!, target: target,
            action: action)
        pinButton.bezelStyle = .accessoryBarAction
        pinButton.isBordered = false
        pinButton.toolTip = "Dock the sidebar (S)"
        pinButton.setAccessibilityLabel("Dock Sidebar")
        pinButton.translatesAutoresizingMaskIntoConstraints = false
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        addSubview(pinButton)
        NSLayoutConstraint.activate([
            material.leadingAnchor.constraint(equalTo: leadingAnchor),
            material.trailingAnchor.constraint(equalTo: trailingAnchor),
            material.topAnchor.constraint(equalTo: topAnchor),
            material.bottomAnchor.constraint(equalTo: bottomAnchor),
            pinButton.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            pinButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            pinButton.widthAnchor.constraint(equalToConstant: 24),
            pinButton.heightAnchor.constraint(equalToConstant: 24),
            content.topAnchor.constraint(equalTo: pinButton.bottomAnchor, constant: 2),
            content.leadingAnchor.constraint(equalTo: leadingAnchor),
            content.trailingAnchor.constraint(equalTo: trailingAnchor),
            content.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    override func layout() {
        super.layout()
        layer?.shadowPath = CGPath(rect: bounds, transform: nil)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for a in trackingAreas { removeTrackingArea(a) }
        addTrackingArea(
            NSTrackingArea(
                rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self, userInfo: nil))
    }

    override func mouseEntered(with event: NSEvent) { owner?.trackingChanged() }
    override func mouseExited(with event: NSEvent) { owner?.trackingChanged() }
}
