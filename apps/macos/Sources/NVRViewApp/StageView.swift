import AppKit
import NVRCore
import QuartzCore
import VideoKit
import AppBase

/// One window's arrangement and what the user is doing with it.
@MainActor
final class WindowState {
    var content: WindowContent
    var name: String
    /// The arrangement shown. Every leaf has a tile identity
    /// (`LayoutNode.withTileIDs`, given on every set), and the views of
    /// tiles no longer in it are forgotten.
    var tree: LayoutNode {
        didSet {
            let named = tree.withTileIDs
            if named != tree { tree = named }
            views.prune(layout: viewsKey, tree: tree)
        }
    }
    var selected: NodePath?
    /// The focused tile (also during the animations in and out).
    var focused: NodePath?
    var editing = false
    /// The window's Fill Tiles switch, and per tile its digital zoom (or
    /// at 1x its filled crop's position) and own Fill choice, in picture
    /// coordinates (docs/UX.md "Digital zoom", "Fill Tiles"). Keyed by
    /// layout, tile and camera (`TileKey`), so two tiles of one camera are
    /// independent views; a view stays through focus, layout switches and
    /// the switch to the main stream, moves with its camera in a swap, and
    /// starts over when its tile is given another camera. Saved with the
    /// window.
    var views = TileViews<TileKey>()
    /// Whether the window's switch is on (tiles without their own choice
    /// fill).
    var fill: Bool { views.fill }

    init(content: WindowContent, name: String, tree: LayoutNode) {
        self.content = content
        self.name = name
        self.tree = tree.withTileIDs
    }

    /// The name this content's tile views go by.
    var viewsKey: String { content.viewsKey }

    /// The view key of the tile at `path` (nil while it shows no camera).
    func viewKey(_ path: NodePath) -> TileKey? { tree.tileKey(at: path, layout: viewsKey) }

    /// The window shows `content` under another name (an unsaved
    /// arrangement saved, a group edited): the views go along.
    func rename(to content: WindowContent) {
        views.rename(layout: viewsKey, to: content.viewsKey)
        self.content = content
    }
}

