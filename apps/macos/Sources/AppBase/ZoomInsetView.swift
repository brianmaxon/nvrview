import AppKit
import NVRCore
import QuartzCore

/// The small map shown in a zoomed tile's corner (docs/UX.md "Digital
/// zoom"): the whole picture's outline with the visible region marked. Plain
/// layers above the Metal surface: it draws no video and costs the renderer
/// nothing. It takes no clicks.
@MainActor
package final class ZoomInsetView: NSView {
    package static let maxWidth: CGFloat = 96
    package static let margin: CGFloat = 8

    private let box = CALayer()
    private let region = CALayer()
    private var shown = false
    private var lastRegion: CGRect?

    package override var isFlipped: Bool { true }
    package override func hitTest(_ point: NSPoint) -> NSView? { nil }

    package init() {
        super.init(frame: .zero)
        wantsLayer = true
        box.backgroundColor = NSColor.black.withAlphaComponent(0.45).cgColor
        box.borderColor = NSColor.white.withAlphaComponent(0.55).cgColor
        box.borderWidth = 1
        box.cornerRadius = 2
        region.backgroundColor = NSColor.white.withAlphaComponent(0.18).cgColor
        region.borderColor = NSColor.white.cgColor
        region.borderWidth = 1.5
        layer?.addSublayer(box)
        box.addSublayer(region)
        layer?.opacity = 0
        isHidden = true
    }

    @available(*, unavailable)
    package required init?(coder: NSCoder) { fatalError("not used") }

    /// Where the map sits for a tile whose picture appears in `picture`
    /// (points, in the stage overlay): its bottom left corner, the
    /// picture's shape, at most a quarter of its width.
    package static func frame(inPicture picture: CGRect, aspect: CGFloat) -> CGRect {
        guard picture.width > 0, picture.height > 0, aspect > 0 else { return .zero }
        var w = min(maxWidth, picture.width / 4)
        var h = w / aspect
        if h > picture.height / 3 {
            h = picture.height / 3
            w = h * aspect
        }
        return CGRect(x: picture.minX + margin, y: picture.maxY - margin - h, width: w, height: h).integral
    }

    /// The visible region as a fraction of the whole picture.
    package func update(region r: CGRect) {
        guard r != lastRegion || region.frame.isEmpty else { return }
        lastRegion = r
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        box.frame = bounds
        let b = bounds
        region.frame = CGRect(
            x: b.minX + r.minX * b.width, y: b.minY + r.minY * b.height, width: max(r.width * b.width, 2),
            height: max(r.height * b.height, 2))
        CATransaction.commit()
    }

    package override func layout() {
        super.layout()
        if let r = lastRegion {
            lastRegion = nil
            update(region: r)
        }
    }

    /// Fades in or out (with the chrome, and only while zoomed).
    package func setShown(_ visible: Bool) {
        guard visible != shown else { return }
        shown = visible
        if visible { isHidden = false }
        let animation = CABasicAnimation(keyPath: "opacity")
        animation.duration = visible ? 0.12 : 0.35
        layer?.add(animation, forKey: "opacity")
        layer?.opacity = visible ? 1 : 0
    }

    package var isShown: Bool { shown }
}
