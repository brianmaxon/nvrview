import CoreGraphics
import Foundation

/// The recording player's picture settings (docs/UX.md "Recordings", "Digital
/// zoom"): per camera in the player, its digital zoom (or a filled picture's
/// crop), its own Fill choice, and its fisheye view and the virtual camera's
/// aim. The rules are the live tiles' own (`TileViews`, `TileZoom`,
/// `FisheyeAim`); this adds only what a player needs on top:
///
/// - The state belongs to the player's tile, not to the playback: play,
///   pause, seek, speed changes, another incident of the same cameras and the
///   switch between the grid and the large or single view all keep it.
/// - A camera that leaves the player (another set of cameras, Remove from
///   Incident) forgets its view (`retain`), so a tile whose camera changed
///   starts at 1x, centered, in the Original fisheye view.
/// - It never reaches the device, a download, or an export: it is state of
///   the picture on screen, and nothing else reads it.
///
/// Pure; Windows has the same type (`PlayerTileViews` in NvrView.Shell) with
/// the same tests.
public struct PlayerTileViews: Equatable, Sendable {
    public private(set) var views = TileViews<RecordingCamera>()
    public private(set) var fisheye: [RecordingCamera: FisheyeTileSetting] = [:]

    public init() {}

    /// Nothing is kept.
    public var isEmpty: Bool { views.count == 0 && fisheye.isEmpty }

    // MARK: Digital zoom and Fill

    public func zoom(_ camera: RecordingCamera) -> TileZoom { views.zoom(camera) }
    public func fills(_ camera: RecordingCamera) -> Bool { views.fills(camera) }

    public mutating func setZoom(_ zoom: TileZoom, for camera: RecordingCamera) { views.setZoom(zoom, for: camera) }

    /// Zoomed by `factor` around the unit point `unit` of the tile, which
    /// stays put (the picture sits in the tile as `base` says).
    public mutating func zoom(_ camera: RecordingCamera, by factor: Double, atUnit unit: CGPoint, base: ZoomBase) {
        views.setZoom(views.zoom(camera).zoomed(by: factor, atUnit: unit, in: base), for: camera)
    }

    /// Whether a drag moves the picture (zoomed in, or filled with part hidden).
    public func canPan(_ camera: RecordingCamera, base: ZoomBase) -> Bool { views.zoom(camera).canPan(in: base) }

    /// The picture follows a drag of `delta` (fractions of the tile).
    public mutating func pan(_ camera: RecordingCamera, byUnit delta: CGVector, base: ZoomBase) {
        guard canPan(camera, base: base) else { return }
        views.setZoom(views.zoom(camera).panned(byUnit: delta, in: base), for: camera)
    }

    /// 0: back to 1x and a filled crop to the middle; a dewarped fisheye
    /// view back to its default direction and field of view.
    public mutating func reset(_ camera: RecordingCamera) {
        if fisheyeDewarps(camera) {
            resetFisheye(camera)
        } else {
            views.setZoom(.none, for: camera)
        }
    }

    public mutating func setFill(_ fills: Bool, for camera: RecordingCamera) { views.setFill(fills, for: camera) }

    // MARK: Fisheye

    public func fisheyeSetting(_ camera: RecordingCamera) -> FisheyeTileSetting { fisheye[camera] ?? .original }

    /// Whether the camera shows a dewarped view now.
    public func fisheyeDewarps(_ camera: RecordingCamera) -> Bool { fisheyeSetting(camera).mode.dewarps }

    public mutating func setFisheyeMode(_ mode: FisheyeViewMode, for camera: RecordingCamera) {
        put(FisheyeTileSetting(mode: mode, aim: fisheye[camera]?.aim), for: camera)
    }

    /// A drag of `delta` points in a view `size` points big turns the view.
    public mutating func turnFisheye(_ camera: RecordingCamera, by delta: CGVector, size: CGSize, mount: FisheyeMount) {
        let s = fisheyeSetting(camera)
        guard s.mode.dewarps else { return }
        let aim = s.aim(for: mount).dragged(by: delta, viewSize: size, mode: s.mode, mount: mount)
        put(FisheyeTileSetting(mode: s.mode, aim: aim), for: camera)
    }

    /// The wheel or a pinch: Virtual PTZ zooms (more than 1 zooms in).
    public mutating func zoomFisheye(_ camera: RecordingCamera, by factor: Double, mount: FisheyeMount) {
        let s = fisheyeSetting(camera)
        guard s.mode == .ptz else { return }
        put(FisheyeTileSetting(mode: s.mode, aim: s.aim(for: mount).zoomed(by: factor, mount: mount)), for: camera)
    }

    public mutating func resetFisheye(_ camera: RecordingCamera) {
        put(FisheyeTileSetting(mode: fisheyeSetting(camera).mode, aim: nil), for: camera)
    }

    private mutating func put(_ setting: FisheyeTileSetting, for camera: RecordingCamera) {
        fisheye[camera] = setting == .original ? nil : setting
    }

    // MARK: Which cameras

    /// Keeps the views of `cameras` only: a camera that is no longer in the
    /// player starts again at 1x when it comes back.
    public mutating func retain(_ cameras: [RecordingCamera]) {
        let keep = Set(cameras)
        views.removeAll { !keep.contains($0) }
        fisheye = fisheye.filter { keep.contains($0.key) }
    }

    /// Forgets everything (the player closed).
    public mutating func removeAll() { self = PlayerTileViews() }

    // MARK: What the header offers

    /// Zoomed past 1x, or a moved virtual camera (Reset Zoom applies).
    public func isZoomed(_ camera: RecordingCamera) -> Bool {
        if fisheyeDewarps(camera) { return fisheyeSetting(camera).aim != nil }
        return views.zoom(camera).isZoomed
    }

    /// A filled picture's crop moved at 1x (Reset Position applies).
    public func isPanned(_ camera: RecordingCamera) -> Bool {
        !fisheyeDewarps(camera) && views.fills(camera) && views.zoom(camera).isPanned
    }
}
