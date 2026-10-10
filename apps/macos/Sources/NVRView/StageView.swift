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
    /// Digital zoom per camera, in picture coordinates (docs/UX.md "Digital
    /// zoom"). Keyed by camera so it follows the picture through layout
    /// changes, swaps, focus and the switch to the main stream; tiles of
    /// the same camera in one window share it.
    var zoom: [CameraRef: TileZoom] = [:]

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

    /// Wired by the alert work (docs/UX.md "Alerts"): whether a camera has
    /// an active alert (its tile's header then stays shown, also in full
    /// screen while the other chrome hides), and how to dismiss it (Return
    /// on the selected tile).
    var alertActive: (CameraRef) -> Bool = { _ in false }
    var dismissAlert: (CameraRef) -> Void = { _ in }

    /// Whether the window is in full screen: there the chrome, including
    /// the pointer, hides after a few idle seconds (docs/UX.md "Full screen
    /// chrome"). Set by the window controller.
    var fullScreen = false {
        didSet {
            guard fullScreen != oldValue else { return }
            noteInteraction()
            if !fullScreen, hidesPointer { NSCursor.setHiddenUntilMouseMoves(false) }
        }
    }
    /// Whether hiding the chrome in full screen also hides the pointer (the
    /// self-test turns it off: it never touches the user's pointer).
    var hidesPointer = true
    /// When the chrome hides; seconds of no interaction.
    private(set) var idle = ChromeIdle(now: CACurrentMediaTime())
    /// The overlay chrome is shown (not hidden for idleness).
    var chromeVisible: Bool { idle.visible }
    private var insets: [NodePath: ZoomInsetView] = [:]
    /// Mouse-wheel zoom easing toward a target scale, per camera, around a
    /// point of the shown picture.
    private var zoomTargets: [CameraRef: (scale: Double, unit: CGPoint, frame: CGRect)] = [:]
    private var lastPrepare: CFTimeInterval?
    /// The picture's layout size per camera, from the last refresh.
    private var pictureSizes: [CameraRef: CGSize] = [:]

    private var focusProgress: Double = 0
    private var focusAnimation: (from: Double, to: Double, start: CFTimeInterval)?
    /// The camera this stage told the leases it focuses.
    private var focusedCamera: CameraRef?
    /// A tile of this stage as the leases know it (stream quality demands).
    private struct DemandOwner: Hashable {
        let stage: ObjectIdentifier
        let path: NodePath
    }
    private var demandOwners: Set<DemandOwner> = []

    private enum Gesture {
        case divider(NodePath)
        case tile(from: NodePath, start: CGPoint, dragging: Bool)
        /// A drag in a zoomed tile pans its picture.
        case pan(CameraRef, last: CGPoint)
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
        for (path, view) in insets where !paths.contains(path) {
            view.removeFromSuperview()
            insets[path] = nil
        }
        for leaf in frames.leaves where chromes[leaf.path] == nil {
            let view = TileChromeView()
            let path = leaf.path
            view.onSpeaker = { [weak self] additive in self?.speakerClicked(path, additive: additive) }
            view.onResetZoom = { [weak self] in self?.resetZoom(at: path) }
            overlay.addSubview(view)
            chromes[leaf.path] = view
            let inset = ZoomInsetView()
            overlay.addSubview(inset)
            insets[leaf.path] = inset
        }
        layoutChrome()
        refreshChrome()
        syncDemands()
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
        layoutInsets(leaves)
        // The focused tile's chrome rides on top while it grows.
        if let f = state.focused, let view = chromes[f], overlay.subviews.last !== (insets[f] ?? view) {
            overlay.addSubview(view, positioned: .above, relativeTo: nil)
            if let inset = insets[f] { overlay.addSubview(inset, positioned: .above, relativeTo: nil) }
        }
    }

    /// Places and updates the zoom maps: shown while the tile is zoomed and
    /// the chrome is up.
    private func layoutInsets(_ leaves: [LeafFrame]) {
        let shown = Set(leaves.map(\.path))
        for (path, inset) in insets where !shown.contains(path) { inset.setShown(false) }
        for leaf in leaves {
            guard let inset = insets[leaf.path] else { continue }
            guard let camera = leaf.camera, let z = state.zoom[camera], z.isZoomed,
                let picture = pictureSizes[camera], picture.width > 0, picture.height > 0
            else {
                inset.setShown(false)
                continue
            }
            let pictureRect = TilePlacement.shownRect(rect: leaf.rect, picture: picture, fill: state.fill)
            let frame = ZoomInsetView.frame(inPicture: pictureRect, aspect: picture.width / picture.height)
            if inset.frame != frame { inset.frame = frame }
            inset.update(region: z.visible(in: zoomFrame(leaf.rect, picture)))
            inset.setShown(chromeVisible)
        }
    }

    /// Re-reads names, badges, veils and listening from the model.
    func refreshChrome() {
        for leaf in frames.leaves {
            guard let view = chromes[leaf.path] else { continue }
            view.apply(content(for: leaf))
            view.setZoomed(leaf.camera.flatMap { state.zoom[$0] }?.isZoomed == true)
            view.setHeaderPinned(leaf.camera.map(alertActive) == true)
            view.setOutlineShown(chromeVisible)
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
        let quality = model.leases.effectiveQuality(state.tree[leaf.path]?.quality ?? .auto, camera)
        if quality != .auto { detail.insert(quality.title, at: 1) }
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
        // The switch to the focused tile's stream, for a few seconds.
        let note = state.focused == path ? model.leases.tile(for: camera).flatMap { model.switchNote(forTile: $0.id) } : nil
        return FrameRateText.make(device: device, shown: shown, displayHz: hz, switchNote: note)
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
        var zooms: [TileZoom]
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
        stepZoom()
        var key = RenderKey(tiles: [], rects: [], versions: [], sizes: [], zooms: [])
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
            // Zoom changes only the source rectangle sampled (TilePlacement).
            let zoom = state.zoom[camera] ?? .none
            if pictureSize != .zero, pictureSizes[camera] != pictureSize { pictureSizes[camera] = pictureSize }
            let placed = TilePlacement.place(
                rect: leaf.rect, scaleX: scaleX, scaleY: scaleY, picture: pictureSize,
                offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)), fill: state.fill,
                zoom: zoom)
            tiles.append(GridTile(picture: tile, color: matrix, destination: placed.destination, source: placed.source))
            key.tiles.append(ObjectIdentifier(tile))
            key.rects.append(cell)
            key.versions.append(snapshot.version)
            key.sizes.append(pictureSize)
            key.zooms.append(zoom)
            // Read only for meters on screen (audible, or hovered).
            if let chrome = chromes[leaf.path], chrome.wantsLevel, let level = model.level(camera) {
                chrome.setLevel(level.peak)
            }
        }
        if key != lastKey || renderer.tiles.count != tiles.count {
            if key.zooms != lastKey?.zooms { layoutInsets(currentLeaves) }
            lastKey = key
            renderer.tiles = tiles
            renderedPaths = paths
        }
    }

    /// The source rectangles the renderer samples now, by leaf (tests and
    /// the self-test check them against the zoom).
    func renderedSource(of path: NodePath) -> CGRect? {
        guard let i = renderedPaths.firstIndex(of: path), i < grid.renderer.tiles.count else { return nil }
        return grid.renderer.tiles[i].source
    }

    // MARK: Focus

    var isFocused: Bool { state.focused != nil }

    /// A pointer gesture (divider drag, tile drag, pan) is in progress: the
    /// tree on screen may differ from the stored layout until it ends.
    var isInteracting: Bool { gesture != nil }
    /// A focus animation is running (self-test).
    var isAnimatingFocus: Bool { focusAnimation != nil }
    /// A mouse-wheel zoom is still easing (self-test).
    var isEasingZoom: Bool { !zoomTargets.isEmpty }

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
        if focusedCamera != nil {
            focusedCamera = nil
            syncDemands()
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
                focusedCamera = camera
                syncDemands()
                refreshChrome()
            } else if a.to == 0 {
                state.focused = nil
                refreshChrome()
            }
        }
        layoutChrome()
    }

    private func endFocusImmediately() {
        focusedCamera = nil
        state.focused = nil
        focusAnimation = nil
        focusProgress = 0
    }

    /// In focus, an arrow key: the focused tile becomes `path`'s, at once,
    /// on the main stream (the new camera's main stream is asked for before
    /// the old one is let go, so a camera in both keeps it).
    private func switchFocus(to path: NodePath) {
        state.focused = path
        state.selected = path
        focusedCamera = state.tree[path]?.camera
        syncDemands()
        layoutChrome()
        refreshChrome()
        delegate?.stageDidChangeState(self)
    }

    // MARK: Keyboard shortcuts (docs/UX.md "Keyboard")

    /// Moves the selection to the tile in `direction` by position; in focus,
    /// focuses that tile instead.
    func moveSelection(_ direction: MoveDirection) {
        if let f = state.focused {
            guard focusAnimation == nil, let n = frames.neighbor(of: f, direction) else { return }
            switchFocus(to: n)
            return
        }
        if let s = state.selected, frames.leaf(s) != nil {
            guard let n = frames.neighbor(of: s, direction) else { return }
            state.selected = n
        } else {
            state.selected = frames.leaves.first?.path
        }
        noteInteraction()
        refreshChrome()
        delegate?.stageDidChangeState(self)
    }

    /// Return: dismisses the selected tile's alert if it has one, else
    /// toggles focus.
    func confirm() {
        if let camera = selectedCamera, alertActive(camera) {
            dismissAlert(camera)
            refreshChrome()
        } else {
            toggleFocus()
        }
    }

    // MARK: Chrome auto-hide (docs/UX.md "Full screen chrome")

    /// A pointer move, click, key or scroll in the window.
    func noteInteraction() {
        guard idle.interact(now: CACurrentMediaTime()) else { return }
        // Back: the outline and maps fade in; in full screen, the header of
        // the tile under the pointer too (moving the pointer also does that).
        refreshChrome()
        layoutInsets(currentLeaves)
        if fullScreen, let p = pointerInOverlay, let leaf = currentLeaves.last(where: { $0.rect.contains(p) }) {
            chromes[leaf.path]?.showChrome()
        }
    }

    /// Called now and then by the window: hides the chrome once idle. Out
    /// of full screen the headers keep their own hover timing; the outline
    /// and zoom map fade either way.
    func tickChrome() {
        guard idle.tick(now: CACurrentMediaTime()) else { return }
        hideChrome()
    }

    /// The seconds of idleness before the chrome hides (the self-test
    /// shortens it).
    var chromeHideDelay: Double {
        get { idle.delay }
        set { idle.delay = newValue }
    }

    private func hideChrome() {
        for view in chromes.values {
            view.setOutlineShown(false)
            // Headers of tiles with an active alert are pinned and stay.
            if fullScreen { view.fadeOut() }
        }
        for inset in insets.values { inset.setShown(false) }
        if fullScreen, hidesPointer { NSCursor.setHiddenUntilMouseMoves(true) }
    }

    /// Shows a tile's header as hovering does (self-test).
    func showHeader(_ path: NodePath) { chromes[path]?.showChrome() }

    /// Whether a tile's header is shown (self-test).
    func headerShown(_ path: NodePath) -> Bool? { chromes[path]?.isHeaderShown }

    /// Whether a tile's zoom map is shown (self-test).
    func zoomMapShown(_ path: NodePath) -> Bool? { insets[path]?.isShown }

    private var pointerInOverlay: CGPoint? {
        guard let window else { return nil }
        let p = overlay.convert(window.mouseLocationOutsideOfEventStream, from: nil)
        return overlay.bounds.contains(p) ? p : nil
    }

    // MARK: Digital zoom (docs/UX.md "Digital zoom")

    /// The part of the picture a tile shows at 1x, as a fraction of it.
    private func zoomFrame(_ rect: CGRect, _ picture: CGSize) -> CGRect {
        TileZoom.normalized(PictureFit.fit(picture: picture, in: rect, fill: state.fill).source, picture: picture)
    }

    /// The tile under the pointer as drawn now (the focused one in focus).
    private func drawnLeaf(at p: CGPoint) -> LeafFrame? {
        currentLeaves.last { $0.rect.contains(p) }
    }

    /// For the tile at `p`: its camera, the unit point of `p` in its shown
    /// picture, the shown picture's size in points, and its 1x frame.
    private func zoomTarget(at p: CGPoint) -> (camera: CameraRef, unit: CGPoint, shown: CGSize, frame: CGRect)? {
        guard let leaf = drawnLeaf(at: p), let camera = leaf.camera, let picture = pictureSizes[camera] else {
            return nil
        }
        let shown = TilePlacement.shownRect(rect: leaf.rect, picture: picture, fill: state.fill)
        return (camera, TilePlacement.unit(p, in: shown), shown.size, zoomFrame(leaf.rect, picture))
    }

    private func setZoom(_ z: TileZoom, for camera: CameraRef) {
        let was = state.zoom[camera]?.isZoomed == true
        state.zoom[camera] = z.isZoomed ? z : nil
        if was != z.isZoomed { refreshChrome() }
        syncDemands()
    }

    /// The zoom a tile's camera has (1x if none).
    func zoom(of camera: CameraRef) -> TileZoom { state.zoom[camera] ?? .none }

    /// Zooms the tile at `p` by `factor` around `p`: at once (pinch), or
    /// eased over the next refreshes (a mouse-wheel notch).
    func zoom(at p: CGPoint, by factor: Double, eased: Bool) {
        guard let t = zoomTarget(at: p) else { return }
        noteInteraction()
        if eased {
            let from = zoomTargets[t.camera]?.scale ?? zoom(of: t.camera).scale
            let target = min(max(from * factor, 1), TileZoom.maxScale)
            zoomTargets[t.camera] = (target, t.unit, t.frame)
        } else {
            zoomTargets[t.camera] = nil
            setZoom(zoom(of: t.camera).zoomed(by: factor, atUnit: t.unit, in: t.frame), for: t.camera)
        }
    }

    /// Pans the tile at `p` by `delta` points (the picture follows).
    func pan(at p: CGPoint, by delta: CGVector) {
        guard let t = zoomTarget(at: p), zoom(of: t.camera).isZoomed, t.shown.width > 0, t.shown.height > 0 else {
            return
        }
        zoomTargets[t.camera] = nil
        let unit = CGVector(dx: delta.dx / t.shown.width, dy: delta.dy / t.shown.height)
        setZoom(zoom(of: t.camera).panned(byUnit: unit, in: t.frame), for: t.camera)
    }

    /// Back to 1x, eased, for the tile at `path` (its reset control).
    func resetZoom(at path: NodePath) {
        guard let leaf = frames.leaf(path), let camera = leaf.camera else { return }
        resetZoom(camera)
    }

    /// 0: back to 1x on the selected (or focused) tile.
    func resetSelectedZoom() {
        guard let camera = selectedCamera else { return }
        resetZoom(camera)
    }

    private func resetZoom(_ camera: CameraRef) {
        guard zoom(of: camera).isZoomed else { return }
        let frame = pictureSizes[camera].flatMap { size in
            frames.leaves.first { $0.camera == camera }.map { zoomFrame($0.rect, size) }
        } ?? TileZoom.wholeFrame
        zoomTargets[camera] = (1, CGPoint(x: 0.5, y: 0.5), frame)
    }

    /// One display refresh of the mouse-wheel easing.
    private func stepZoom() {
        let now = CACurrentMediaTime()
        let dt = lastPrepare.map { min(now - $0, 0.1) } ?? 1.0 / 60
        lastPrepare = now
        guard !zoomTargets.isEmpty else { return }
        for (camera, t) in zoomTargets {
            let current = zoom(of: camera)
            let scale = TileZoom.eased(from: current.scale, to: t.scale, dt: dt)
            let q = current.picturePoint(atUnit: t.unit, in: t.frame)
            setZoom(current.zoomed(to: scale, keeping: q, atUnit: t.unit, in: t.frame), for: camera)
            if scale == t.scale { zoomTargets[camera] = nil }
        }
    }

    /// Leaves focus without animating (the window closes, the tree changes).
    /// Ends focus at once, without animation, when the window's content is
    /// replaced (another layout, group, or camera). Unlike `tearDown` the
    /// stage keeps drawing: tearing it down here left the video surface
    /// showing the old arrangement under the new tiles.
    func cancelFocus() {
        guard state.focused != nil else { return }
        endFocusImmediately()
        syncDemands()
    }

    /// The window is closing: ends focus, withdraws every stream request,
    /// and stops the renderer asking this stage for frames.
    func tearDown() {
        endFocusImmediately()
        syncDemands(clearAll: true)
        grid.prepare = nil
    }

    // MARK: Stream quality (docs/UX.md "Stream quality per tile")

    /// Tells the leases what each tile asks of its camera: its quality, focus
    /// (once the focus animation is done) and zoom. The leases keep each
    /// camera on the highest stream any tile of any window asks for.
    private func syncDemands(clearAll: Bool = false) {
        var owners: Set<DemandOwner> = []
        if !clearAll {
            let id = ObjectIdentifier(self)
            for leaf in state.tree.leaves {
                guard let camera = leaf.camera else { continue }
                let owner = DemandOwner(stage: id, path: leaf.path)
                owners.insert(owner)
                model.leases.setDemand(
                    owner: owner, camera: camera, quality: state.tree[leaf.path]?.quality ?? .auto,
                    focused: focusedCamera != nil && state.focused == leaf.path,
                    zoomScale: state.zoom[camera]?.scale ?? 1)
            }
        }
        for owner in demandOwners.subtracting(owners) { model.leases.clearDemands(owner: owner) }
        demandOwners = owners
    }

    /// Sets the quality of the tile at `path`; saved with the layout.
    func setQuality(_ quality: TileQuality, at path: NodePath) {
        edit { $0.settingQuality(quality, at: path) }
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

    /// A plain drag in a tile zoomed past 1x pans its picture; at 1x, or
    /// while editing the layout, it drags the tile to swap (docs/UX.md
    /// "Digital zoom").
    private func panGesture(at p: CGPoint) -> Gesture? {
        guard !state.editing, let leaf = drawnLeaf(at: p), let camera = leaf.camera, zoom(of: camera).isZoomed else {
            return nil
        }
        return .pan(camera, last: p)
    }

    fileprivate func mouseDown(at p: CGPoint, clickCount: Int) {
        window?.makeFirstResponder(self)
        if state.focused != nil {
            if clickCount == 2 { unfocus() } else { gesture = panGesture(at: p) }
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
        gesture = panGesture(at: p) ?? .tile(from: leaf.path, start: p, dragging: false)
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
        case .pan(let camera, let last)?:
            gesture = .pan(camera, last: p)
            // Measured in the tile where the drag began, wherever it goes.
            guard let leaf = currentLeaves.last(where: { $0.camera == camera && $0.rect.contains(last) }) else { return }
            pan(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: CGVector(dx: p.x - last.x, dy: p.y - last.y))
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
        // Space, Return and the arrows arrive as window shortcuts
        // (ViewerWindow); what is left here is the stage's own.
        switch event.keyCode {
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

    /// Quality ▸ Auto, Clear, Fluent, and Balanced where the camera has it.
    private func qualityMenuItem(camera: CameraRef, path: NodePath) -> NSMenuItem {
        let current = model.leases.effectiveQuality(state.tree[path]?.quality ?? .auto, camera)
        let sub = NSMenu()
        for q in TileQuality.allCases where q != .balanced || model.leases.offersBalanced(camera) {
            let i = ClosureMenuItem(title: q.title) { [weak self] in self?.setQuality(q, at: path) }
            i.state = q == current ? .on : .off
            switch q {
            case .auto: i.toolTip = "The low stream in the grid; the main stream when focused or zoomed in"
            case .clear: i.toolTip = "The main stream, also in the grid"
            case .fluent: i.toolTip = "The low stream, also when focused"
            case .balanced: i.toolTip = "The middle stream"
            }
            sub.addItem(i)
        }
        sub.addItem(.separator())
        let note = NSMenuItem(title: "The main stream on many tiles is costly", action: nil, keyEquivalent: "")
        note.isEnabled = false
        sub.addItem(note)
        let item = NSMenuItem(title: "Quality", action: nil, keyEquivalent: "")
        item.submenu = sub
        return item
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
            menu.addItem(qualityMenuItem(camera: camera, path: leaf.path))
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

    // A mouse wheel zooms around the pointer; a trackpad's (or Magic
    // Mouse's) two-finger scroll pans a zoomed tile, and zooms with Option
    // held; a pinch zooms.
    override func scrollWheel(with event: NSEvent) {
        guard let stage else { return }
        let p = point(event)
        if event.hasPreciseScrollingDeltas {
            if event.modifierFlags.contains(.option) {
                stage.zoom(at: p, by: exp(Double(event.scrollingDeltaY) * 0.01), eased: false)
            } else {
                stage.pan(at: p, by: CGVector(dx: event.scrollingDeltaX, dy: event.scrollingDeltaY))
            }
        } else if event.scrollingDeltaY != 0 {
            stage.zoom(at: p, by: pow(1.25, Double(event.scrollingDeltaY)), eased: true)
        }
    }

    override func magnify(with event: NSEvent) {
        stage?.zoom(at: point(event), by: 1 + Double(event.magnification), eased: false)
    }

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
