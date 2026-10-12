import AppKit
import NVRCore
import SwiftUI

// The live verification view (docs/UX.md "Smart alerts", "Live checks"):
// over a tile (a band across its bottom) or a verification inset, the
// detector's boxes on the live picture and the check's latest steps; and
// the small verification window that never takes focus. The step log's
// words are `TraceText`'s; the event log shows the same.

/// What an overlay shows of one check.
struct VerifyOverlayContent: Equatable {
    var checkID: UInt64
    var headline: String
    /// The latest steps, oldest first.
    var lines: [String]
    var boxes: [VerifyBox]
    /// Other checks on the same camera.
    var others: Int
    var critical: Bool
    /// Offer "Dismiss All" (more than one check is shown).
    var canDismissAll: Bool
}

extension VerifyOverlayContent {
    @MainActor
    init(check c: VerifyCheck, camera: AlertCamera?, model: AppModel, lines count: Int, others: Int) {
        let name: (AlertCamera) -> String = { [model] cam in model.cameraRef(cam).map(model.name(of:)) ?? "Camera \(cam.channel + 1)" }
        checkID = c.id
        var head = c.headline(cameraName: name)
        if others > 0 { head += " · +\(others) more" }
        headline = head
        lines = c.steps.suffix(count).map { TraceText.line($0, cameraName: name) }
        if let e = c.explanation, c.decision != nil, lines.last.map({ !$0.hasPrefix("Delivered") }) ?? true {
            lines = Array((lines + [e]).suffix(count))
        }
        boxes = camera.flatMap { c.pictures[$0]?.boxes } ?? []
        self.others = others
        critical = c.severity == "critical" || c.candidate
        canDismissAll = model.smartAlerts.board.shown.count > 1
    }
}

/// Boxes over a picture and a band of text with the check's steps. Lets
/// clicks through except on its buttons; tracks the pointer (hovering
/// holds the check on screen).
@MainActor
final class VerificationOverlayView: PassThroughView {
    private(set) var content: VerifyOverlayContent?
    /// Where the picture is drawn, in this view's coordinates (top-left
    /// origin).
    var pictureRect: CGRect = .zero {
        didSet { if pictureRect != oldValue { needsDisplay = true } }
    }
    /// The band's height limit as a fraction of the view's height.
    var bandFraction: CGFloat = 0.45
    var onClose: (UInt64) -> Void = { _ in }
    var onDismissAll: () -> Void = {}
    var onHover: (UInt64, Bool) -> Void = { _, _ in }

    private let close = NSButton()
    private let dismissAll = NSButton(title: "Dismiss All", target: nil, action: nil)
    static let lineHeight: CGFloat = 14