extension WindowContent {
    /// The name the window's tile views go by for this content.
    var viewsKey: String {
        switch self {
        case .layout(let id): TileKey.layoutKey(layout: id)
        case .group(let id): TileKey.layoutKey(group: id)
        case .scratch: TileKey.scratchKey
        }
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
    static let focusDuration = FocusAnimation.defaultDuration

    let model: AppModel
    let state: WindowState
    weak var delegate: StageDelegate?
    let grid: MetalGridView
    private let overlay = StageOverlay()
    /// The view over the tiles that holds their chrome.
    var tileOverlay: NSView { overlay }
    private(set) var chromes: [NodePath: TileChromeView] = [:]
    private(set) var frames = LayoutFrames(leaves: [], dividers: [])
    private var lastKey: RenderKey?
    /// The call window's stage (docs/CAPTURE.md, "The call window is a
    /// regular tile"): one leaf, no layout editing, split, swap, focus,
    /// Show Camera, Remove or Clear, no alert insets, and the selection is
    /// always that tile. Its owner holds the camera's lease.
    let fixedTile: Bool
    /// The fixed tile asks for the main stream (Auto, by the drawn size
    /// and zoom: `CallWindowQuality`).
    private(set) var fixedAsksMain = false
    /// The fixed tile's sub-stream picture width, once it showed one.
    private var fixedSubWidth: Double?
    /// The short message over the stage ("… is coming soon").
    let toast = ToastView()
    /// When the toast goes, on the stage's clock.
    var toastHideAt: Double?
    /// The camera motion popover (pan pad or lens sliders) and what drives it.
    var ptzPopover: NSPopover?
    var ptzPopoverController: PTZPopoverController?
    var ptzObserver: Int?
    /// The pictures this stage saved, the last one with what it was made
    /// from (self-test).
    var pictureSaves = 0
    var lastPictureSave: PictureSaveRecord?

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
    /// The clock every animation and timeout of the stage reads (the
    /// model's: the system clock in the app, a manual one in the self-test).
    let clock: any ViewerClock
    /// When the chrome hides; seconds of no interaction.
    private(set) var idle: ChromeIdle
    /// The overlay chrome is shown (not hidden for idleness).
    var chromeVisible: Bool { idle.visible }
    private var insets: [NodePath: ZoomInsetView] = [:]
    /// Mouse-wheel zoom easing toward a target scale, per tile view, around
    /// a point of the shown picture.
    private var zoomEasing = ZoomEasing<TileKey>()
    /// Virtual PTZ zoom easing toward a field of view, per tile.
    private var fisheyeEasing = FisheyeZoomEasing<NodePath>()
    /// When a fisheye view moved by scrolling or easing is saved with the
    /// layout (once it rests), on the stage's clock.
    private var fisheyeSaveDue: Double?
    /// The picture's layout size per camera, from the last refresh.
    var pictureSizes: [CameraRef: CGSize] = [:]

    private var focusAnimation = FocusAnimation(duration: StageView.focusDuration)
    /// The camera this stage told the leases it focuses.
    private var focusedCamera: CameraRef?
    /// A tile of this stage as the leases know it (stream quality demands).
    private struct DemandOwner: Hashable {
        let stage: ObjectIdentifier
        let path: NodePath
    }
    private var demandOwners: Set<DemandOwner> = []

    // Live smart-alert checks (docs/UX.md "Smart alerts"; StageView+Verify.swift):
    // a band in each tile showing a checked camera, an overlay on each
    // verification inset.
    var verifyBands: [NodePath: VerificationOverlayView] = [:]
    var verifyInsetOverlays: [CameraRef: VerificationOverlayView] = [:]
    var verifyToken: UUID?
    /// Where each live check was first placed in this window (tile or
    /// inset): it keeps that place until it ends, however the stage changes
    /// (focus, a swap, another layout), so it does not jump around.
    var verifyPlaced: [UInt64: VerifyPlace] = [:]
    /// The placement setting `verifyPlaced` was decided under (choosing another
    /// in Settings places the live checks again).
    var verifyPlacedFor: VerifyPlacementSetting?

    // Alert insets (docs/UX.md "Alert insets"; StageView+Insets.swift).
    var insetBoard = InsetBoard()
    var alertInsetViews: [CameraRef: InsetView] = [:]
    /// Where each drawn inset's picture goes, in the overlay's coordinates.
    var insetVideoRects: [CameraRef: CGRect] = [:]
    let moreChip = MoreChipView(labelWithString: "")
    /// This window's own inset grid; nil: the Settings default
    /// (`effectiveInsetGrid`). Saved with the window.
    var insetGridOverride: InsetGridSetting? {
        didSet { if insetGridOverride != oldValue { syncInsets() } }
    }
    /// The grid the board and layout were last given.
    var appliedInsetGrid = InsetGridSetting.standard
    /// The user chose the window's grid (save the window).
    var onInsetGridChange: () -> Void = {}
    /// Whether the last layout left drawn insets out for lack of room.
    var insetsTruncated = false
    /// Holds the insets above the tiles' chrome (top-left origin; clicks
    /// between insets reach the stage).
    let insetLayer = PassThroughView()
    /// The swapped-in camera's header and a doorbell call over the stage.
    let stageCall = StageCallView()
    /// The leases the insets and the swapped-in camera hold.
    var insetHeld: [CameraRef: Int] = [:]
    var detectionToken: UUID?
    /// The swapped-in camera, as the leases know its demand (main stream).
    var takeoverCamera: CameraRef?
    /// Each open call's presentation as this window last saw it (a call
    /// that pops in gets its inset here even if the window had no item).
    var seenCallPresentations: [CameraRef: CallPresentation] = [:]
    /// Where the user moved this window's inset column (saved with the
    /// window); nil: the default corner.
    var insetAnchor: InsetAnchor? {
        didSet { if insetAnchor != oldValue { layoutInsetViews() } }
    }
    /// The column's position while it is being dragged.
    var insetDragAnchor: InsetAnchor?
    /// Where a column drag started: the pointer and the column's frame.
    var insetDragStart: (pointer: CGPoint, column: CGRect)?
    /// The inset column's frame as last laid out (nil: no inset drawn).
    var insetColumn: CGRect?
    /// The user moved the column or reset its position (save the window).
    var onInsetAnchorChange: () -> Void = {}
    /// The window's picture settings changed (a zoom, a pan, a Fill
    /// choice): the owner saves the window (debounced there).
    var onViewsChanged: () -> Void = {}

    private enum Gesture {
        case divider(NodePath)
        case tile(from: NodePath, start: CGPoint, dragging: Bool)
        /// A drag in a zoomed tile, or on a filled picture with part hidden,
        /// pans its picture.
        case pan(TileKey, last: CGPoint)
        /// A drag in a dewarped fisheye tile turns its view.
        case aim(NodePath, last: CGPoint)
    }
    private var gesture: Gesture?
    private var dropTarget: NodePath?
    /// The leaf each of the renderer's tiles draws, in its order.
    private var renderedPaths: [NodePath] = []

    var gap: CGFloat { state.editing ? 6 : 2 }

    init(model: AppModel, state: WindowState, renderer: GridRenderer, fixedTile: Bool = false) {
        self.model = model
        self.state = state
        self.fixedTile = fixedTile
        clock = model.clock
        idle = ChromeIdle(now: model.clock.now)
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
        installInsets()
        installVerification()
        installToast()
        installPTZ()
        if fixedTile { state.selected = state.tree.leaves.first?.path }
        overlay.registerForDraggedTypes([.string])
        alertActive = { [weak model] in model?.alerts.isActive($0) == true }
        dismissAlert = { [weak model] in model?.dismissAlert($0) }
        grid.prepare = { [weak self] renderer, size in
            MainActor.assumeIsolated { self?.prepare(renderer: renderer, drawable: size) }
        }
        if PacingRecorder.traceFromEnvironment {
            grid.renderer.onNewPicture = { [weak self] _, _, _ in
                MainActor.assumeIsolated { self?.trace.drawn += 1 }
            }
        }
    }

    /// NVRVIEW_PACING_TRACE=1: what the window's live tiles delivered and drew, logged every ten seconds.
    private var trace = (since: 0.0, drawn: 0, refreshes: 0, busy: Int64(0))

    private func tracePacing(now: Double) {
        guard PacingRecorder.traceFromEnvironment else { return }
        if trace.since == 0 { trace.since = now }
        guard now - trace.since >= 10 else { return }
        let tiles = state.tree.cameras.compactMap { model.leases.tile(for: $0) }
        let reports = tiles.map { t in
            defer { t.pacing.reset() }
            return t.pacing.report()
        }
        let delivered = reports.map(\.released.pictures).reduce(0, +)
        let seconds = now - trace.since
        let refreshes = grid.refreshes - trace.refreshes, busy = grid.refreshBusyMicros - trace.busy
        NSLog(
            "pacing: %d tiles, delivered %.1f/s, drawn %.1f/s (%.2f), %.1f refreshes/s, %.1f ms of main thread per refresh; first tile %@",
            tiles.count, Double(delivered) / seconds, Double(trace.drawn) / seconds,
            delivered > 0 ? Double(trace.drawn) / Double(delivered) : 0, Double(refreshes) / seconds,
            refreshes > 0 ? Double(busy) / Double(refreshes) / 1000 : 0, reports.first.map { String(describing: $0) } ?? "none")
        trace = (now, 0, grid.refreshes, grid.refreshBusyMicros)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var acceptsFirstResponder: Bool { true }

    // MARK: Layout

    /// The tiles where they are now (focus animation applied).
    private var currentLeaves: [LeafFrame] {
        RectAnimation.focusFrames(
            grid: frames, focused: state.focused, bounds: overlay.bounds,
            progress: state.focused == nil ? 0 : focusAnimation.progress)
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
            let view = TileChromeView(clock: clock)
            let path = leaf.path
            view.onDismissAlert = { [weak self] in
                guard let self, let camera = self.frames.leaves.first(where: { $0.path == path })?.camera else { return }
                self.dismissAlert(camera)
                self.refreshChrome()
            }
            view.onItem = { [weak self] item, button in self?.performHeaderItem(item, at: path, from: button) }
            view.isPending = { TileActionStub.isPending($0) }
            view.onRetry = { [weak self] in
                guard let self, let camera = self.frames.leaves.first(where: { $0.path == path })?.camera else { return }
                self.model.retry(camera)
                self.refreshChrome()
            }
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
        // Which cameras the window shows decides where live checks go.
        if verifyToken != nil { syncVerification() }
    }

    override func layout() {
        super.layout()
        frames = state.tree.frames(in: overlay.bounds, gap: gap)
        layoutChrome()
        layoutInsetViews()
        if !toast.isHidden { toast.place(in: bounds) }
        window?.invalidateCursorRects(for: overlay)
    }

    private func layoutChrome() {
        let leaves = insetBoard.swap == nil ? currentLeaves : []
        let shown = Set(leaves.map(\.path))
        for (path, view) in chromes { view.isHidden = !shown.contains(path) }
        for leaf in leaves {
            guard let view = chromes[leaf.path] else { continue }
            if view.frame != leaf.rect { view.frame = leaf.rect }
        }
        layoutInsets(leaves)
        layoutVerification(leaves)
        // The focused tile's chrome rides on top while it grows.
        if let f = state.focused, let view = chromes[f], overlay.subviews.last !== (insets[f] ?? view) {
            overlay.addSubview(view, positioned: .above, relativeTo: nil)
            if let inset = insets[f] { overlay.addSubview(inset, positioned: .above, relativeTo: nil) }
        }
    }

    /// Lays the tiles' chrome out again (the swap hides it).
    func relayoutChrome() { layoutChrome() }

    /// Places and updates the zoom maps: shown while the tile is zoomed and
    /// the chrome is up.
    private func layoutInsets(_ leaves: [LeafFrame]) {
        let shown = Set(leaves.map(\.path))
        for (path, inset) in insets where !shown.contains(path) { inset.setShown(false) }
        for leaf in leaves {
            guard let inset = insets[leaf.path] else { continue }
            guard let camera = leaf.camera, let key = state.viewKey(leaf.path), case let z = state.views.zoom(key),
                z.isZoomed, fisheyeMode(leaf.path) == nil,
                let picture = pictureSizes[camera], picture.width > 0, picture.height > 0
            else {
                inset.setShown(false)
                continue
            }
            // In the picture as drawn zoomed (it grows over the bars).
            let pictureRect = TilePlacement.shownRect(
                rect: leaf.rect, picture: picture, fill: state.views.fills(key), zoom: z)
            let frame = ZoomInsetView.frame(inPicture: pictureRect, aspect: picture.width / picture.height)
            if inset.frame != frame { inset.frame = frame }
            inset.update(region: z.visible(in: zoomBase(leaf.rect, picture, key)))
            inset.setShown(chromeVisible)
        }
    }

    /// Re-reads names, badges, veils and listening from the model.
    func refreshChrome() {
        refreshInsets()
        for leaf in frames.leaves {
            guard let view = chromes[leaf.path] else { continue }
            view.apply(content(for: leaf))
            view.setHeaderPinned(leaf.camera.map(alertActive) == true)
            view.setOutlineShown(chromeVisible)
        }
    }

    private func alertLines(for camera: CameraRef) -> [String]? {
        model.alerts.alert(for: camera)?.lines(now: Date())
    }

    private func content(for leaf: LeafFrame) -> TileChromeContent {
        // The call window's one tile is always selected; no outline there.
        let selected = !fixedTile && state.selected == leaf.path && (state.editing || state.focused == nil)
        guard let camera = leaf.camera else {
            return TileChromeContent(
                title: "Empty tile", hasCamera: false, audible: false, selected: selected, editing: state.editing,
                dropTarget: dropTarget == leaf.path)
        }
        let active = model.detections.active(for: camera)
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
        if let mode = fisheyeMode(leaf.path) { detail.insert(mode.title, at: 1) }
        detail.append(model.hasCameraAI(camera) ? "detection: camera AI" : "detection: this Mac")
        if let a = active.first { detail[detail.count - 1] = "\(DetectionTracker.label(a.kind)) by \(DetectionTracker.sourceLabel(a.source))" }
        return TileChromeContent(
            title: model.name(of: camera), subtitle: detail.joined(separator: " · "),
            rates: model.showFrameRates ? rateText(leaf.path, camera) : nil,
            indicators: IndicatorItem.items(from: active), corner: model.indicatorCorner,
            alertLines: alertLines(for: camera),
            veil: model.veil(for: camera), retry: model.retryOffered(for: camera), hasCamera: true, audible: model.audible.contains(camera),
            header: headerTile(camera, path: leaf.path), recordingPill: model.capture.board.pill(for: camera),
            selected: selected, editing: state.editing, dropTarget: dropTarget == leaf.path)
    }

    /// Whether the tile at `path` is zoomed past 1x, or shows a moved
    /// virtual camera (its Reset Zoom applies).
    func isZoomed(_ path: NodePath) -> Bool {
        if fisheyeMode(path) != nil { return state.tree[path]?.fisheye.aim != nil }
        return state.viewKey(path).map { state.views.zoom($0).isZoomed } == true
    }

    /// Whether the tile at `path` shows a filled picture's crop moved off
    /// center at 1x (its Reset Position applies).
    func isPanned(_ path: NodePath) -> Bool {
        guard fisheyeMode(path) == nil, let key = state.viewKey(path) else { return false }
        return state.views.fills(key) && state.views.zoom(key).isPanned
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
        // Sound: a calm slate, apart from the object colors.
        case "smoke_alarm", "co_alarm": NSColor(calibratedRed: 0.4, green: 0.47, blue: 0.58, alpha: 1)
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
        var dewarps: [DewarpConstants?]
    }

    /// Called by the Metal view on every refresh with the drawable size in
    /// pixels: advances the focus animation, lays out the tiles, feeds the
    /// level meters. Assigns the renderer's tiles only when something changed.
    private func prepare(renderer: GridRenderer, drawable: CGSize) {
        tick(now: clock.now)
        let points = overlay.bounds
        guard points.width > 0, points.height > 0 else { return }
        // Per axis: during a resize the drawable may lag the view (it is
        // stretched to the bounds until its size catches up).
        let scaleX = drawable.width / points.width, scaleY = drawable.height / points.height
        var key = RenderKey(tiles: [], rects: [], versions: [], sizes: [], zooms: [], dewarps: [])
        var tiles: [GridTile] = []
        var paths: [NodePath] = []
        /// One tile drawn into `rect` (points): the layout's, the swapped-in
        /// camera's over the whole stage, or an inset's on top.
        func draw(_ tile: CoreTile, camera: CameraRef, rect: CGRect, zoom: TileZoom, fill: Bool, path: NodePath? = nil) {
            let cell = rect.pixels(scaleX: scaleX, scaleY: scaleY)
            let snapshot = tile.snapshot
            let picture = tile.latestPicture()
            let size = picture.map { PixelSize(width: UInt32($0.width), height: UInt32($0.height)) }
            let mapping = PictureMapping(snapshot: snapshot, pictureSize: size)
            let pictureSize = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
            let matrix = snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            if pictureSize != .zero, pictureSizes[camera] != pictureSize {
                pictureSizes[camera] = pictureSize
                // Whether a drag pans depends on the picture's shape.
                window?.invalidateCursorRects(for: overlay)
            }
            // Zoomed, the picture grows over the bars, then crops (TilePlacement).
            let placed = TilePlacement.place(
                rect: rect, scaleX: scaleX, scaleY: scaleY, picture: pictureSize,
                offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)), fill: fill, zoom: zoom)
            // A dewarped fisheye tile fills its whole cell through the dewarp
            // pipeline (docs/RENDERING.md "Fisheye dewarping").
            let dewarp = path.flatMap { fisheyeDewarp(camera: camera, path: $0, picture: picture, tileSize: cell.size) }
            if let dewarp {
                tiles.append(GridTile(picture: tile, color: matrix, destination: cell, dewarp: dewarp.constants))
            } else {
                tiles.append(GridTile(picture: tile, color: matrix, destination: placed.destination, source: placed.source))
            }
            key.dewarps.append(tiles.last?.dewarp)
            key.tiles.append(ObjectIdentifier(tile))
            key.rects.append(cell)
            key.versions.append(snapshot.version)
            key.sizes.append(pictureSize)
            key.zooms.append(zoom)
        }
        // After a swap the swapped-in camera fills the stage instead of the
        // layout (nothing is drawn under it).
        if let shown = insetBoard.swap?.shown {
            if let tile = model.leases.tile(for: shown) {
                draw(tile, camera: shown, rect: points, zoom: .none, fill: false)
            }
        } else {
            for leaf in currentLeaves {
                guard let camera = leaf.camera, let tile = model.leases.tile(for: camera) else { continue }
                paths.append(leaf.path)
                draw(
                    tile, camera: camera, rect: leaf.rect, zoom: zoom(atTile: leaf.path), fill: fills(atTile: leaf.path),
                    path: leaf.path)
                if fixedTile {
                    followFixedDemand(
                        tile, camera: camera, drawnWidth: Double(leaf.rect.width * scaleX) * zoom(atTile: leaf.path).scale)
                }
                // Read only for meters on screen (audible, or hovered).
                if let chrome = chromes[leaf.path], chrome.wantsLevel, let level = model.level(camera) {
                    chrome.setLevel(level.peak)
                }
            }
        }
        // Insets on top, the top of the stack drawn last; the same decoded
        // pictures as every other tile of that camera (no extra decode).
        for item in insetBoard.shown.reversed() {
            guard let rect = insetVideoRects[item.camera] else { continue }
            if item.isReturn && item.showsLayout {
                // The whole layout in miniature: the stage's own leaves with
                // their zoom and fill, drawn from the same tiles (their
                // streams are as the stage asks for them).
                let mini = state.tree.frames(in: CGRect(origin: .zero, size: rect.size), gap: 1)
                for leaf in mini.leaves {
                    guard let camera = leaf.camera, let tile = model.leases.tile(for: camera) else { continue }
                    draw(
                        tile, camera: camera, rect: leaf.rect.offsetBy(dx: rect.minX, dy: rect.minY),
                        zoom: zoom(atTile: leaf.path), fill: fills(atTile: leaf.path), path: leaf.path)
                }
                continue
            }
            guard let tile = model.leases.tile(for: item.camera) else { continue }
            draw(tile, camera: item.camera, rect: rect, zoom: .none, fill: false)
        }
        if key != lastKey || renderer.tiles.count != tiles.count {
            if key.zooms != lastKey?.zooms { layoutInsets(currentLeaves) }
            lastKey = key
            renderer.tiles = tiles
            renderedPaths = paths
        }
    }

