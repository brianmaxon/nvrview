import AppKit
import NVRCore

// The camera motion controls (docs/UX.md "Pan, tilt, and zoom"): a pad of
// four directions with a speed slider and, when the camera reports lens
// ranges, zoom and focus sliders below it; a popover of the lens controls
// alone; and the presets menu (recall only). Every control is a thin
// shell over `PTZModel`; the self-test drives the same entry points the
// pointer does.

/// A control that is held: down on mouse down, up on mouse up (also when the
/// pointer has left it), or when it goes away with its popover.
@MainActor
final class HoldButton: NSButton {
    var onDown: () -> Void = {}
    var onUp: () -> Void = {}
    private(set) var isDown = false

    init(symbol: String, label: String) {
        super.init(frame: .zero)
        image = NSImage(systemSymbolName: symbol, accessibilityDescription: label)
        imagePosition = .imageOnly
        bezelStyle = .regularSquare
        setAccessibilityLabel(label)
        toolTip = label
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// Presses the control, as a mouse down does.
    func pressDown() {
        guard !isDown else { return }
        isDown = true
        highlight(true)
        onDown()
    }

    /// Lets go of it, as a mouse up does.
    func pressUp() {
        guard isDown else { return }
        isDown = false
        highlight(false)
        onUp()
    }

    override func mouseDown(with event: NSEvent) { pressDown() }
    override func mouseUp(with event: NSEvent) { pressUp() }

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        if newWindow == nil { pressUp() }
        super.viewWillMove(toWindow: newWindow)
    }
}

/// A lens axis: label, a step down, a slider across the camera's range,
/// and a step up.
@MainActor
final class PTZLensRow: NSView {
    let axis: PTZLensAxis
    let minus = NSButton()
    let plus = NSButton()
    let slider = NSSlider()
    private let label: NSTextField
    var onStep: (Int) -> Void = { _ in }
    var onDrag: (Int) -> Void = { _ in }
    var onEnd: () -> Void = {}

    init(axis: PTZLensAxis) {
        self.axis = axis
        label = NSTextField(labelWithString: axis == .zoom ? "Zoom" : "Focus")
        super.init(frame: .zero)
        label.font = .systemFont(ofSize: 11)
        for (b, symbol, name, step) in [(minus, "minus", "out", -1), (plus, "plus", "in", 1)] {
            b.image = NSImage(systemSymbolName: symbol, accessibilityDescription: "\(label.stringValue) \(name)")
            b.imagePosition = .imageOnly
            b.bezelStyle = .regularSquare
            b.target = self
            b.tag = step
            b.action = #selector(stepped(_:))
            b.setAccessibilityLabel(axis == .zoom ? "Zoom \(name)" : "Focus \(step < 0 ? "nearer" : "farther")")
        }
        slider.target = self
        slider.action = #selector(moved(_:))
        slider.isContinuous = true
        slider.setAccessibilityLabel(label.stringValue)
        for v in [label, minus, plus, slider] { addSubview(v) }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override func layout() {
        super.layout()
        label.frame = CGRect(x: 0, y: 4, width: 44, height: 16)
        minus.frame = CGRect(x: 46, y: 0, width: 24, height: 24)
        plus.frame = CGRect(x: bounds.width - 24, y: 0, width: 24, height: 24)
        slider.frame = CGRect(x: 74, y: 2, width: max(bounds.width - 74 - 28, 20), height: 20)
    }

    func update(range: PTZRange, position: Int) {
        slider.minValue = Double(range.min)
        slider.maxValue = Double(max(range.max, range.min + 1))
        slider.integerValue = range.clamped(position)
    }

    @objc private func stepped(_ sender: NSButton) { onStep(sender.tag) }

    @objc private func moved(_ sender: NSSlider) {
        onDrag(sender.integerValue)
        if NSApp.currentEvent?.type == .leftMouseUp { onEnd() }
    }

    /// The slider dragged to `position` (and let go, when `ended`), as the
    /// pointer does.
    func drag(to position: Int, ended: Bool) {
        slider.integerValue = position
        onDrag(slider.integerValue)
        if ended { onEnd() }
    }
}

/// The popover's content: the pad (`.pad`) or the lens sliders (`.lens`) for
/// one camera, with the camera's name and the latest refusal or failure.
@MainActor
final class PTZPopoverController: NSViewController {
    enum Mode { case pad, lens }

