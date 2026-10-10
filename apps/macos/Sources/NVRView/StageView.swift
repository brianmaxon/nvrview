import AppKit
import NVRCore
import QuartzCore
import VideoKit

/// One window's arrangement and what the user is doing with it.
@MainActor
final class WindowState {
    var content: WindowContent
    var name: String
    var tree: LayoutNode
    var selected: NodePath?
    /// The focused tile (also during the animations in and out).
    var focused: NodePath?
    var editing = false
    /// Crop pictures to fill their tiles instead of letterboxing.
    var fill = false

    init(content: WindowContent, name: String, tree: LayoutNode) {
        self.content = content
        self.name = name
        self.tree = tree
    }
}

/// What the stage asks its owner to do.
@MainActor
protocol StageDelegate: AnyObject {
    /// The tree changed by a finished gesture (resize, swap, split, ...).
    func stage(_ stage: StageView, didEdit tree: LayoutNode)
    /// Selection, focus or hover state changed (menus, toolbar).
    func stageDidChangeState(_ stage: StageView)
}

/// A window's live view: the Metal grid drawing every tile of the split
/// tree in one pass, and above it a transparent overlay with each tile's
/// chrome that takes the pointer: select, drag dividers, drag a tile onto
/// another to swap, drop a camera from the sidebar, double-click to focus.
///
/// Focus is an animation of the same live tiles: the focused tile's
/// rectangle grows to the whole stage on every display refresh, nothing is
/// recreated, and only once it is done is the camera switched to its main
/// stream (the sub-stream keeps showing until the main stream's first
/// keyframe has decoded; the core does that). Leaving focus asks for the
/// sub-stream back at once and shrinks the tile while the main stream keeps
/// showing.
@MainActor
final class StageView: NSView {
    static let focusDuration: CFTimeInterval = 0.28

    let model: AppModel
    let state: WindowState
    weak var delegate: StageDelegate?
    let grid: MetalGridView
    private let overlay = StageOverlay()
    private var chromes: [NodePath: TileChromeView] = [:]
    private var frames = LayoutFrames(leaves: [], dividers: [])
    private var lastKey: RenderKey?

    private var focusProgress: Double = 0
    private var focusAnimation: (from: Double, to: Double, start: CFTimeInterval)?
    /// The camera this stage told the leases it focuses.
    private var focusedCamera: CameraRef?

    private enum Gesture {
        case divider(NodePath)
        case tile(from: NodePath, start: CGPoint, dragging: Bool)
    }
    private var gesture: Gesture?
    private var dropTarget: NodePath?
    /// The leaf each of the renderer's tiles draws, in its order.
    private var renderedPaths: [NodePath] = []

    var gap: CGFloat { state.editing ? 6 : 2 }

