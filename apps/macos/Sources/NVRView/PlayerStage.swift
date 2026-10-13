import AppKit
import NVRCore
import SwiftUI
import VideoKit

/// Draws the player's cameras with the app's Metal grid renderer, the one
/// live view uses: each cell shows its `CoreTile`'s newest decoded picture
/// (VideoToolbox), fitted to the cell with the picture's own geometry and
/// color (`PictureMapping`, `TilePlacement`), so recorded and live pictures
/// look the same. The SwiftUI layer above handles clicks and labels.
///
/// The cell's picture settings (`PlayerTileViews`: digital zoom, Fill, the
/// fisheye view) change only the rectangles the renderer draws and samples,
/// through the same `TilePlacement` and `FisheyeDewarp` the live tiles use:
/// no extra decoding, copies, or stream changes, so the playback's pacing is
/// untouched (docs/UX.md "Recordings"). Playback streams are fixed per play,
/// so the live tiles' zoom-driven quality switch has nothing to do here.
@MainActor
final class PlayerStageView: NSView {
    struct Item: Equatable {
        let tile: CoreTile
        /// The cell in points, top-left origin.
        var rect: CGRect
        var camera: RecordingCamera
        static func == (a: Item, b: Item) -> Bool { a.tile === b.tile && a.rect == b.rect && a.camera == b.camera }
    }

    let grid: MetalGridView?
    weak var player: RecordingsPlayer?
    var items: [Item] = [] {
        didSet { if items != oldValue { lastKey = nil } }
    }

    /// The pictures' layout sizes by camera, as drawn last (the zoom's base).
    private(set) var pictureSizes: [RecordingCamera: CGSize] = [:]
    /// The source and destination the renderer was given last, by camera
    /// (the self-test checks them against the zoom).
    private var rendered: [RecordingCamera: (source: CGRect?, destination: CGRect, dewarp: DewarpConstants?)] = [:]
    private var insets: [RecordingCamera: ZoomInsetView] = [:]
    private var zoomEasing = ZoomEasing<RecordingCamera>()
    private var fisheyeEasing = FisheyeZoomEasing<RecordingCamera>()
    /// The clock the easing runs on (the self-test sets its own).
    var now: () -> Double = { ProcessInfo.processInfo.systemUptime }
    private var monitor: Any?

    private struct RenderKey: Equatable {
        var tiles: [ObjectIdentifier]
        var rects: [CGRect]
        var versions: [UInt64]
        var sizes: [CGSize]
        var zooms: [TileZoom]
        var fills: [Bool]
        var dewarps: [DewarpConstants?]
    }

    private var lastKey: RenderKey?