    override var isFlipped: Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        close.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "Close Check")
        close.isBordered = false
        close.contentTintColor = .white
        close.target = self
        close.action = #selector(closeClicked)
        close.setAccessibilityLabel("Close Check")
        // Never takes the keyboard from the stage.
        close.refusesFirstResponder = true
        addSubview(close)
        dismissAll.bezelStyle = .inline
        dismissAll.controlSize = .small
        dismissAll.target = self
        dismissAll.action = #selector(dismissAllClicked)
        dismissAll.refusesFirstResponder = true
        addSubview(dismissAll)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func apply(_ c: VerifyOverlayContent?) {
        guard c != content else { return }
        let hadCheck = content?.checkID
        content = c
        isHidden = c == nil
        dismissAll.isHidden = c?.canDismissAll != true
        if hadCheck != c?.checkID, let hadCheck { onHover(hadCheck, false) }
        needsLayout = true
        needsDisplay = true
    }

    /// The band across the bottom: its height for the lines shown.
    var bandRect: CGRect {
        guard let c = content else { return .zero }
        let wanted = CGFloat(c.lines.count + 1) * Self.lineHeight + 10
        let h = min(wanted, max(bounds.height * bandFraction, Self.lineHeight * 2 + 10))
        return CGRect(x: 0, y: bounds.height - h, width: bounds.width, height: h)
    }

    override func layout() {
        super.layout()
        let band = bandRect
        close.frame = CGRect(x: band.maxX - 22, y: band.minY + 3, width: 18, height: 18)
        dismissAll.sizeToFit()
        dismissAll.frame.origin = CGPoint(x: close.frame.minX - dismissAll.frame.width - 6, y: band.minY + 2)
    }

    override func draw(_ dirtyRect: NSRect) {
        guard let c = content else { return }
        // The boxes, labeled (not by color alone).
        for b in c.boxes {
            let r = ROIGeometry.toView(x: b.x, y: b.y, w: b.w, h: b.h, fit: pictureRect)
            let path = NSBezierPath(rect: r)
            let color: NSColor
            switch b.verdict {
            case .hit:
                color = .systemGreen
                path.lineWidth = 2.5
            case .belowFloor:
                color = .systemGray
                path.lineWidth = 1
                path.setLineDash([4, 3], count: 2, phase: 0)
            case .outsideRegion:
                color = .systemOrange
                path.lineWidth = 1.5
                path.setLineDash([1.5, 2.5], count: 2, phase: 0)
            case .otherClass:
                color = .systemGray
                path.lineWidth = 1
            }
            color.setStroke()
            path.stroke()
            let label = NSAttributedString(
                string: b.caption,
                attributes: [
                    .font: NSFont.systemFont(ofSize: 10, weight: .semibold), .foregroundColor: NSColor.white,
                    .backgroundColor: color.withAlphaComponent(0.8),
                ])
            label.draw(at: CGPoint(x: r.minX, y: max(r.minY - 13, pictureRect.minY)))
        }
        // The band.
        let band = bandRect
        NSColor.black.withAlphaComponent(0.72).setFill()
        band.fill()
        (c.critical ? NSColor.systemRed : NSColor.systemTeal).setFill()
        CGRect(x: band.minX, y: band.minY, width: 3, height: band.height).fill()
        let head = NSAttributedString(
            string: c.headline,
            attributes: [.font: NSFont.systemFont(ofSize: 11, weight: .bold), .foregroundColor: NSColor.white])
        let right = (dismissAll.isHidden ? close.frame.minX : dismissAll.frame.minX) - 6
        head.draw(with: CGRect(x: band.minX + 8, y: band.minY + 4, width: max(right - band.minX - 8, 0), height: Self.lineHeight), options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        let rows = max(Int((band.height - 10) / Self.lineHeight) - 1, 0)
        for (i, line) in c.lines.suffix(rows).enumerated() {
            NSAttributedString(
                string: line,
                attributes: [.font: NSFont.systemFont(ofSize: 10.5), .foregroundColor: NSColor.white.withAlphaComponent(0.9)]
            ).draw(
                with: CGRect(
                    x: band.minX + 8, y: band.minY + 4 + CGFloat(i + 1) * Self.lineHeight, width: band.width - 16,
                    height: Self.lineHeight), options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        for t in trackingAreas { removeTrackingArea(t) }
        addTrackingArea(NSTrackingArea(rect: bandRect, options: [.mouseEnteredAndExited, .activeAlways], owner: self))
    }

    override func mouseEntered(with event: NSEvent) { if let id = content?.checkID { onHover(id, true) } }
    override func mouseExited(with event: NSEvent) { if let id = content?.checkID { onHover(id, false) } }

    @objc private func closeClicked() { if let id = content?.checkID { onClose(id) } }
    @objc private func dismissAllClicked() { onDismissAll() }

    /// The self-test's click on the close button.
    func clickCloseForTest() { closeClicked() }
}

// MARK: The verification window

/// A small window of live checks that never takes focus (shown when a
/// check's camera is on no viewer window and the setting asks for a
/// window, or always with "Always in a small window").
@MainActor
final class VerificationPanelController: NSWindowController {
    private let model: AppModel

    init(model: AppModel) {
        self.model = model
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 380, height: 460),
            styleMask: [.titled, .closable, .resizable, .utilityWindow, .nonactivatingPanel], backing: .buffered,
            defer: false)
        panel.title = "Smart Alert Checks"
        panel.isReleasedWhenClosed = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.setFrameAutosaveName("SmartAlertChecks")
        panel.contentViewController = NSHostingController(rootView: VerificationPanelView(model: model))
        super.init(window: panel)
        if panel.frame.origin == .zero, let screen = NSScreen.main {
            panel.setFrameTopLeftPoint(CGPoint(x: screen.visibleFrame.maxX - 400, y: screen.visibleFrame.maxY - 20))
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// Shows it without activating the app or taking the keyboard.
    func present() {
        guard let window, !window.isVisible else { return }
        window.orderFrontRegardless()
    }

    func hide() { window?.orderOut(nil) }
}

struct VerificationPanelView: View {
    let model: AppModel
    @State private var tick = 0

    private var alerts: SmartAlertsController { model.smartAlerts }

    var body: some View {
        let checks = alerts.panelChecks
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(checks.isEmpty ? "No checks" : "\(checks.count) check\(checks.count == 1 ? "" : "s")").font(.headline)
                Spacer()
                Button("Dismiss All") { alerts.dismissAll() }.disabled(checks.isEmpty)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(checks.prefix(VerificationBoard.maxShown)) { c in
                        VerificationCard(model: model, check: c)
                    }
                    if checks.count > VerificationBoard.maxShown {
                        Text("+\(checks.count - VerificationBoard.maxShown) more").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .padding(10)
        .frame(minWidth: 320, minHeight: 240)
        .onExitCommand { if let top = checks.first { alerts.dismiss(top.id) } }
    }
}

/// One check in the window: its picture with boxes (the picture the
/// detector checked, when it handed one over) and its whole step log.
struct VerificationCard: View {
    let model: AppModel
    let check: VerifyCheck

    var body: some View {
        let name: (AlertCamera) -> String = { [model] cam in model.cameraRef(cam).map(model.name(of:)) ?? "Camera \(cam.channel + 1)" }
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(check.headline(cameraName: name)).font(.subheadline.bold())
                Spacer()
                Button {
                    model.smartAlerts.dismiss(check.id)
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Close Check")
            }
            ForEach(check.cameras, id: \.self) { cam in
                BoxedPicture(image: model.smartAlerts.pictures[check.id]?[cam], boxes: check.pictures[cam]?.boxes ?? [])
                    .frame(height: 150)
                    .overlay(alignment: .topLeading) {
                        Text(name(cam)).font(.caption2).padding(3).background(.black.opacity(0.6)).foregroundStyle(.white)
                    }
            }
            TraceLogView(steps: check.steps, cameraName: name)
            if let e = check.explanation { Text(e).font(.caption).textSelection(.enabled) }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.secondary.opacity(0.1)))
        .onHover { model.smartAlerts.hover(check.id, $0) }
    }
}

/// A picture (or a gray stand-in) with judged boxes over it.
struct BoxedPicture: View {
    let image: CGImage?
    let boxes: [VerifyBox]

    var body: some View {
        GeometryReader { geo in
            let size = image.map { CGSize(width: $0.width, height: $0.height) } ?? CGSize(width: 16, height: 9)
            let fit = ROIGeometry.fitRect(picture: size, in: geo.size)
            ZStack(alignment: .topLeading) {
                Color.black.opacity(0.85)
                if let image {
                    Image(decorative: image, scale: 1).resizable().frame(width: fit.width, height: fit.height)
                        .offset(x: fit.minX, y: fit.minY)
                }
                Canvas { ctx, _ in
                    for b in boxes {
                        let r = ROIGeometry.toView(x: b.x, y: b.y, w: b.w, h: b.h, fit: fit)
                        let style: StrokeStyle =
                            switch b.verdict {
                            case .hit: StrokeStyle(lineWidth: 2.5)
                            case .belowFloor: StrokeStyle(lineWidth: 1, dash: [4, 3])
                            case .outsideRegion: StrokeStyle(lineWidth: 1.5, dash: [1.5, 2.5])
                            case .otherClass: StrokeStyle(lineWidth: 1)
                            }
                        let color: Color = b.verdict == .hit ? .green : b.verdict == .outsideRegion ? .orange : .gray
                        ctx.stroke(Path(r), with: .color(color), style: style)
                        ctx.draw(
                            Text(b.caption).font(.system(size: 9, weight: .semibold)).foregroundColor(.white),
                            at: CGPoint(x: r.minX + 2, y: max(r.minY - 7, fit.minY + 6)), anchor: .leading)
                    }
                }
            }
        }
    }
}

/// The step log: one line per trace step, in order.
struct TraceLogView: View {
    let steps: [TraceStep]
    let cameraName: (AlertCamera) -> String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(steps.enumerated()), id: \.offset) { _, s in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(TraceText.clock(s.atMs)).font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
                    Text(TraceText.line(s, cameraName: cameraName)).font(.caption).textSelection(.enabled)
                }
            }
        }
    }
}
