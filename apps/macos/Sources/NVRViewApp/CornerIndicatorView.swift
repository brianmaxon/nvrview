import AppKit
import NVRCore

/// The corner indicators of one tile (docs/UX.md "Corner indicators"): small
/// monochrome SF Symbols with a soft shadow, one per active detection kind,
/// in the configured corner. Always visible (not part of the hover chrome),
/// fading in and out as kinds start and stop, with a short hold after a stop.
///
/// Plain layers that change only when a detection does: nothing here runs per
/// frame. It never takes clicks.
@MainActor
final class CornerIndicatorView: NSView, NSViewToolTipOwner {
    static let fadeSeconds: TimeInterval = 0.25
    static let gap: Double = 5
    static let inset: Double = 8
    /// Room left at a top-right corner for the header's speaker button.
    static let speakerReserve: Double = 30

    var corner: ChromeCorner = .fallback {
        didSet { if corner != oldValue { needsLayout = true } }
    }

    private var hold = IndicatorHold()
    private var icons: [IndicatorKind: CALayer] = [:]
    /// Kinds shown now, in order (fading-out icons are not in it).
    private(set) var shown: [IndicatorItem] = []
    private var timer: Timer?

    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Detections")
    }
    required init?(coder: NSCoder) { fatalError("not used") }

    private static func now() -> Double { ProcessInfo.processInfo.systemUptime }

    /// What is detected now for this tile's camera.
    func set(active: [IndicatorItem]) {
        hold.update(active, now: Self.now())
        refresh()
    }

    private func refresh() {
        let now = Self.now()
        let visible = hold.visible(now: now)
        timer?.invalidate()
        timer = nil
        if let next = hold.nextExpiry(now: now) {
            timer = Timer.scheduledTimer(withTimeInterval: next - now + 0.02, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated { self?.refresh() }
            }
        }
        guard visible != shown else { return }
        let kinds = Set(visible.map(\.kind))
        for (kind, icon) in icons where !kinds.contains(kind) {
            icons[kind] = nil
            fade(icon, to: 0) { icon.removeFromSuperlayer() }
        }
        for item in visible where icons[item.kind] == nil {
            let icon = Self.makeIcon(item.kind)
            icon.opacity = 0
            icons[item.kind] = icon
            layer?.addSublayer(icon)
            fade(icon, to: 1)
        }
        shown = visible
        needsLayout = true
    }

    private func fade(_ layer: CALayer, to opacity: Float, done: (() -> Void)? = nil) {
        CATransaction.begin()
        CATransaction.setAnimationDuration(Self.fadeSeconds)
        CATransaction.setCompletionBlock(done)
        layer.opacity = opacity
        CATransaction.commit()
    }

    private static func makeIcon(_ kind: IndicatorKind) -> CALayer {
        let l = CALayer()
        let image = bakedSymbol(kind)
        l.contents = image.cgImage(forProposedRect: nil, context: nil, hints: nil)
        l.contentsGravity = .resizeAspect
        l.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        return l
    }

    /// The symbol, tinted and with its soft shadow drawn into the bitmap once
    /// (a live shadow on a view above the Metal surface costs an offscreen
    /// pass on every display refresh).
    private static func bakedSymbol(_ kind: IndicatorKind) -> NSImage {
        let points: CGFloat = 20, pad: CGFloat = 3
        let symbol =
            NSImage(systemSymbolName: kind.symbolName, accessibilityDescription: kind.label)
            ?? NSImage(systemSymbolName: IndicatorKind.other.symbolName, accessibilityDescription: nil) ?? NSImage()
        let color =
            kind.isSound
            ? NSColor(calibratedRed: 1, green: 0.78, blue: 0.4, alpha: 0.95) : NSColor.white.withAlphaComponent(0.9)
        // Tinted copy, fitted in the square.
        let tinted = NSImage(size: NSSize(width: points, height: points), flipped: false) { rect in
            let fit = symbol.size
            let k = min(rect.width / max(fit.width, 1), rect.height / max(fit.height, 1))
            let r = NSRect(x: (rect.width - fit.width * k) / 2, y: (rect.height - fit.height * k) / 2, width: fit.width * k, height: fit.height * k)
            symbol.draw(in: r)
            color.setFill()
            r.fill(using: .sourceAtop)
            return true
        }
        let total = points + 2 * pad
        let out = NSImage(size: NSSize(width: total, height: total), flipped: false) { _ in
            let s = NSShadow()
            s.shadowColor = NSColor.black.withAlphaComponent(0.75)
            s.shadowOffset = NSSize(width: 0, height: -1)
            s.shadowBlurRadius = 2.5
            NSGraphicsContext.saveGraphicsState()
            s.set()
            tinted.draw(in: NSRect(x: pad, y: pad, width: points, height: points))
            NSGraphicsContext.restoreGraphicsState()
            return true
        }
        // Freeze it: a drawing-handler image would run its handler again on
        // every redraw.
        var rect = CGRect(origin: .zero, size: out.size)
        let frozen = out.cgImage(forProposedRect: &rect, context: nil, hints: nil)
            .map { NSImage(cgImage: $0, size: out.size) } ?? out
        frozen.accessibilityDescription = kind.label
        return frozen
    }

    /// The symbol size for a tile of this size.
    static func symbolSize(for size: CGSize) -> Double { min(max(min(size.width, size.height) * 0.07, 12), 20) }

    /// How much of the corner's edge the row takes, with its padding (zero
    /// when nothing shows), so the header's title can keep clear of it.
    func occupiedWidth() -> CGFloat {
        guard !shown.isEmpty else { return 0 }
        let s = Self.symbolSize(for: bounds.size)
        return CGFloat(Double(shown.count) * s + Double(shown.count - 1) * Self.gap + Self.inset)
    }

    private struct LayoutKey: Equatable {
        var size: CGSize
        var corner: ChromeCorner
        var shown: [IndicatorItem]
    }
    private var laidOut: LayoutKey?

    override func layout() {
        super.layout()
        // The chrome re-lays-out often; this only when something moved.
        let key = LayoutKey(size: bounds.size, corner: corner, shown: shown)
        if key == laidOut { return }
        laidOut = key
        let s = Self.symbolSize(for: bounds.size)
        let reserve = corner == .topRight ? Self.speakerReserve : 0
        let origin = corner.rowOrigin(
            count: shown.count, symbol: s, gap: Self.gap, inset: Self.inset,
            in: (Double(bounds.width) - reserve, Double(bounds.height)))
        let grow = s * 3 / 20
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        removeAllToolTips()
        for (i, item) in shown.enumerated() {
            let x = origin.x + Double(i) * (s + Self.gap)
            icons[item.kind]?.frame = CGRect(x: x - grow, y: origin.y - grow, width: s + 2 * grow, height: s + 2 * grow)
            toolTipOwners[i] = item.description
            addToolTip(CGRect(x: x, y: origin.y, width: s, height: s), owner: self, userData: UnsafeMutableRawPointer(bitPattern: i + 1))
        }
        CATransaction.commit()
    }

    private var toolTipOwners: [Int: String] = [:]

    nonisolated func view(_ view: NSView, stringForToolTip tag: NSView.ToolTipTag, point: NSPoint, userData data: UnsafeMutableRawPointer?) -> String {
        guard let data else { return "" }
        let i = Int(bitPattern: data) - 1
        return MainActor.assumeIsolated { toolTipOwners[i] ?? "" }
    }

    override func accessibilityChildren() -> [Any]? {
        shown.map { item in
            let e = NSAccessibilityElement()
            e.setAccessibilityRole(.image)
            e.setAccessibilityLabel(item.description)
            e.setAccessibilityParent(self)
            return e
        }
    }
}