    override init(frame: NSRect) {
        // A view without Metal (a machine without a GPU) stays black.
        grid = (try? GridRenderer()).map(MetalGridView.init(renderer:))
        super.init(frame: frame)
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        if let grid {
            grid.frame = bounds
            grid.autoresizingMask = [.width, .height]
            addSubview(grid)
            grid.prepare = { [weak self] renderer, drawable in
                MainActor.assumeIsolated { self?.prepare(renderer: renderer, drawable: drawable) }
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    isolated deinit {
        if let monitor { NSEvent.removeMonitor(monitor) }
    }

    /// Frames drawn so far (the self-test checks pictures reach the screen).
    var framesDrawn: Int { grid?.framesDrawn ?? 0 }

    // MARK: Drawing

    private var views: PlayerTileViews { player?.tileViews ?? PlayerTileViews() }

    private func ref(_ camera: RecordingCamera) -> CameraRef {
        CameraRef(device: camera.device, channel: camera.channel, lens: camera.lens)
    }

    /// Whether the camera is a fisheye camera the app can dewarp.
    func isFisheye(_ camera: RecordingCamera) -> Bool { player?.app?.isFisheye(ref(camera)) == true }

    func mount(_ camera: RecordingCamera) -> FisheyeMount { player?.app?.cameraOptions(ref(camera)).fisheyeMount ?? .ceiling }

    /// The dewarp of the camera's view drawn `tileSize` pixels big, or nil
    /// when it draws its picture as is (not a fisheye camera, Original, or
    /// the circle is not known yet).
    func dewarp(_ camera: RecordingCamera, picture: DecodedPicture?, tileSize: CGSize) -> FisheyeDewarp? {
        let setting = views.fisheyeSetting(camera)
        guard setting.mode.dewarps, let app = player?.app, app.isFisheye(ref(camera)),
            let lens = app.fisheyeLens(ref(camera), picture: picture)
        else { return nil }
        let mount = mount(camera)
        return FisheyeDewarp(lens: lens, mount: mount, mode: setting.mode, aim: setting.aim(for: mount), tileSize: tileSize)
    }

    private func prepare(renderer: GridRenderer, drawable: CGSize) {
        tick(now: now())
        let points = bounds
        guard points.width > 0, points.height > 0 else { return }
        let scaleX = drawable.width / points.width, scaleY = drawable.height / points.height
        let views = views
        var key = RenderKey(tiles: [], rects: [], versions: [], sizes: [], zooms: [], fills: [], dewarps: [])
        var tiles: [GridTile] = []
        var drawn: [RecordingCamera: (source: CGRect?, destination: CGRect, dewarp: DewarpConstants?)] = [:]
        for item in items {
            let snapshot = item.tile.snapshot
            let picture = item.tile.latestPicture()
            let size = picture.map { PixelSize(width: UInt32($0.width), height: UInt32($0.height)) }
            let mapping = PictureMapping(snapshot: snapshot, pictureSize: size)
            let pictureSize = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
            if pictureSize != .zero { pictureSizes[item.camera] = pictureSize }
            let matrix = snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            let zoom = views.zoom(item.camera), fill = views.fills(item.camera)
            let cell = item.rect.pixels(scaleX: scaleX, scaleY: scaleY)
            let placed = TilePlacement.place(
                rect: item.rect, scaleX: scaleX, scaleY: scaleY, picture: pictureSize,
                offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)), fill: fill, zoom: zoom)
            // A dewarped fisheye cell fills its whole cell through the dewarp
            // pipeline (docs/RENDERING.md "Fisheye dewarping").
            let dewarp = dewarp(item.camera, picture: picture, tileSize: cell.size)
            if let dewarp {
                tiles.append(GridTile(picture: item.tile, color: matrix, destination: cell, dewarp: dewarp.constants))
            } else {
                tiles.append(
                    GridTile(picture: item.tile, color: matrix, destination: placed.destination, source: placed.source))
            }
            drawn[item.camera] = (dewarp == nil ? placed.source : nil, tiles.last!.destination, tiles.last?.dewarp)
            key.dewarps.append(tiles.last?.dewarp)
            key.tiles.append(ObjectIdentifier(item.tile))
            key.rects.append(cell)
            key.versions.append(snapshot.version)
            key.sizes.append(pictureSize)
            key.zooms.append(zoom)
            key.fills.append(fill)
        }
        if key != lastKey || renderer.tiles.count != tiles.count {
            if key.zooms != lastKey?.zooms || key.sizes != lastKey?.sizes || key.fills != lastKey?.fills
                || key.rects != lastKey?.rects || key.dewarps != lastKey?.dewarps
            {
                layoutInsets(views)
            }
            lastKey = key
            renderer.tiles = tiles
            rendered = drawn
        }
    }

    // MARK: Zoom maps

    /// The zoom maps: shown while a cell is zoomed (not in a dewarped view),
    /// in the bottom left corner of the picture as drawn.
    private func layoutInsets(_ views: PlayerTileViews) {
        var live = Set<RecordingCamera>()
        for item in items {
            let z = views.zoom(item.camera)
            guard z.isZoomed, !views.fisheyeDewarps(item.camera), let picture = pictureSizes[item.camera] else { continue }
            live.insert(item.camera)
            let inset: ZoomInsetView
            if let existing = insets[item.camera] {
                inset = existing
            } else {
                inset = ZoomInsetView()
                insets[item.camera] = inset
                addSubview(inset)
            }
            let fill = views.fills(item.camera)
            let shown = TilePlacement.shownRect(rect: item.rect, picture: picture, fill: fill, zoom: z)
                .intersection(item.rect)
            inset.frame = ZoomInsetView.frame(inPicture: shown, aspect: picture.width / picture.height)
            inset.update(region: z.visible(in: ZoomBase(picture: picture, tile: item.rect.size, fill: fill)))
            inset.setShown(true)
        }
        for (camera, inset) in insets where !live.contains(camera) { inset.setShown(false) }
    }

    /// Whether a cell's zoom map is shown (self-test).
    func zoomMapShown(_ camera: RecordingCamera) -> Bool { insets[camera]?.isShown == true }

    /// The source rectangle the renderer samples for the camera's cell, in
    /// buffer pixels, and where it draws it (self-test).
    func renderedSource(of camera: RecordingCamera) -> CGRect? { rendered[camera]?.source }
    func renderedDestination(of camera: RecordingCamera) -> CGRect? { rendered[camera]?.destination }
    func renderedDewarp(of camera: RecordingCamera) -> DewarpConstants? { rendered[camera]?.dewarp }

    /// Draws once now, as the display would (self-test).
    func drawNow() { if let grid { grid.prepare?(grid.renderer, grid.drawableSize) } }

    // MARK: Digital zoom (docs/UX.md "Digital zoom")

    /// The cell under `p`.
    func item(at p: CGPoint) -> Item? { items.last { $0.rect.contains(p) } }

    /// How the camera's picture sits in its cell at 1x.
    func zoomBase(_ item: Item) -> ZoomBase {
        guard let picture = pictureSizes[item.camera] else { return .whole }
        return ZoomBase(picture: picture, tile: item.rect.size, fill: views.fills(item.camera))
    }

    private func currentBase(_ camera: RecordingCamera) -> ZoomBase {
        guard let item = items.first(where: { $0.camera == camera }) else { return .whole }
        return zoomBase(item)
    }

    /// The part of the picture a cell shows now, in buffer pixels (what Save
    /// Zoomed Area saves).
    func visibleRegion(of camera: RecordingCamera) -> PixelRegion? {
        guard let item = items.first(where: { $0.camera == camera }), let tile = player?.cells.first(where: { $0.camera == camera })?.tile,
            let picture = tile.latestPicture()
        else { return nil }
        let mapping = PictureMapping(
            snapshot: tile.snapshot, pictureSize: PixelSize(width: UInt32(picture.width), height: UInt32(picture.height)))
        return PictureRegion.visible(
            layoutSize: CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height)),
            offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)), tile: item.rect.size,
            fill: views.fills(camera), zoom: views.zoom(camera))
    }

    private func edit(_ change: (inout PlayerTileViews) -> Void) {
        guard let player else { return }
        change(&player.tileViews)
    }

    /// Zooms the cell at `p` by `factor` around `p`: at once (pinch), or
    /// eased over the next refreshes (a mouse-wheel notch). A dewarped
    /// fisheye cell zooms its virtual camera instead (Virtual PTZ only).
    func zoom(at p: CGPoint, by factor: Double, eased: Bool) {
        guard let item = item(at: p) else { return }
        let camera = item.camera
        if views.fisheyeDewarps(camera) {
            let aim = views.fisheyeSetting(camera).aim(for: mount(camera))
            if eased {
                guard views.fisheyeSetting(camera).mode == .ptz else { return }
                let from = fisheyeEasing.target(for: camera) ?? aim.fov
                fisheyeEasing.set(fov: from / factor, for: camera, now: now())
            } else {
                fisheyeEasing.cancel(camera)
                edit { $0.zoomFisheye(camera, by: factor, mount: mount(camera)) }
            }
            return
        }
        let unit = TilePlacement.unit(p, in: item.rect)
        if eased {
            let from = zoomEasing.target(for: camera)?.scale ?? views.zoom(camera).scale
            let target = min(max(from * factor, 1), TileZoom.maxScale)
            zoomEasing.set(.init(scale: target, unit: unit), for: camera, now: now())
        } else {
            zoomEasing.cancel(camera)
            let base = zoomBase(item)
            edit { $0.zoom(camera, by: factor, atUnit: unit, base: base) }
        }
    }

    /// Pans the cell at `p` by `delta` points (the picture follows); a
    /// dewarped fisheye cell turns its view.
    func pan(at p: CGPoint, by delta: CGVector) {
        guard let item = item(at: p) else { return }
        pan(item, by: delta)
    }

    func pan(camera: RecordingCamera, by delta: CGVector) {
        guard let item = items.first(where: { $0.camera == camera }) else { return }
        pan(item, by: delta)
    }

    private func pan(_ item: Item, by delta: CGVector) {
        let camera = item.camera
        if views.fisheyeDewarps(camera) {
            fisheyeEasing.cancel(camera)
            edit { $0.turnFisheye(camera, by: delta, size: item.rect.size, mount: mount(camera)) }
            return
        }
        guard item.rect.width > 0, item.rect.height > 0 else { return }
        let base = zoomBase(item)
        guard views.canPan(camera, base: base) else { return }
        zoomEasing.cancel(camera)
        let unit = CGVector(dx: delta.dx / item.rect.width, dy: delta.dy / item.rect.height)
        edit { $0.pan(camera, byUnit: unit, base: base) }
    }

    /// Whether a drag on the camera's cell moves the picture (zoomed in, a
    /// filled picture with part hidden, or a dewarped fisheye view).
    func picturePans(_ camera: RecordingCamera) -> Bool {
        guard let item = items.first(where: { $0.camera == camera }) else { return false }
        return views.fisheyeDewarps(camera) || views.canPan(camera, base: zoomBase(item))
    }

    /// Back to 1x (and a filled crop to the middle), eased from a zoom; a
    /// dewarped fisheye cell back to its default view.
    func reset(_ camera: RecordingCamera, eased: Bool = true) {
        fisheyeEasing.cancel(camera)
        if views.fisheyeDewarps(camera) {
            zoomEasing.cancel(camera)
            edit { $0.resetFisheye(camera) }
            return
        }
        let z = views.zoom(camera)
        guard !z.isDefault else { return }
        guard z.isZoomed, eased else {
            zoomEasing.cancel(camera)
            edit { $0.reset(camera) }
            return
        }
        zoomEasing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5), recenter: true), for: camera, now: now())
    }

    /// Whether a mouse-wheel zoom is still easing (self-test).
    var isEasing: Bool { zoomEasing.isEasing || fisheyeEasing.isEasing }

    /// One step of the wheel easing, on `now`'s clock. Time-based, so it
    /// ends on its target however few refreshes there are.
    func tick(now: Double) {
        if zoomEasing.isEasing {
            let steps = zoomEasing.tick(now: now, current: { views.zoom($0) }, base: { camera, _ in currentBase(camera) })
            for step in steps {
                guard items.contains(where: { $0.camera == step.key }) else {
                    zoomEasing.cancel(step.key)
                    continue
                }
                edit { $0.setZoom(step.zoom, for: step.key) }
            }
        }
        if fisheyeEasing.isEasing {
            let steps = fisheyeEasing.tick(now: now) { camera in
                views.fisheyeSetting(camera).aim(for: mount(camera)).fov
            }
            for step in steps {
                let camera = step.key
                let aim = views.fisheyeSetting(camera).aim(for: mount(camera))
                edit { $0.zoomFisheye(camera, by: aim.fov / max(step.fov, 1e-6), mount: mount(camera)) }
            }
        }
    }

    // MARK: Pointer input

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        guard window != nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify]) { [weak self] e in
            // Local monitors run on the main thread.
            nonisolated(unsafe) let event = e
            let used = MainActor.assumeIsolated { self?.handle(event) == true }
            return used ? nil : e
        }
    }

    /// A wheel, a two-finger scroll or a pinch over a cell, as live tiles
    /// take them: a mouse wheel zooms around the pointer, each notch 1.25x,
    /// eased; a trackpad's scroll pans (and zooms with Option held); a pinch
    /// zooms. False leaves the event to the rest of the window.
    func handle(_ e: NSEvent) -> Bool {
        guard e.window === window, window != nil else { return false }
        return handle(e, at: convert(e.locationInWindow, from: nil))
    }

    func handle(_ e: NSEvent, at p: NSPoint) -> Bool {
        guard bounds.contains(p), item(at: p) != nil else { return false }
        switch e.type {
        case .magnify:
            zoom(at: p, by: 1 + Double(e.magnification), eased: false)
        case .scrollWheel:
            if e.hasPreciseScrollingDeltas {
                if e.modifierFlags.contains(.option) {
                    zoom(at: p, by: exp(Double(e.scrollingDeltaY) * 0.01), eased: false)
                } else {
                    pan(at: p, by: CGVector(dx: e.scrollingDeltaX, dy: e.scrollingDeltaY))
                }
            } else if e.scrollingDeltaY != 0 {
                zoom(at: p, by: pow(1.25, Double(e.scrollingDeltaY)), eased: true)
            }
        default:
            return false
        }
        return true
    }

    // MARK: Saving

    /// What to draw for `kind` from the camera's cell now (Save Picture).
    func pictureRender(camera: RecordingCamera, kind: PictureKind) -> PictureRender? {
        guard let tile = player?.cells.first(where: { $0.camera == camera })?.tile else { return nil }
        let size = items.first { $0.camera == camera }?.rect.size
        return PictureRenderBuilder.make(
            tile: tile, kind: kind, tileSize: size, fill: views.fills(camera), zoom: views.zoom(camera)
        ) { picture, layout in
            self.dewarp(camera, picture: picture, tileSize: layout)
        }
    }
}

/// The player's pictures in SwiftUI.
struct PlayerStage: NSViewRepresentable {
    let items: [PlayerStageView.Item]
    let player: RecordingsPlayer
    /// Hands the view to the window (the self-test reads it).
    var onView: (PlayerStageView) -> Void = { _ in }

    func makeNSView(context: Context) -> PlayerStageView {
        let v = PlayerStageView(frame: .zero)
        v.player = player
        player.stage = v
        onView(v)
        return v
    }

    func updateNSView(_ view: PlayerStageView, context: Context) {
        view.player = player
        view.items = items
    }
}