    init(model: AppModel, state: WindowState, renderer: GridRenderer) {
        self.model = model
        self.state = state
        grid = MetalGridView(renderer: renderer)
        super.init(frame: NSRect(x: 0, y: 0, width: 960, height: 540))
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        grid.autoresizingMask = [.width, .height]
        grid.frame = bounds
        overlay.autoresizingMask = [.width, .height]
        overlay.frame = bounds
        overlay.stage = self
        addSubview(grid)
        addSubview(overlay)
        overlay.registerForDraggedTypes([.string])
        grid.prepare = { [weak self] renderer, size in
            MainActor.assumeIsolated { self?.prepare(renderer: renderer, drawable: size) }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var acceptsFirstResponder: Bool { true }

    // MARK: Layout

    /// The tiles where they are now (focus animation applied).
    private var currentLeaves: [LeafFrame] {
        RectAnimation.focusFrames(
            grid: frames, focused: state.focused, bounds: overlay.bounds,
            progress: state.focused == nil ? 0 : focusProgress)
    }

    /// Recomputes the frames and rebuilds chrome for the current tree.
    func reload() {
        frames = state.tree.frames(in: overlay.bounds, gap: gap)
        if let s = state.selected, state.tree[s]?.isLeaf != true { state.selected = nil }
        if let f = state.focused, state.tree[f]?.isLeaf != true {
            endFocusImmediately()
        }
        let paths = Set(frames.leaves.map(\.path))
        for (path, view) in chromes where !paths.contains(path) {
            view.removeFromSuperview()
            chromes[path] = nil
        }
        for leaf in frames.leaves where chromes[leaf.path] == nil {
            let view = TileChromeView()
            let path = leaf.path
            view.onSpeaker = { [weak self] additive in self?.speakerClicked(path, additive: additive) }
            overlay.addSubview(view)
            chromes[leaf.path] = view
        }
        layoutChrome()
        refreshChrome()
        window?.invalidateCursorRects(for: overlay)
        lastKey = nil
    }

    override func layout() {
        super.layout()
        frames = state.tree.frames(in: overlay.bounds, gap: gap)
        layoutChrome()
        window?.invalidateCursorRects(for: overlay)
    }

    private func layoutChrome() {
        let leaves = currentLeaves
        let shown = Set(leaves.map(\.path))
        for (path, view) in chromes { view.isHidden = !shown.contains(path) }
        for leaf in leaves {
            guard let view = chromes[leaf.path] else { continue }
            if view.frame != leaf.rect { view.frame = leaf.rect }
        }
        // The focused tile's chrome rides on top while it grows.
        if let f = state.focused, let view = chromes[f], overlay.subviews.last !== view {
            overlay.addSubview(view, positioned: .above, relativeTo: nil)
        }
    }

    /// Re-reads names, badges, veils and listening from the model.
    func refreshChrome() {
        for leaf in frames.leaves {
            guard let view = chromes[leaf.path] else { continue }
            view.apply(content(for: leaf))
        }
    }

    private func content(for leaf: LeafFrame) -> TileChromeContent {
        let selected = state.selected == leaf.path && (state.editing || state.focused == nil)
        guard let camera = leaf.camera else {
            return TileChromeContent(
                title: "Empty tile", hasCamera: false, audible: false, selected: selected, editing: state.editing,
                dropTarget: dropTarget == leaf.path)
        }
        let active = model.detections.active(for: camera)
        let multiLens = model.isMultiLens(camera)
        let badges = active.map { a in
            TileChromeContent.Badge(
                text: DetectionTracker.label(a.kind) + (a.source == .local ? " · Mac" : "")
                    + (multiLens ? " · " + AppModel.lensName(camera.lens) : ""),
                color: Self.color(for: a.kind))
        }
        var detail: [String] = [model.deviceName(camera.device)]
        if let tile = model.leases.tile(for: camera) {
            let t = tile.tierState
            if t.requested != t.shown {
                detail.append("switching to \(t.requested.name) stream")
            } else {
                detail.append(t.shown == .main ? "main stream" : "\(t.shown.name) stream")
            }
        }
        detail.append(model.hasCameraAI(camera) ? "detection: camera AI" : "detection: this Mac")
        if let a = active.first { detail[detail.count - 1] = "\(DetectionTracker.label(a.kind)) by \(DetectionTracker.sourceLabel(a.source))" }
        return TileChromeContent(
            title: model.name(of: camera), subtitle: detail.joined(separator: " · "),
            rates: model.showFrameRates ? rateText(leaf.path, camera) : nil, badges: badges,
            veil: model.veil(for: camera), hasCamera: true, audible: model.audible.contains(camera),
            selected: selected, editing: state.editing, dropTarget: dropTarget == leaf.path)
    }

    /// The rate the first tile showing `camera` drew new pictures at.
    func shownRate(of camera: CameraRef) -> Double? {
        guard let path = frames.leaves.first(where: { $0.camera == camera })?.path,
            let i = renderedPaths.firstIndex(of: path)
        else { return nil }
        return grid.renderer.shownRate(ofTile: i)
    }

    /// "Device 15.0 fps · Shown 15.0 fps · 120 Hz": what the camera sends
    /// (camera timestamps), what this tile actually drew, and the display.
    private func rateText(_ path: NodePath, _ camera: CameraRef) -> String {
        let device = model.leases.tile(for: camera)?.deviceRate
        let shown = renderedPaths.firstIndex(of: path).flatMap { grid.renderer.shownRate(ofTile: $0) }
        let hz = window?.screen?.maximumFramesPerSecond ?? 0
        return FrameRateText.make(device: device, shown: shown, displayHz: hz)
    }

    static func color(for kind: String) -> NSColor {
        switch kind {
        case "people", "face": NSColor(calibratedRed: 0.24, green: 0.48, blue: 1, alpha: 1)
        case "vehicle": NSColor(calibratedRed: 0.9, green: 0.54, blue: 0.18, alpha: 1)
        case "dog_cat": NSColor(calibratedRed: 0.13, green: 0.65, blue: 0.56, alpha: 1)
        case "package": NSColor(calibratedRed: 0.62, green: 0.42, blue: 0.85, alpha: 1)
        default: NSColor(calibratedWhite: 0.45, alpha: 0.9)
        }
    }

    // MARK: Drawing (every display refresh)

    private struct RenderKey: Equatable {
        var tiles: [ObjectIdentifier]
        var rects: [CGRect]
        var versions: [UInt64]
        var sizes: [CGSize]
    }

    /// Called by the Metal view on every refresh with the drawable size in
    /// pixels: advances the focus animation, lays out the tiles, feeds the
    /// level meters. Assigns the renderer's tiles only when something changed.
    private func prepare(renderer: GridRenderer, drawable: CGSize) {
        stepAnimation()
        let points = overlay.bounds
        guard points.width > 0, points.height > 0 else { return }
        // Per axis: during a resize the drawable may lag the view (it is
        // stretched to the bounds until its size catches up).
        let scaleX = drawable.width / points.width, scaleY = drawable.height / points.height
        var key = RenderKey(tiles: [], rects: [], versions: [], sizes: [])
        var tiles: [GridTile] = []
        var paths: [NodePath] = []
        for leaf in currentLeaves {
            guard let camera = leaf.camera, let tile = model.leases.tile(for: camera) else { continue }
            paths.append(leaf.path)
            let cell = leaf.rect.pixels(scaleX: scaleX, scaleY: scaleY)
            let snapshot = tile.snapshot
            let picture = tile.latestPicture()
            let size = picture.map { PixelSize(width: UInt32($0.width), height: UInt32($0.height)) }
            let mapping = PictureMapping(snapshot: snapshot, pictureSize: size)
            let pictureSize = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
            let matrix = snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            let gridTile: GridTile
            if mapping.layoutSize == .zero {
                gridTile = GridTile(picture: tile, color: matrix, destination: cell, source: nil)
            } else {
                // Fitted where the picture appears (even scale), then mapped
                // into the possibly stretched drawable, so the picture keeps
                // its shape while the drawable catches up with a resize.
                let stretch = scaleY / scaleX
                let shown = abs(stretch - 1) < 0.001 ? cell : leaf.rect.pixels(scale: scaleX)
                let fit = PictureFit.fit(picture: pictureSize, in: shown, fill: state.fill)
                let destination = shown == cell
                    ? fit.destination
                    : CGRect(
                        x: fit.destination.minX, y: fit.destination.minY * stretch,
                        width: fit.destination.width, height: fit.destination.height * stretch)
                gridTile = GridTile(
                    picture: tile, color: matrix, destination: destination,
                    source: fit.source.offsetBy(dx: CGFloat(mapping.offset.x), dy: CGFloat(mapping.offset.y)))
            }
            tiles.append(gridTile)
            key.tiles.append(ObjectIdentifier(tile))
            key.rects.append(cell)
            key.versions.append(snapshot.version)
            key.sizes.append(pictureSize)
            // Read only for meters on screen (audible, or hovered).
            if let chrome = chromes[leaf.path], chrome.wantsLevel, let level = model.level(camera) {
                chrome.setLevel(level.peak)
            }
        }
        if key != lastKey || renderer.tiles.count != tiles.count {
            lastKey = key
            renderer.tiles = tiles
            renderedPaths = paths
        }
    }

    // MARK: Focus

    var isFocused: Bool { state.focused != nil }

    func toggleFocus(_ path: NodePath? = nil) {
        if state.focused != nil {
            unfocus()
        } else if let p = path ?? state.selected ?? (frames.leaves.count == 1 ? frames.leaves.first?.path : nil),
            state.tree[p]?.isLeaf == true
        {
            focus(p)
        }
    }

    func focus(_ path: NodePath) {
        guard !state.editing, state.tree[path]?.camera != nil || frames.leaves.count > 1 else { return }
        state.selected = path
        state.focused = path
        animateFocus(to: 1)
        refreshChrome()
        delegate?.stageDidChangeState(self)
    }

    func unfocus() {
        guard state.focused != nil else { return }
        // The grid's stream is asked for at once; the main stream keeps
        // showing until it is there, while the tile shrinks back.
        if let camera = focusedCamera {
            model.leases.setFocused(camera, false)
            focusedCamera = nil
        }
        animateFocus(to: 0)
        delegate?.stageDidChangeState(self)
    }

    private func animateFocus(to target: Double) {
        focusAnimation = (focusProgress, target, CACurrentMediaTime())
    }

    private func stepAnimation() {
        guard let a = focusAnimation else { return }
        let t = (CACurrentMediaTime() - a.start) / Self.focusDuration
        focusProgress = a.from + (a.to - a.from) * min(max(t, 0), 1)
        if t >= 1 {
            focusAnimation = nil
            focusProgress = a.to
            if a.to == 1, let f = state.focused, let camera = state.tree[f]?.camera {
                // After the animation: the sharper stream, swapped in by the
                // core after its first keyframe.
                model.leases.setFocused(camera, true)
                focusedCamera = camera
                refreshChrome()
            } else if a.to == 0 {
                state.focused = nil
                refreshChrome()
            }
        }
        layoutChrome()
    }

    private func endFocusImmediately() {
        if let camera = focusedCamera { model.leases.setFocused(camera, false) }
        focusedCamera = nil
        state.focused = nil
        focusAnimation = nil
        focusProgress = 0
    }

    /// Leaves focus without animating (the window closes, the tree changes).
    func tearDown() {
        endFocusImmediately()
        grid.prepare = nil
    }

    // MARK: Editing

    func edit(_ change: (LayoutNode) -> LayoutNode?) {
        guard let tree = change(state.tree) else { return }
        state.tree = tree
        reload()
        delegate?.stage(self, didEdit: tree)
    }

    func split(_ axis: SplitAxis) {
        guard let path = state.selected ?? (frames.leaves.count == 1 ? [] : nil),
            let (tree, new) = state.tree.splitting(at: path, axis: axis)
        else { return }
        state.selected = new
        edit { _ in tree }
    }

    func removeSelected() {
        guard let path = state.selected, let (tree, survivor) = state.tree.removing(at: path) else { return }
        state.selected = tree[survivor]?.isLeaf == true ? survivor : nil
        edit { _ in tree }
    }

    func assign(_ camera: CameraRef?, at path: NodePath) {
        edit { $0.assigning(camera, at: path) }
    }

    private func speakerClicked(_ path: NodePath, additive: Bool) {
        guard let camera = state.tree[path]?.camera else { return }
        model.toggleListening(camera, additive: additive)
    }

    /// The selected tile's camera (Listen menu, keyboard).
    var selectedCamera: CameraRef? {
        if let f = state.focused { return state.tree[f]?.camera }
        if let s = state.selected { return state.tree[s]?.camera }
        return frames.leaves.count == 1 ? frames.leaves.first?.camera : nil
    }

    // MARK: Pointer (from the overlay)

    fileprivate func mouseDown(at p: CGPoint, clickCount: Int) {
        window?.makeFirstResponder(self)
        if state.focused != nil {
            if clickCount == 2 { unfocus() }
            return
        }
        if let d = frames.divider(at: p, minimum: 8) {
            if clickCount == 2 {
                edit { $0.settingRatio(0.5, at: d.path) }
                return
            }
            gesture = .divider(d.path)
            return
        }
        guard let leaf = frames.leaf(at: p) else { return }
        if clickCount == 2, !state.editing {
            focus(leaf.path)
            return
        }
        state.selected = leaf.path
        gesture = .tile(from: leaf.path, start: p, dragging: false)
        refreshChrome()
        delegate?.stageDidChangeState(self)
    }

    fileprivate func mouseDragged(to p: CGPoint) {
        switch gesture {
        case .divider(let path)?:
            guard let d = frames.dividers.first(where: { $0.path == path }),
                let tree = state.tree.settingRatio(LayoutNode.ratio(dragging: d, to: p, gap: gap), at: path)
            else { return }
            state.tree = tree
            frames = tree.frames(in: overlay.bounds, gap: gap)
            layoutChrome()
        case .tile(let from, let start, let dragging)?:
            let moved = dragging || hypot(p.x - start.x, p.y - start.y) > 5
            gesture = .tile(from: from, start: start, dragging: moved)
            guard moved else { return }
            NSCursor.closedHand.set()
            let target = frames.leaf(at: p)?.path
            setDropTarget(target == from ? nil : target)
        case nil:
            break
        }
    }

    fileprivate func mouseUp(at p: CGPoint) {
        defer {
            gesture = nil
            setDropTarget(nil)
            NSCursor.arrow.set()
        }
        switch gesture {
        case .divider?:
            window?.invalidateCursorRects(for: overlay)
            delegate?.stage(self, didEdit: state.tree)
        case .tile(let from, _, true)?:
            guard let to = frames.leaf(at: p)?.path, to != from else { return }
            state.selected = to
            edit { $0.swapping(from, to) }
        default:
            break
        }
    }

    private func setDropTarget(_ path: NodePath?) {
        guard dropTarget != path else { return }
        dropTarget = path
        refreshChrome()
    }

    fileprivate func cursorRects() -> [(CGRect, NSCursor)] {
        guard state.focused == nil else { return [] }
        return frames.dividers.map { d in
            (d.hitRect(minimum: 8), d.axis == .row ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown)
        }
    }

    // MARK: Drag and drop (cameras from the sidebar)

    static let cameraPrefix = "nvrview-camera:"

    /// "nvrview-camera:CHANNEL[.LENS]@DEVICE".
    static func dragString(_ camera: CameraRef) -> String {
        "\(cameraPrefix)\(camera.channel)\(camera.lens == 0 ? "" : ".\(camera.lens)")@\(camera.device)"
    }

    static func camera(fromDrag s: String) -> CameraRef? {
        guard s.hasPrefix(cameraPrefix) else { return nil }
        let rest = s.dropFirst(cameraPrefix.count)
        guard let at = rest.firstIndex(of: "@") else { return nil }
        let numbers = rest[..<at].split(separator: ".", omittingEmptySubsequences: false)
        guard (1...2).contains(numbers.count), let channel = Int(numbers[0]) else { return nil }
        let lens = numbers.count == 2 ? Int(numbers[1]) : 0
        guard let lens, (0...CameraRef.maxLens).contains(lens) else { return nil }
        let device = String(rest[rest.index(after: at)...])
        return device.isEmpty ? nil : CameraRef(device: device, channel: channel, lens: lens)
    }

    fileprivate func dragUpdated(at p: CGPoint, camera: CameraRef?) -> NSDragOperation {
        guard camera != nil, state.focused == nil, let leaf = frames.leaf(at: p) else {
            setDropTarget(nil)
            return []
        }
        setDropTarget(leaf.path)
        return .copy
    }

    fileprivate func performDrop(at p: CGPoint, camera: CameraRef?) -> Bool {
        defer { setDropTarget(nil) }
        guard let camera, state.focused == nil, let leaf = frames.leaf(at: p) else { return false }
        state.selected = leaf.path
        assign(camera, at: leaf.path)
        return true
    }

    fileprivate func dragExited() { setDropTarget(nil) }

    // MARK: Keyboard and menus

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76, 49:  // Return, Enter, Space
            toggleFocus()
        case 53:  // Escape
            if state.focused != nil {
                unfocus()
            } else if state.editing {
                delegate?.stageDidChangeState(self)
                NSApp.sendAction(#selector(ViewerWindowController.toggleEditing(_:)), to: nil, from: self)
            }
        case 51, 117:  // Delete, Forward Delete
            if state.editing { removeSelected() } else { super.keyDown(with: event) }
        default:
            super.keyDown(with: event)
        }
    }

    fileprivate func menu(at p: CGPoint) -> NSMenu? {
        guard state.focused == nil, let leaf = frames.leaf(at: p) else { return nil }
        state.selected = leaf.path
        refreshChrome()
        let menu = NSMenu()
        func item(_ title: String, _ action: @escaping () -> Void) {
            menu.addItem(ClosureMenuItem(title: title, action: action))
        }
        if let camera = leaf.camera {
            item(model.audible.contains(camera) ? "Stop Listening" : "Listen") { [weak self] in
                self?.model.toggleListening(camera, additive: false)
            }
            item("Focus") { [weak self] in self?.focus(leaf.path) }
            menu.addItem(.separator())
        }
        item("Split Side by Side") { [weak self] in self?.split(.row) }
        item("Split Stacked") { [weak self] in self?.split(.col) }
        item("Remove Tile") { [weak self] in self?.removeSelected() }
        if leaf.camera != nil {
            item("Clear Tile") { [weak self] in self?.assign(nil, at: leaf.path) }
        }
        let show = NSMenuItem(title: "Show Camera", action: nil, keyEquivalent: "")
        let cameras = NSMenu()
        for d in model.devices {
            guard !d.channels.isEmpty else { continue }
            cameras.addItem(NSMenuItem.sectionHeader(title: d.config.name))
            for camera in d.channels.flatMap({ model.cameras(device: d.key, channel: $0) }) {
                let i = ClosureMenuItem(title: model.name(of: camera)) { [weak self] in self?.assign(camera, at: leaf.path) }
                i.state = leaf.camera == camera ? .on : .off
                cameras.addItem(i)
            }
        }
        show.submenu = cameras
        show.isEnabled = !cameras.items.isEmpty
        menu.addItem(show)
        return menu
    }
}

/// The transparent view above the Metal surface: holds the tiles' chrome and
/// turns pointer events into stage gestures (top-left origin, like the
/// layout geometry).
@MainActor
private final class StageOverlay: NSView {
    weak var stage: StageView?