    /// Auto for the call window's tile: the main stream while its picture
    /// is drawn (zoom included) wider than the sub-stream's pictures.
    private func followFixedDemand(_ tile: CoreTile, camera: CameraRef, drawnWidth: Double) {
        if let picture = pictureSizes[camera], picture.width > 0, tile.tierState.shown == model.leases.tiers(for: camera).grid {
            fixedSubWidth = Double(picture.width)
        }
        let main = CallWindowQuality.wantsMain(
            drawnWidth: drawnWidth, subWidth: fixedSubWidth, wasMain: fixedAsksMain)
        guard main != fixedAsksMain else { return }
        fixedAsksMain = main
        syncDemands()
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
    var isAnimatingFocus: Bool { focusAnimation.isRunning }
    /// A mouse-wheel zoom is still easing (self-test).
    var isEasingZoom: Bool { zoomEasing.isEasing }

    // MARK: Time

    /// One step of everything time-driven, at `now` on the stage's clock:
    /// the focus animation, zoom easing, chrome auto-hide and the headers'
    /// fade-out. The display link calls it on every refresh and the window's
    /// timer now and then (a window behind others gets few refreshes, and
    /// what is time-based must still finish); the self-test calls it after
    /// advancing its clock.
    func tick(now: Double) {
        stepFocus(now: now)
        stepZoom(now: now)
        stepFisheye(now: now)
        tickInsets(now: now)
        tickToast(now: now)
        if idle.tick(now: now) { hideChrome() }
        for view in chromes.values { view.tick(now: now) }
        model.ptz.tick(now: now)
        tracePacing(now: now)
    }

    /// `tick(now:)` at the clock's time.
    func tick() { tick(now: clock.now) }

    func toggleFocus(_ path: NodePath? = nil) {
        guard !fixedTile else { return }
        if state.focused != nil {
            unfocus()
        } else if let p = path ?? state.selected ?? (frames.leaves.count == 1 ? frames.leaves.first?.path : nil),
            state.tree[p]?.isLeaf == true
        {
            focus(p)
        }
    }

    func focus(_ path: NodePath) {
        guard !fixedTile, !state.editing, state.tree[path]?.camera != nil || frames.leaves.count > 1 else { return }
        state.selected = path
        state.focused = path
        animateFocus(to: 1)
        refreshChrome()
        if verifyToken != nil { syncVerification() }
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
        focusAnimation.start(to: target, now: clock.now)
    }

    private func stepFocus(now: Double) {
        guard focusAnimation.isRunning else { return }
        switch focusAnimation.tick(now: now) {
        case .focused?:
            if let f = state.focused, let camera = state.tree[f]?.camera {
                // After the animation: the sharper stream, swapped in by the
                // core after its first keyframe.
                focusedCamera = camera
                syncDemands()
                refreshChrome()
            }
        case .unfocused?:
            state.focused = nil
            refreshChrome()
            if verifyToken != nil { syncVerification() }
        case nil:
            break
        }
        layoutChrome()
    }

    private func endFocusImmediately() {
        focusedCamera = nil
        state.focused = nil
        focusAnimation.reset()
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
        guard !fixedTile else { return }
        if let f = state.focused {
            guard !focusAnimation.isRunning, let n = frames.neighbor(of: f, direction) else { return }
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
        guard idle.interact(now: clock.now) else { return }
        // Back: the outline and maps fade in; in full screen, the header of
        // the tile under the pointer too (moving the pointer also does that).
        refreshChrome()
        layoutInsets(currentLeaves)
        if fullScreen, let p = pointerInOverlay, let leaf = currentLeaves.last(where: { $0.rect.contains(p) }) {
            chromes[leaf.path]?.showChrome()
        }
    }

    /// The seconds of idleness before the chrome hides (the self-test
    /// shortens it).
    var chromeHideDelay: Double {
        get { idle.delay }
        set { idle.delay = newValue }
    }

    /// Hides the chrome once idle (from `tick`). Out of full screen the
    /// headers keep their own hover timing; the outline and zoom map fade
    /// either way.
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

    /// Whether a tile's veil shows its Retry button (self-test).
    func retryShown(_ path: NodePath) -> Bool? { chromes[path]?.isRetryShown }

    /// Presses a tile's Retry button, as a click would (self-test).
    func pressRetry(_ path: NodePath) { chromes[path]?.pressRetry() }

    /// Whether a tile's zoom map is shown (self-test).
    func zoomMapShown(_ path: NodePath) -> Bool? { insets[path]?.isShown }

    private var pointerInOverlay: CGPoint? {
        guard let window else { return nil }
        let p = overlay.convert(window.mouseLocationOutsideOfEventStream, from: nil)
        return overlay.bounds.contains(p) ? p : nil
    }

    // MARK: Digital zoom (docs/UX.md "Digital zoom")

    /// How the picture sits in the tile `rect` at 1x (fitted or filled).
    private func zoomBase(_ rect: CGRect, _ picture: CGSize, _ key: TileKey) -> ZoomBase {
        ZoomBase(picture: picture, tile: rect.size, fill: state.views.fills(key))
    }

    /// The tile under the pointer as drawn now (the focused one in focus).
    private func drawnLeaf(at p: CGPoint) -> LeafFrame? {
        currentLeaves.last { $0.rect.contains(p) }
    }

    /// For the tile at `p`: its view key, the unit point of `p` in the
    /// tile, the tile's size in points, and how its picture sits in it at
    /// 1x.
    private func zoomTarget(at p: CGPoint)
        -> (key: TileKey, path: NodePath, unit: CGPoint, size: CGSize, base: ZoomBase)?
    {
        guard let leaf = drawnLeaf(at: p), let camera = leaf.camera, let key = state.viewKey(leaf.path),
            let picture = pictureSizes[camera]
        else { return nil }
        return (key, leaf.path, TilePlacement.unit(p, in: leaf.rect), leaf.rect.size, zoomBase(leaf.rect, picture, key))
    }

    /// The tile drawn now whose view is `key`, if any.
    private func drawnLeaf(of key: TileKey) -> LeafFrame? {
        currentLeaves.first { $0.camera == key.camera && state.viewKey($0.path) == key }
    }

    /// How the picture of the view `key` sits at 1x now: from the picture's
    /// size at this refresh, so a picture that changed size or shape since
    /// the easing began (the main stream replacing the sub-stream) is
    /// followed.
    private func currentZoomBase(_ key: TileKey) -> ZoomBase {
        guard let picture = pictureSizes[key.camera], picture.width > 0, picture.height > 0,
            let leaf = drawnLeaf(of: key)
        else { return .whole }
        return zoomBase(leaf.rect, picture, key)
    }

    private func setZoom(_ z: TileZoom, for key: TileKey) {
        let old = state.views.zoom(key)
        guard z != old else { return }
        state.views.setZoom(z, for: key)
        if old.isZoomed != z.isZoomed || old.isPanned != z.isPanned {
            refreshChrome()
            window?.invalidateCursorRects(for: overlay)
        }
        syncDemands()
        viewsChanged()
    }

    /// The zoom of the tile at `path` (1x if none, or without a camera).
    func zoom(atTile path: NodePath) -> TileZoom { state.viewKey(path).map { state.views.zoom($0) } ?? .none }

    /// Whether the tile at `path` fills (its own choice, else the window's).
    func fills(atTile path: NodePath) -> Bool { state.viewKey(path).map { state.views.fills($0) } ?? state.fill }

    /// Fill Tile for one tile (the tile menu): it fills or fits.
    func setFill(_ fills: Bool, atTile path: NodePath) {
        guard let key = state.viewKey(path), state.views.fills(key) != fills else { return }
        state.views.setFill(fills, for: key)
        fillChanged()
    }

    /// Fill Tiles for the window: every tile follows (the tiles' own
    /// choices are cleared).
    func setWindowFill(_ fills: Bool) {
        state.views.setWindowFill(fills)
        fillChanged()
    }

    /// Whether a tile of the window's content has a Fill choice of its own
    /// (the menu's switch shows mixed).
    var fillMixed: Bool { state.views.mixed(layout: state.viewsKey) }

    private func fillChanged() {
        refreshChrome()
        window?.invalidateCursorRects(for: overlay)
        lastKey = nil
        delegate?.stageDidChangeState(self)
        viewsChanged()
    }

    /// Asks the owner to save the window (debounced there).
    private func viewsChanged() { onViewsChanged() }

    /// Whether a plain drag on the picture of the tile `leaf` moves the
    /// picture (zoomed in, or a filled picture with part hidden) instead of
    /// swapping the tile.
    func picturePans(_ leaf: LeafFrame) -> Bool {
        guard let camera = leaf.camera, let key = state.viewKey(leaf.path), fisheyeMode(leaf.path) == nil,
            let picture = pictureSizes[camera]
        else { return false }
        return state.views.zoom(key).canPan(in: zoomBase(leaf.rect, picture, key))
    }

    /// Zooms the tile at `p` by `factor` around `p`: at once (pinch), or
    /// eased over the next refreshes (a mouse-wheel notch).
    func zoom(at p: CGPoint, by factor: Double, eased: Bool) {
        if let leaf = drawnLeaf(at: p), fisheyeMode(leaf.path) != nil {
            zoomFisheye(leaf.path, by: factor, eased: eased)
            return
        }
        guard let t = zoomTarget(at: p) else { return }
        noteInteraction()
        if eased {
            let from = zoomEasing.target(for: t.key)?.scale ?? state.views.zoom(t.key).scale
            let target = min(max(from * factor, 1), TileZoom.maxScale)
            zoomEasing.set(.init(scale: target, unit: t.unit, tile: t.path), for: t.key, now: clock.now)
        } else {
            zoomEasing.cancel(t.key)
            setZoom(state.views.zoom(t.key).zoomed(by: factor, atUnit: t.unit, in: t.base), for: t.key)
        }
    }

    /// Pans the tile at `p` by `delta` points (the picture follows).
    func pan(at p: CGPoint, by delta: CGVector) {
        if let leaf = drawnLeaf(at: p), fisheyeMode(leaf.path) != nil {
            noteInteraction()
            turnFisheye(leaf.path, by: delta, size: leaf.rect.size)
            fisheyeSaveDue = clock.now + 0.5
            return
        }
        guard let t = zoomTarget(at: p), state.views.zoom(t.key).canPan(in: t.base), t.size.width > 0,
            t.size.height > 0
        else {
            return
        }
        zoomEasing.cancel(t.key)
        let unit = CGVector(dx: delta.dx / t.size.width, dy: delta.dy / t.size.height)
        setZoom(state.views.zoom(t.key).panned(byUnit: unit, in: t.base), for: t.key)
    }

    /// Back to 1x for the tile at `path` (its reset control): eased, or at
    /// once (`eased` false: the self-test, which keeps its clock still).
    func resetZoom(at path: NodePath, eased: Bool = true) {
        if fisheyeMode(path) != nil {
            resetFisheye(path)
            return
        }
        guard let key = state.viewKey(path) else { return }
        resetZoom(key, eased: eased)
    }

    /// 0: back to 1x on the selected (or focused) tile; a dewarped fisheye
    /// tile back to its default view.
    func resetSelectedZoom() {
        guard let path = selectedPath else { return }
        resetZoom(at: path)
    }

    /// Back to 1x and, for a filled picture, its crop back to the middle:
    /// eased from a zoom, at once from a crop moved at 1x.
    private func resetZoom(_ key: TileKey, eased: Bool = true) {
        let z = state.views.zoom(key)
        guard !z.isDefault else { return }
        guard z.isZoomed, eased else {
            zoomEasing.cancel(key)
            setZoom(.none, for: key)
            return
        }
        zoomEasing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5), recenter: true), for: key, now: clock.now)
    }

    /// One step of the mouse-wheel easing (from `tick`). Time-based, so it
    /// finishes however few refreshes there are, and it ends exactly on the
    /// target (1x is no zoom at all). A view whose tile went (removed, or
    /// given another camera) stops easing.
    private func stepZoom(now: Double) {
        guard zoomEasing.isEasing else { return }
        let steps = zoomEasing.tick(
            now: now, current: { state.views.zoom($0) },
            base: { key, _ in currentZoomBase(key) })
        for step in steps {
            guard drawnLeaf(of: step.key) != nil else {
                zoomEasing.cancel(step.key)
                continue
            }
            setZoom(step.zoom, for: step.key)
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
        tearDownVerification()
        tearDownInsets()
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
                let focused = fixedTile ? fixedAsksMain : focusedCamera != nil && state.focused == leaf.path
                model.leases.setDemand(
                    owner: owner, camera: camera, quality: state.tree[leaf.path]?.quality ?? .auto,
                    focused: focused, zoomScale: zoom(atTile: leaf.path).scale)
            }
        }
        for owner in demandOwners.subtracting(owners) { model.leases.clearDemands(owner: owner) }
        demandOwners = owners
    }

    /// Sets the quality of the tile at `path`; saved with the layout.
    func setQuality(_ quality: TileQuality, at path: NodePath) {
        edit { $0.settingQuality(quality, at: path) }
    }

    // MARK: Fisheye views (docs/UX.md "Fisheye cameras")

    /// The dewarped view of the tile at `path`, or nil when it shows its
    /// picture as is (not a fisheye camera, or Original).
    func fisheyeMode(_ path: NodePath) -> FisheyeViewMode? {
        guard let camera = state.tree[path]?.camera, model.isFisheye(camera) else { return nil }
        let mode = state.tree[path]?.fisheye.mode ?? .original
        return mode.dewarps ? mode : nil
    }

    /// The dewarp of the tile at `path` drawn `tileSize` pixels big, or nil
    /// when it draws its picture as is (also until the circle is known).
    func fisheyeDewarp(camera: CameraRef, path: NodePath, picture: DecodedPicture?, tileSize: CGSize) -> FisheyeDewarp? {
        guard let mode = fisheyeMode(path), let lens = model.fisheyeLens(camera, picture: picture) else { return nil }
        let mount = model.cameraOptions(camera).fisheyeMount
        let setting = state.tree[path]?.fisheye ?? .original
        return FisheyeDewarp(lens: lens, mount: mount, mode: mode, aim: setting.aim(for: mount), tileSize: tileSize)
    }

    /// The dewarp a tile drew last, at its size then (self-test).
    func fisheyeDewarp(of path: NodePath) -> FisheyeDewarp? {
        guard let i = renderedPaths.firstIndex(of: path), i < grid.renderer.tiles.count,
            grid.renderer.tiles[i].dewarp != nil,
            let camera = state.tree[path]?.camera, let tile = model.leases.tile(for: camera)
        else { return nil }
        return fisheyeDewarp(
            camera: camera, path: path, picture: tile.latestPicture(), tileSize: grid.renderer.tiles[i].destination.size)
    }

    /// Where a tile is drawn in the drawable's pixels (self-test).
    func renderedDestination(of path: NodePath) -> CGRect? {
        guard let i = renderedPaths.firstIndex(of: path), i < grid.renderer.tiles.count else { return nil }
        return grid.renderer.tiles[i].destination
    }

    private func mount(_ path: NodePath) -> FisheyeMount {
        state.tree[path]?.camera.map { model.cameraOptions($0).fisheyeMount } ?? .ceiling
    }

    /// Sets the view of the tile at `path`; saved with the layout.
    func setFisheyeMode(_ mode: FisheyeViewMode, at path: NodePath) {
        fisheyeEasing.cancel(path)
        let aim = state.tree[path]?.fisheye.aim
        edit { $0.settingFisheye(FisheyeTileSetting(mode: mode, aim: aim), at: path) }
    }

    /// Moves a tile's view in memory (saved by `saveFisheye`).
    private func setAim(_ aim: FisheyeAim?, at path: NodePath) {
        guard let mode = state.tree[path]?.fisheye.mode,
            let tree = state.tree.settingFisheye(FisheyeTileSetting(mode: mode, aim: aim), at: path)
        else { return }
        let hadAim = state.tree[path]?.fisheye.aim != nil
        state.tree = tree
        if hadAim != (aim != nil) { refreshChrome() }
    }

    /// Saves the tree with the moved views (a drag ended, scrolling or
    /// easing came to rest).
    private func saveFisheye() {
        fisheyeSaveDue = nil
        delegate?.stage(self, didEdit: state.tree)
    }

    /// A drag (or two-finger scroll) of `delta` points in a tile `size`
    /// points big turns its view; the picture follows the pointer.
    func turnFisheye(_ path: NodePath, by delta: CGVector, size: CGSize) {
        guard let mode = fisheyeMode(path) else { return }
        let m = mount(path)
        let aim = (state.tree[path]?.fisheye ?? .original).aim(for: m)
        setAim(aim.dragged(by: delta, viewSize: size, mode: mode, mount: m), at: path)
    }

    /// The wheel or a pinch: Virtual PTZ zooms (eased for a wheel notch).
    func zoomFisheye(_ path: NodePath, by factor: Double, eased: Bool) {
        guard fisheyeMode(path) == .ptz else { return }
        noteInteraction()
        let m = mount(path)
        let aim = (state.tree[path]?.fisheye ?? .original).aim(for: m)
        if eased {
            let from = fisheyeEasing.target(for: path) ?? aim.fov
            fisheyeEasing.set(fov: from / factor, for: path, now: clock.now)
        } else {
            fisheyeEasing.cancel(path)
            setAim(aim.zoomed(by: factor, mount: m), at: path)
            fisheyeSaveDue = clock.now + 0.5
        }
    }

    private func resetFisheye(_ path: NodePath) {
        fisheyeEasing.cancel(path)
        setAim(nil, at: path)
        saveFisheye()
    }

    /// One step of the PTZ zoom easing and the deferred save (from `tick`).
    private func stepFisheye(now: Double) {
        if fisheyeEasing.isEasing {
            let steps = fisheyeEasing.tick(now: now) { path in
                (state.tree[path]?.fisheye ?? .original).aim(for: mount(path)).fov
            }
            for step in steps {
                var aim = (state.tree[step.key]?.fisheye ?? .original).aim(for: mount(step.key))
                aim.fov = step.fov
                setAim(aim, at: step.key)
            }
            if !fisheyeEasing.isEasing { fisheyeSaveDue = now }
        }
        if let due = fisheyeSaveDue, now >= due, gesture == nil { saveFisheye() }
    }

    /// Whether a fisheye zoom is still easing (self-test).
    var isEasingFisheye: Bool { fisheyeEasing.isEasing }

    /// Fisheye View ▸ Original, Panorama, Double Panorama, Quad, Virtual PTZ.
    func fisheyeMenuItem(path: NodePath) -> NSMenuItem {
        let current = state.tree[path]?.fisheye.mode ?? .original
        let sub = NSMenu()
        for mode in FisheyeViewMode.allCases {
            let i = ClosureMenuItem(title: mode.title) { [weak self] in self?.setFisheyeMode(mode, at: path) }
            i.state = mode == current ? .on : .off
            switch mode {
            case .original: i.toolTip = "The circle as the camera sends it, with digital zoom"
            case .panorama: i.toolTip = "Everything around the camera in one strip"
            case .doublePanorama: i.toolTip = "Two half-circle strips stacked"
            case .quad: i.toolTip = "Four virtual cameras looking four ways"
            case .ptz: i.toolTip = "One virtual camera: drag to aim, scroll or pinch to zoom (nothing is sent to the camera)"
            }
            sub.addItem(i)
        }
        let item = NSMenuItem(title: "Fisheye View", action: nil, keyEquivalent: "")
        item.submenu = sub
        return item
    }

    /// Fisheye Mount ▸ Ceiling, Wall, Desk (per camera).
    func mountMenuItem(camera: CameraRef) -> NSMenuItem {
        let current = model.cameraOptions(camera).fisheyeMount
        let sub = NSMenu()
        for m in FisheyeMount.allCases {
            let i = ClosureMenuItem(title: m.title) { [weak self] in
                self?.model.setFisheyeMount(camera, m)
                self?.lastKey = nil
                self?.refreshChrome()
            }
            i.state = m == current ? .on : .off
            sub.addItem(i)
        }
        let item = NSMenuItem(title: "Fisheye Mount", action: nil, keyEquivalent: "")
        item.submenu = sub
        return item
    }

    // MARK: Editing

    func edit(_ change: (LayoutNode) -> LayoutNode?) {
        guard let tree = change(state.tree) else { return }
        state.tree = tree
        reload()
        delegate?.stage(self, didEdit: state.tree)
    }

    func split(_ axis: SplitAxis) {
        guard !fixedTile, let path = state.selected ?? (frames.leaves.count == 1 ? [] : nil),
            let (tree, new) = state.tree.splitting(at: path, axis: axis)
        else { return }
        state.selected = new
        edit { _ in tree }
    }

    func removeSelected() {
        guard !fixedTile, let path = state.selected, let (tree, survivor) = state.tree.removing(at: path) else { return }
        state.selected = tree[survivor]?.isLeaf == true ? survivor : nil
        edit { _ in tree }
    }

    /// Shows `camera` in the tile at `path`; a camera new to the tile
    /// starts at the default view (1x, centered, following the window).
    func assign(_ camera: CameraRef?, at path: NodePath) {
        guard !fixedTile else { return }
        edit { $0.assigning(camera, at: path) }
    }

    /// Swaps the cameras of two tiles; each view goes with its camera (the
    /// tiles keep their identities, members and places).
    func swapTiles(_ from: NodePath, _ to: NodePath) {
        guard !fixedTile, from != to, let tree = state.tree.swapping(from, to) else { return }
        state.views.swapViews(layout: state.viewsKey, tree: state.tree, from, to)
        state.selected = to
        edit { _ in tree }
    }

    /// The selected tile's camera (Listen menu, keyboard).
    var selectedCamera: CameraRef? {
        if let f = state.focused { return state.tree[f]?.camera }
        if let s = state.selected { return state.tree[s]?.camera }
        return frames.leaves.count == 1 ? frames.leaves.first?.camera : nil
    }

    // MARK: Pointer (from the overlay)

    /// A plain drag on the picture of a tile zoomed past 1x, or of a
    /// filled tile whose picture is partly hidden, pans the picture; on
    /// other tiles, on a tile's shown header, or while editing the layout,
    /// it drags the tile to swap (docs/UX.md "Digital zoom", "Fill Tiles").
    private func panGesture(at p: CGPoint) -> Gesture? {
        guard !state.editing, let leaf = drawnLeaf(at: p), !onShownHeader(p, leaf.path) else { return nil }
        if fisheyeMode(leaf.path) != nil { return .aim(leaf.path, last: p) }
        guard let key = state.viewKey(leaf.path), picturePans(leaf) else { return nil }
        return .pan(key, last: p)
    }

    /// Whether `p` is on the shown hover header of the tile at `path`
    /// (where a drag swaps tiles).
    private func onShownHeader(_ p: CGPoint, _ path: NodePath) -> Bool {
        chromes[path]?.shownHeaderFrame?.contains(p) == true
    }

    /// What a plain drag starting at `p` does (self-test: the same choice
    /// the pointer makes): "pan", "aim", "swap" or nil.
    func dragKind(at p: CGPoint) -> String? {
        switch panGesture(at: p) {
        case .pan?: return "pan"
        case .aim?: return "aim"
        default: return !fixedTile && state.focused == nil && frames.leaf(at: p) != nil ? "swap" : nil
        }
    }

    /// Drags from `from` to `to` as the pointer would (self-test): the
    /// same mouse down, dragged and up handlers, in `steps` moves.
    func simulateDrag(from: CGPoint, to: CGPoint, steps: Int = 4) {
        mouseDown(at: from, clickCount: 1)
        let n = max(steps, 1)
        for i in 1...n {
            let k = CGFloat(i) / CGFloat(n)
            mouseDragged(to: CGPoint(x: from.x + (to.x - from.x) * k, y: from.y + (to.y - from.y) * k))
        }
        mouseUp(at: to)
    }

    fileprivate func mouseDown(at p: CGPoint, clickCount: Int) {
        window?.makeFirstResponder(self)
        if fixedTile {
            // One tile: a drag pans or aims it; nothing else moves.
            gesture = panGesture(at: p)
            return
        }
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
        case .pan(let key, let last)?:
            gesture = .pan(key, last: p)
            NSCursor.closedHand.set()
            // Measured in the tile where the drag began, wherever it goes.
            guard let leaf = drawnLeaf(of: key) else { return }
            pan(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: CGVector(dx: p.x - last.x, dy: p.y - last.y))
        case .aim(let path, let last)?:
            gesture = .aim(path, last: p)
            guard let leaf = currentLeaves.last(where: { $0.path == path }) else { return }
            turnFisheye(path, by: CGVector(dx: p.x - last.x, dy: p.y - last.y), size: leaf.rect.size)
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
            swapTiles(from, to)
        case .aim?:
            saveFisheye()
        case .pan?:
            window?.invalidateCursorRects(for: overlay)
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
        // An open hand over a picture a drag pans, below the header's
        // strip (a drag there swaps the tile).
        var rects: [(CGRect, NSCursor)] = []
        if !state.editing {
            for leaf in currentLeaves where picturePans(leaf) {
                let strip = min(Self.headerDragHeight, leaf.rect.height)
                rects.append((leaf.rect.divided(atDistance: strip, from: .minYEdge).remainder, NSCursor.openHand))
            }
        }
        guard state.focused == nil else { return rects }
        return rects
            + frames.dividers.map { d in
                (d.hitRect(minimum: 8), d.axis == .row ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown)
            }
    }

    /// The height of a tile's header strip, where the pointer stays an
    /// arrow (a drag there swaps the tile).
    static let headerDragHeight: CGFloat = 40

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
        guard !fixedTile, camera != nil, state.focused == nil, let leaf = frames.leaf(at: p) else {
            setDropTarget(nil)
            return []
        }
        setDropTarget(leaf.path)
        return .copy
    }

    fileprivate func performDrop(at p: CGPoint, camera: CameraRef?) -> Bool {
        defer { setDropTarget(nil) }
        guard !fixedTile, let camera, state.focused == nil, let leaf = frames.leaf(at: p) else { return false }
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
            if handleInsetKey(.dismiss) { return }
            if state.focused != nil {
                unfocus()
            } else if state.editing {
                delegate?.stageDidChangeState(self)
                NSApp.sendAction(#selector(ViewerWindowController.toggleEditing(_:)), to: nil, from: self)
            }
        case 51, 117:  // Delete, Forward Delete
            if state.editing, !fixedTile { removeSelected() } else { super.keyDown(with: event) }
        default:
            super.keyDown(with: event)
        }
    }

    /// Quality ▸ Auto, Clear, Fluent, and Balanced where the camera has it.
    func qualityMenuItem(camera: CameraRef, path: NodePath) -> NSMenuItem {
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

    /// The right-click menu: the tile's actions from the header model
    /// (`TileHeaderLayout.menu(for:)`, the header's order and wording),
    /// then, in a viewer window, the layout's own items.
    fileprivate func menu(at p: CGPoint) -> NSMenu? {
        // A full-screen (focused) tile gets its menu too: the picture and
        // camera items (Quality, Fisheye View, Fill, Save Picture, ...),
        // with Exit Full Screen in place of the layout edits.
        guard let leaf = state.focused == nil ? frames.leaf(at: p) : drawnLeaf(at: p) else { return nil }
        state.selected = leaf.path
        refreshChrome()
        return contextMenu(for: leaf)
    }

    /// The right-click menu of a tile (also for the self-test).
    func contextMenu(for leaf: LeafFrame) -> NSMenu {
        let menu = NSMenu()
        func item(_ title: String, _ action: @escaping () -> Void) {
            menu.addItem(ClosureMenuItem(title: title, action: action))
        }
        if let camera = leaf.camera, let tile = headerTile(camera, path: leaf.path) {
            for entry in TileHeaderLayout.menu(for: tile) {
                menu.addItem(menuItem(entry, camera: camera, path: leaf.path))
            }
            if fixedTile { return menu }
            menu.addItem(.separator())
            if state.focused != nil {
                item("Exit Full Screen") { [weak self] in self?.unfocus() }
                return menu
            }
            item("Focus") { [weak self] in self?.focus(leaf.path) }
        }
        if fixedTile || state.focused != nil { return menu }
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
