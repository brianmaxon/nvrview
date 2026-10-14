import AppKit
import NVRCore
import SwiftUI

// The Events window's picture helpers (docs/UX.md "Events"): a split whose
// divider is remembered, the method switches, the picture's context menu,
// and the large viewer (its own resizable window: the same switches, arrow
// keys between frames, Esc to close, wheel or pinch zoom with drag pan over
// the live tiles' `TileZoom`).

/// Two panes with a draggable divider; the position (a fraction of the
/// space) is kept in the user defaults under `key`.
struct PersistentSplit<First: View, Second: View>: View {
    let vertical: Bool
    let key: String
    let minFirst: CGFloat
    let minSecond: CGFloat
    let first: First
    let second: Second
    @State private var fraction: Double

    static var thickness: CGFloat { 7 }

    init(
        vertical: Bool, key: String, defaultFraction: Double, minFirst: CGFloat, minSecond: CGFloat,
        defaults: UserDefaults = .standard, @ViewBuilder first: () -> First, @ViewBuilder second: () -> Second
    ) {
        self.vertical = vertical
        self.key = key
        self.minFirst = minFirst
        self.minSecond = minSecond
        self.first = first()
        self.second = second()
        _fraction = State(initialValue: defaults.object(forKey: key) as? Double ?? defaultFraction)
    }

    var body: some View {
        GeometryReader { geo in
            let total = vertical ? geo.size.height : geo.size.width
            let f = SplitLayout.clamped(
                fraction: fraction, total: total - Self.thickness, minFirst: minFirst, minSecond: minSecond)
            let length = ((total - Self.thickness) * f).rounded()
            let layout = vertical ? AnyLayout(VStackLayout(spacing: 0)) : AnyLayout(HStackLayout(spacing: 0))
            layout {
                first.frame(
                    width: vertical ? nil : length, height: vertical ? length : nil
                )
                .frame(maxWidth: vertical ? .infinity : nil, maxHeight: vertical ? nil : .infinity)
                .clipped()
                divider(total: total - Self.thickness)
                second.frame(maxWidth: .infinity, maxHeight: .infinity).clipped()
            }
            .coordinateSpace(name: Self.space)
        }
    }

    private static var space: String { "persistent-split" }

    private func divider(total: CGFloat) -> some View {
        ZStack {
            Color.clear
            Rectangle().fill(Color(nsColor: .separatorColor))
                .frame(width: vertical ? nil : 1, height: vertical ? 1 : nil)
        }
        .frame(width: vertical ? nil : Self.thickness, height: vertical ? Self.thickness : nil)
        .contentShape(Rectangle())
        .pointerStyle(vertical ? .rowResize : .columnResize)
        .gesture(
            DragGesture(minimumDistance: 1, coordinateSpace: .named(Self.space))
                .onChanged { v in
                    let position = (vertical ? v.location.y : v.location.x) - Self.thickness / 2
                    fraction = SplitLayout.clamped(
                        fraction: SplitLayout.fraction(at: position, total: total), total: total, minFirst: minFirst,
                        minSecond: minSecond)
                }
                .onEnded { _ in UserDefaults.standard.set(fraction, forKey: key) }
        )
        .accessibilityElement()
        .accessibilityLabel(vertical ? "Picture and text divider" : "List and detail divider")
    }
}

/// A checkbox per method: its boxes shown or not, with its color and shape.
struct MethodToggles: View {
    @Bindable var events: SmartAlertEventsModel
    let evidence: AlertEvidence

    var body: some View {
        HStack(spacing: 14) {
            ForEach(Array(evidence.methods.enumerated()), id: \.element.name) { i, m in
                Toggle(isOn: Binding(get: { events.visibleMethods.contains(m.name) }, set: { _ in events.toggleMethod(m.name) })) {
                    HStack(spacing: 4) {
                        Image(systemName: EvidenceView.symbols[i % EvidenceView.symbols.count])
                            .foregroundStyle(EvidenceView.colors[i % EvidenceView.colors.count])
                        Text(m.title)
                    }
                }
                .toggleStyle(.checkbox)
            }
            Spacer(minLength: 0)
        }
        .font(.callout)
    }
}

/// The picture's context menu in the pane.
struct EvidencePictureMenu: View {
    let events: SmartAlertEventsModel

    var body: some View {
        Button("Open in Preview") { events.openInPreview() }
        Button("Save Picture With Detection Boxes As…") { events.savePictureAs(withBoxes: true, window: NSApp.keyWindow) }
            .disabled(!(events.subject?.hasBoxes ?? false))
        Button("Save Picture Only As…") { events.savePictureAs(withBoxes: false, window: NSApp.keyWindow) }
        Button("Copy") { events.copyPicture() }
        if events.pinnedFolder != nil {
            Divider()
            Button("Show in Finder") { events.showInFinder() }
        }
    }
}

// MARK: The large viewer

/// A window that passes the viewer's keys on (arrows, Esc) wherever the
/// focus is inside it.
final class EvidenceViewerWindow: NSWindow {
    var onKey: (EvidenceViewerKey) -> Void = { _ in }

    override func keyDown(with event: NSEvent) {
        if let key = EvidenceViewerKey(keyCode: event.keyCode) { onKey(key) } else { super.keyDown(with: event) }
    }

    override func cancelOperation(_ sender: Any?) { onKey(.close) }
}

@MainActor
final class EvidenceViewerWindowController: NSWindowController, NSWindowDelegate {
    let events: SmartAlertEventsModel
    var onClose: () -> Void = {}