    override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    private func point(_ event: NSEvent) -> CGPoint { convert(event.locationInWindow, from: nil) }

    override func mouseDown(with event: NSEvent) { stage?.mouseDown(at: point(event), clickCount: event.clickCount) }
    override func mouseDragged(with event: NSEvent) { stage?.mouseDragged(to: point(event)) }
    override func mouseUp(with event: NSEvent) { stage?.mouseUp(at: point(event)) }
    override func menu(for event: NSEvent) -> NSMenu? { stage?.menu(at: point(event)) }

    override func resetCursorRects() {
        for (rect, cursor) in stage?.cursorRects() ?? [] { addCursorRect(rect, cursor: cursor) }
    }

    private func camera(_ info: any NSDraggingInfo) -> CameraRef? {
        info.draggingPasteboard.string(forType: .string).flatMap(StageView.camera(fromDrag:))
    }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        stage?.dragUpdated(at: convert(sender.draggingLocation, from: nil), camera: camera(sender)) ?? []
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        stage?.dragUpdated(at: convert(sender.draggingLocation, from: nil), camera: camera(sender)) ?? []
    }

    override func draggingExited(_ sender: (any NSDraggingInfo)?) { stage?.dragExited() }

    override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
        stage?.performDrop(at: convert(sender.draggingLocation, from: nil), camera: camera(sender)) ?? false
    }
}

/// A menu item that runs a closure.
final class ClosureMenuItem: NSMenuItem {
    private let run: () -> Void

    init(title: String, action: @escaping () -> Void) {
        run = action
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        target = self
    }

    @available(*, unavailable)
    required init(coder: NSCoder) { fatalError("not used") }

    @objc private func fire() { run() }
}