    let camera: CameraRef
    let mode: Mode
    private unowned let ptz: PTZModel
    private let name: String
    private(set) var pad: [PTZDirection: HoldButton] = [:]
    private(set) var lensRows: [PTZLensAxis: PTZLensRow] = [:]
    private(set) var speedSlider: NSSlider?
    private let status = NSTextField(wrappingLabelWithString: "")
    private let heading: NSTextField
    private let speedLabel = NSTextField(labelWithString: "")
    private var token: Int?
    static let width: CGFloat = 240

    init(camera: CameraRef, mode: Mode, name: String, ptz: PTZModel) {
        self.camera = camera
        self.mode = mode
        self.ptz = ptz
        self.name = name
        heading = NSTextField(labelWithString: name)
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override func loadView() {
        view = FlippedView(frame: CGRect(x: 0, y: 0, width: Self.width, height: 160))
        heading.font = .systemFont(ofSize: 12, weight: .semibold)
        status.font = .systemFont(ofSize: 11)
        status.textColor = .systemRed
        token = ptz.addObserver { [weak self] _ in self?.refresh() }
        ptz.clearProblem(camera)
        build()
        ptz.withInfo(camera) { [weak self] _ in self?.refresh() }
    }

    deinit {
        MainActor.assumeIsolated {
            if let token { ptz.removeObserver(token) }
        }
    }

    /// The popover goes away: nothing stays held.
    func closed() {
        for b in pad.values { b.pressUp() }
        ptz.release()
    }

    /// Lays the controls out for what the camera can do.
    private func build() {
        for v in view.subviews { v.removeFromSuperview() }
        pad = [:]
        lensRows = [:]
        speedSlider = nil
        let w = Self.width
        var y: CGFloat = 10
        heading.frame = CGRect(x: 12, y: y, width: w - 24, height: 16)
        view.addSubview(heading)
        y += 24
        guard let info = ptz.info(for: camera) else {
            let reading = NSTextField(labelWithString: "Reading the camera…")
            reading.font = .systemFont(ofSize: 11)
            reading.frame = CGRect(x: 12, y: y, width: w - 24, height: 16)
            view.addSubview(reading)
            y += 24
            finish(y)
            return
        }
        if mode == .pad {
            if info.panTilt {
                let cell = CGSize(width: 44, height: 36)
                let left = (w - cell.width * 3) / 2
                func place(_ d: PTZDirection, _ symbol: String, _ label: String, col: Int, row: Int) {
                    let b = HoldButton(symbol: symbol, label: label)
                    b.frame = CGRect(
                        x: left + CGFloat(col) * cell.width, y: y + CGFloat(row) * cell.height,
                        width: cell.width - 4, height: cell.height - 4)
                    b.onDown = { [weak self] in
                        guard let self else { return }
                        ptz.press(d, camera: camera)
                    }
                    b.onUp = { [weak self] in self?.ptz.release() }
                    view.addSubview(b)
                    pad[d] = b
                }
                place(.up, "chevron.up", "Tilt up", col: 1, row: 0)
                place(.left, "chevron.left", "Pan left", col: 0, row: 1)
                place(.right, "chevron.right", "Pan right", col: 2, row: 1)
                place(.down, "chevron.down", "Tilt down", col: 1, row: 2)
                let stop = NSButton(
                    image: NSImage(systemSymbolName: "stop.fill", accessibilityDescription: "Stop") ?? NSImage(),
                    target: self, action: #selector(stopPressed))
                stop.bezelStyle = .regularSquare
                stop.frame = CGRect(x: left + cell.width, y: y + cell.height, width: cell.width - 4, height: cell.height - 4)
                stop.setAccessibilityLabel("Stop")
                view.addSubview(stop)
                y += cell.height * 3 + 8
                let speed = NSSlider(
                    value: Double(ptz.speed), minValue: Double(info.minSpeed), maxValue: Double(info.maxSpeed),
                    target: self, action: #selector(speedChanged(_:)))
                speed.setAccessibilityLabel("Speed")
                speed.frame = CGRect(x: 60, y: y, width: w - 72, height: 20)
                speedLabel.font = .systemFont(ofSize: 11)
                speedLabel.frame = CGRect(x: 12, y: y + 2, width: 46, height: 16)
                speedLabel.stringValue = "Speed \(ptz.speed)"
                view.addSubview(speed)
                view.addSubview(speedLabel)
                speedSlider = speed
                y += 30
            } else {
                let note = NSTextField(labelWithString: "This camera does not pan or tilt")
                note.font = .systemFont(ofSize: 11)
                note.frame = CGRect(x: 12, y: y, width: w - 24, height: 16)
                view.addSubview(note)
                y += 24
            }
        }
        for axis in PTZLensAxis.allCases {
            guard let range = info.range(axis) else { continue }
            let row = PTZLensRow(axis: axis)
            row.frame = CGRect(x: 12, y: y, width: w - 24, height: 24)
            row.update(range: range, position: ptz.lensPosition(camera, axis) ?? range.cur)
            row.onStep = { [weak self] direction in
                guard let self else { return }
                ptz.stepLens(axis, direction, camera: camera)
            }
            row.onDrag = { [weak self] p in
                guard let self else { return }
                ptz.dragLens(axis, to: p, camera: camera)
            }
            row.onEnd = { [weak self] in
                guard let self else { return }
                ptz.endLensDrag(axis, camera: camera)
            }
            view.addSubview(row)
            lensRows[axis] = row
            y += 30
        }
        if mode == .lens, lensRows.isEmpty {
            let note = NSTextField(labelWithString: "This camera has no zoom or focus")
            note.font = .systemFont(ofSize: 11)
            note.frame = CGRect(x: 12, y: y, width: w - 24, height: 16)
            view.addSubview(note)
            y += 24
        }
        finish(y)
    }

    private func finish(_ top: CGFloat) {
        var y = top
        let text = ptz.problems[camera] ?? ""
        status.stringValue = text
        let height: CGFloat = text.isEmpty ? 0 : 30
        status.frame = CGRect(x: 12, y: y, width: Self.width - 24, height: height)
        view.addSubview(status)
        y += height + 10
        preferredContentSize = CGSize(width: Self.width, height: y)
        view.frame.size = preferredContentSize
    }

    /// The camera's state changed (capabilities read, a position, a refusal).
    func refresh() {
        let haveRows = !lensRows.isEmpty || !pad.isEmpty
        if ptz.info(for: camera) != nil, !haveRows || shapeChanged() {
            build()
            return
        }
        if ptz.info(for: camera) == nil, view.subviews.count < 2 { build() }
        for (axis, row) in lensRows {
            if let range = ptz.info(for: camera)?.range(axis), let p = ptz.lensPosition(camera, axis) {
                row.update(range: range, position: p)
            }
        }
        let text = ptz.problems[camera] ?? ""
        if status.stringValue != text {
            status.stringValue = text
            build()
        }
    }

    private func shapeChanged() -> Bool {
        guard let info = ptz.info(for: camera) else { return false }
        let wantsPad = mode == .pad && info.panTilt
        return wantsPad != !pad.isEmpty
            || Set(PTZLensAxis.allCases.filter { info.range($0) != nil }) != Set(lensRows.keys)
    }

    @objc private func stopPressed() { ptz.release() }

    @objc private func speedChanged(_ sender: NSSlider) {
        ptz.speed = sender.integerValue
        speedLabel.stringValue = "Speed \(ptz.speed)"
    }

    /// The speed slider moved to `value`, as the pointer does.
    func setSpeed(_ value: Int) {
        guard let slider = speedSlider else { return }
        slider.integerValue = value
        speedChanged(slider)
    }

    /// The line the popover shows for a refusal or failure (self-test).
    var statusText: String { status.stringValue }
}


/// The presets menu of a camera: its list in the camera's order, the first
/// nine with their Option+digit hint; a choice recalls it. Recall only:
/// saving or deleting presets is a device setting and is not offered.
@MainActor
func presetsMenu(info: PTZInfo, camera: CameraRef, ptz: PTZModel) -> NSMenu {
    let menu = NSMenu(title: "Presets")
    menu.autoenablesItems = false
    for item in presetItems(info: info, camera: camera, ptz: ptz) { menu.addItem(item) }
    return menu
}

/// The items of a presets menu: one per preset, or a disabled "No presets".
@MainActor
func presetItems(info: PTZInfo, camera: CameraRef, ptz: PTZModel) -> [NSMenuItem] {
    if info.presets.isEmpty {
        let none = NSMenuItem(title: "No presets", action: nil, keyEquivalent: "")
        none.isEnabled = false
        return [none]
    }
    return info.presets.enumerated().map { i, preset in
        let title = preset.name.isEmpty ? "Preset \(i + 1)" : preset.name
        let item = ClosureMenuItem(title: title) { [weak ptz] in ptz?.recall(preset.id, camera: camera) }
        if i < 9 {
            item.keyEquivalent = String(i + 1)
            item.keyEquivalentModifierMask = [.option]
        }
        return item
    }
}
