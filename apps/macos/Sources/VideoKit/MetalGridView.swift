import AppKit
import CoreGraphics
import QuartzCore

/// A view backed by a `CAMetalLayer` that draws a `GridRenderer` from a
/// `CAMetalDisplayLink` on the main run loop. Every refresh it calls
/// `prepare` (where the owner updates the layout) and then draws only if
/// something changed; otherwise the layer keeps showing the last frame.
///
/// Size changes are coalesced (`DrawableSizePolicy`): while the view is
/// resized continuously (full screen, window resize) the layer keeps the
/// drawable it has, scaled to its bounds, and takes a new drawable size at
/// most every 100 ms and once the size settles. It presents every refresh
/// throughout; `prepare` gets the drawable size actually in use.
@MainActor
public final class MetalGridView: NSView {
    public let renderer: GridRenderer

    /// Called on every refresh before drawing, with the drawable size in
    /// pixels. Update `renderer.tiles` here when the layout changes.
    public var prepare: ((GridRenderer, CGSize) -> Void)?

    /// Refreshes seen and frames actually drawn.
    public private(set) var refreshes = 0
    public private(set) var framesDrawn = 0
    /// How many times the drawable size changed.
    public private(set) var drawableResizes = 0

    /// The size of the drawables the layer hands out now, in pixels.
    public var drawableSize: CGSize { metalLayer.drawableSize }

    private var sizePolicy = DrawableSizePolicy()

    private let metalLayer = CAMetalLayer()
    private var displayLink: CAMetalDisplayLink?
    private var captureRequests: [(RenderedImage) -> Void] = []

    public init(renderer: GridRenderer) {
        self.renderer = renderer
        super.init(frame: NSRect(x: 0, y: 0, width: 640, height: 480))
        metalLayer.device = renderer.device
        metalLayer.pixelFormat = GridRenderer.pixelFormat
        metalLayer.framebufferOnly = true
        metalLayer.isOpaque = true
        metalLayer.backgroundColor = NSColor.black.cgColor
        metalLayer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        metalLayer.maximumDrawableCount = 3
        // A drawable of the previous size is stretched to the bounds until
        // the new size is applied.
        metalLayer.contentsGravity = .resize
        layerContentsPlacement = .scaleAxesIndependently
        wantsLayer = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override public func makeBackingLayer() -> CALayer { metalLayer }

    override public var isOpaque: Bool { true }

    /// Draws the next refresh even if nothing changed, also into an
    /// offscreen texture, and hands the readback of exactly what was
    /// presented to `handler` (on the main thread).
    public func captureNextFrame(_ handler: @escaping (RenderedImage) -> Void) {
        captureRequests.append(handler)
    }

    override public func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window != nil {
            updateDrawableSize()
            if displayLink == nil {
                let link = CAMetalDisplayLink(metalLayer: metalLayer)
                link.delegate = self
                link.add(to: .main, forMode: .common)
                displayLink = link
            }
        } else {
            displayLink?.invalidate()
            displayLink = nil
        }
    }

    override public func layout() {
        super.layout()
        updateDrawableSize()
    }

    override public func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        updateDrawableSize()
    }

    private func updateDrawableSize() {
        let scale = window?.backingScaleFactor ?? 1
        metalLayer.contentsScale = scale
        let size = CGSize(width: (bounds.width * scale).rounded(), height: (bounds.height * scale).rounded())
        if let apply = sizePolicy.want(size, now: CACurrentMediaTime()) { setDrawableSize(apply) }
    }

    private func setDrawableSize(_ size: CGSize) {
        guard size != metalLayer.drawableSize else { return }
        metalLayer.drawableSize = size
        drawableResizes += 1
    }

    fileprivate func refresh(_ drawable: any CAMetalDrawable) {
        refreshes += 1
        // Takes effect from the next drawable; this one is still presented.
        if let apply = sizePolicy.tick(now: CACurrentMediaTime()) { setDrawableSize(apply) }
        let texture = drawable.texture
        prepare?(renderer, CGSize(width: texture.width, height: texture.height))
        let capture = !captureRequests.isEmpty
        guard capture || renderer.needsDisplay(width: texture.width, height: texture.height) else { return }
        let image = renderer.draw(to: drawable, capture: capture)
        framesDrawn += 1
        if capture, let image {
            let handlers = captureRequests
            captureRequests.removeAll()
            for handler in handlers { handler(image) }
        }
    }
}

// Isolated conformance: the link is scheduled on the main run loop (see
// viewDidMoveToWindow), so its callbacks arrive on the main actor.
extension MetalGridView: @MainActor CAMetalDisplayLinkDelegate {
    public func metalDisplayLink(_ link: CAMetalDisplayLink, needsUpdate update: CAMetalDisplayLink.Update) {
        refresh(update.drawable)
    }
}