    init(events: SmartAlertEventsModel) {
        self.events = events
        let window = EvidenceViewerWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 700),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Kept Picture"
        window.isReleasedWhenClosed = false
        window.contentMinSize = NSSize(width: 480, height: 360)
        window.setFrameAutosaveName("EvidenceViewer")
        super.init(window: window)
        let hosting = NSHostingController(rootView: EvidenceViewerView(events: events))
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 960, height: 700))
        window.setFrameUsingName("EvidenceViewer")
        window.delegate = self
        window.onKey = { [weak self] key in self?.handle(key) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// What a key does: arrows step the frames, Esc closes.
    func handle(_ key: EvidenceViewerKey) {
        switch key {
        case .previous: events.stepFrame(by: -1)
        case .next: events.stepFrame(by: 1)
        case .close: close()
        }
    }

    func windowWillClose(_ notification: Notification) { onClose() }
}

struct EvidenceViewerView: View {
    @Bindable var events: SmartAlertEventsModel
    @State private var zoom = TileZoom.none

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                if let ev = events.evidence {
                    MethodToggles(events: events, evidence: ev)
                    if ev.frames.count > 1 {
                        Button { events.stepFrame(by: -1) } label: { Image(systemName: "chevron.left") }
                            .accessibilityLabel("Previous picture")
                        Text("Picture \(events.frame + 1) of \(ev.frames.count)").font(.callout.monospacedDigit())
                        Button { events.stepFrame(by: 1) } label: { Image(systemName: "chevron.right") }
                            .accessibilityLabel("Next picture")
                    }
                }
            }
            .padding(8)
            Divider()
            GeometryReader { geo in
                ZStack {
                    Color.black
                    if let image = events.evidenceImages[events.frame], let ev = events.evidence {
                        let base = ZoomBase(picture: image.size, tile: geo.size, fill: false)
                        let full = Self.pictureFrame(zoom.clamped(in: base), base: base, in: geo.size)
                        EvidenceOverlayView(image: image, evidence: ev, frame: events.frame, visible: events.visibleMethods)
                            .frame(width: full.width, height: full.height)
                            .position(x: full.midX, y: full.midY)
                        EvidenceViewerInput(
                            zoom: { factor, unit in
                                zoom = zoom.zoomed(by: factor, atUnit: unit, in: base)
                            },
                            pan: { delta in zoom = zoom.panned(byUnit: delta, in: base) },
                            reset: { zoom = .none },
                            menu: { events.pictureMenu(window: { NSApp.keyWindow }) })
                    } else {
                        Text("This picture is gone.").foregroundStyle(.white)
                    }
                }
                .clipped()
            }
        }
        .frame(minWidth: 480, minHeight: 360)
    }

    /// The whole picture's rectangle in the view at `zoom`: the destination
    /// `TileZoom` gives is the part of the view covered, `visible` the part of
    /// the picture shown there.
    static func pictureFrame(_ zoom: TileZoom, base: ZoomBase, in size: CGSize) -> CGRect {
        let d = zoom.destination(in: base), v = zoom.visible(in: base)
        guard v.width > 0, v.height > 0 else { return CGRect(origin: .zero, size: size) }
        let w = d.width * size.width / v.width, h = d.height * size.height / v.height
        return CGRect(x: d.minX * size.width - v.minX * w, y: d.minY * size.height - v.minY * h, width: w, height: h)
    }
}

/// Wheel and pinch zoom around the pointer, drag to pan, double-click to
/// reset, and the context menu, over the picture. Units are fractions of
/// the view (origin top left), as `TileZoom` takes them.
struct EvidenceViewerInput: NSViewRepresentable {
    var zoom: (Double, CGPoint) -> Void
    var pan: (CGVector) -> Void
    var reset: () -> Void
    var menu: () -> NSMenu?

    func makeNSView(context: Context) -> InputView { InputView() }

    func updateNSView(_ view: InputView, context: Context) {
        view.zoom = zoom
        view.pan = pan
        view.reset = reset
        view.menuProvider = menu
    }

    final class InputView: NSView {
        var zoom: (Double, CGPoint) -> Void = { _, _ in }
        var pan: (CGVector) -> Void = { _ in }
        var reset: () -> Void = {}
        var menuProvider: () -> NSMenu? = { nil }

        override var isFlipped: Bool { true }

        private func unit(_ event: NSEvent) -> CGPoint {
            let p = convert(event.locationInWindow, from: nil)
            return CGPoint(x: p.x / max(bounds.width, 1), y: p.y / max(bounds.height, 1))
        }

        // A mouse wheel zooms around the pointer; a trackpad's two-finger
        // scroll pans, and zooms with Option held; a pinch zooms (as a tile).
        override func scrollWheel(with event: NSEvent) {
            if event.hasPreciseScrollingDeltas {
                if event.modifierFlags.contains(.option) {
                    zoom(exp(Double(event.scrollingDeltaY) * 0.01), unit(event))
                } else {
                    pan(CGVector(dx: event.scrollingDeltaX / max(bounds.width, 1), dy: event.scrollingDeltaY / max(bounds.height, 1)))
                }
            } else if event.scrollingDeltaY != 0 {
                zoom(pow(1.25, Double(event.scrollingDeltaY)), unit(event))
            }
        }

        override func magnify(with event: NSEvent) { zoom(1 + Double(event.magnification), unit(event)) }

        override func mouseDown(with event: NSEvent) {
            if event.clickCount == 2 { reset() }
        }

        override func mouseDragged(with event: NSEvent) {
            pan(CGVector(dx: event.deltaX / max(bounds.width, 1), dy: event.deltaY / max(bounds.height, 1)))
        }

        override func menu(for event: NSEvent) -> NSMenu? { menuProvider() }
    }
}
