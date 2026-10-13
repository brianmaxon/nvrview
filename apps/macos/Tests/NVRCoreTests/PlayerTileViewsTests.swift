import CoreGraphics
import Foundation
import NVRCore
import Testing

/// The recording player's picture settings (docs/UX.md "Recordings"): they
/// belong to the player's tile, so a seek, a pause, a speed change or a
/// change of layout keeps them, another camera starts fresh, and nothing the
/// player downloads or exports reads them. Windows has the same tests
/// (PlayerTileViewsTests.cs).
@Suite struct PlayerTileViewsTests {
    private let a = RecordingCamera(device: "dev", channel: 0)
    private let b = RecordingCamera(device: "dev", channel: 1)
    private let c = RecordingCamera(device: "dev", channel: 2)
    /// A 16:9 picture fitted in a 16:9 tile.
    private let base = ZoomBase.whole

    @Test func zoomKeepsThePointUnderThePointer() {
        var v = PlayerTileViews()
        let unit = CGPoint(x: 0.25, y: 0.75)
        let before = v.zoom(a).picturePoint(atUnit: unit, in: base)
        v.zoom(a, by: 2, atUnit: unit, base: base)
        #expect(v.zoom(a).scale == 2)
        let after = v.zoom(a).picturePoint(atUnit: unit, in: base)
        #expect(abs(after.x - before.x) < 1e-9 && abs(after.y - before.y) < 1e-9)
        #expect(v.isZoomed(a))
        #expect(!v.isZoomed(b))
    }

    @Test func theMathIsTheLiveTilesOwn() {
        var v = PlayerTileViews()
        v.zoom(a, by: 3, atUnit: CGPoint(x: 0.9, y: 0.1), base: base)
        v.pan(a, byUnit: CGVector(dx: -0.1, dy: 0.05), base: base)
        let live = TileZoom.none.zoomed(by: 3, atUnit: CGPoint(x: 0.9, y: 0.1), in: base)
            .panned(byUnit: CGVector(dx: -0.1, dy: 0.05), in: base)
        #expect(v.zoom(a) == live)
    }

    @Test func panStopsAtTheEdgesAndNeedsAZoom() {
        var v = PlayerTileViews()
        v.pan(a, byUnit: CGVector(dx: 0.3, dy: 0.3), base: base)
        #expect(v.zoom(a) == .none, "a drag at 1x moves nothing")
        v.zoom(a, by: 2, atUnit: CGPoint(x: 0.5, y: 0.5), base: base)
        v.pan(a, byUnit: CGVector(dx: 5, dy: 5), base: base)
        let visible = v.zoom(a).visible(in: base)
        #expect(visible.minX >= -1e-9 && visible.minY >= -1e-9 && visible.maxX <= 1 + 1e-9 && visible.maxY <= 1 + 1e-9)
    }

    @Test func eachCameraHasItsOwnView() {
        var v = PlayerTileViews()
        v.zoom(a, by: 4, atUnit: CGPoint(x: 0.2, y: 0.2), base: base)
        v.setFill(true, for: b)
        #expect(v.zoom(b) == .none)
        #expect(v.fills(b) && !v.fills(a))
        #expect(v.zoom(a).scale == 4)
    }

    @Test func aSeekOrALayoutChangeKeepsTheView() {
        // The view is state of the player's tile: nothing about the
        // playback (position, rate, the grid or single mode) is an input.
        var v = PlayerTileViews()
        v.zoom(a, by: 2.5, atUnit: CGPoint(x: 0.3, y: 0.6), base: base)
        v.setFisheyeMode(.ptz, for: b)
        let kept = v
        // The cameras playing stay the same through any number of seeks and
        // layout changes: retaining them changes nothing.
        v.retain([a, b])
        v.retain([b, a])
        #expect(v == kept)
        // A second incident of the same cameras keeps the view too.
        v.retain([a, b, c])
        #expect(v == kept)
    }

    @Test func aCameraThatLeavesStartsFreshWhenItComesBack() {
        var v = PlayerTileViews()
        v.zoom(a, by: 2, atUnit: CGPoint(x: 0.5, y: 0.5), base: base)
        v.setFill(true, for: a)
        v.setFisheyeMode(.panorama, for: a)
        v.zoom(b, by: 2, atUnit: CGPoint(x: 0.5, y: 0.5), base: base)
        v.retain([b])
        #expect(v.zoom(a) == .none && !v.fills(a) && v.fisheyeSetting(a) == .original)
        #expect(v.zoom(b).scale == 2, "the camera that stayed keeps its view")
        v.retain([])
        #expect(v.isEmpty)
    }

    @Test func resetBringsBackOneXAndTheCrop() {
        var v = PlayerTileViews()
        let wide = ZoomBase(picture: CGSize(width: 16, height: 9), tile: CGSize(width: 1, height: 1), fill: true)
        v.setFill(true, for: a)
        v.pan(a, byUnit: CGVector(dx: -0.1, dy: 0), base: wide)
        #expect(v.isPanned(a) && !v.isZoomed(a))
        v.reset(a)
        #expect(v.zoom(a).isDefault && !v.isPanned(a))
        v.zoom(a, by: 3, atUnit: CGPoint(x: 0.1, y: 0.9), base: base)
        v.reset(a)
        #expect(v.zoom(a) == .none)
    }

    @Test func fillIsPerCamera() {
        var v = PlayerTileViews()
        v.setFill(true, for: a)
        #expect(v.fills(a) && !v.fills(b))
        v.setFill(false, for: a)
        #expect(!v.fills(a) && v.isEmpty)
    }

    @Test func aFisheyeViewTurnsAndZoomsLikeALiveTile() {
        var v = PlayerTileViews()
        let size = CGSize(width: 400, height: 300)
        v.turnFisheye(a, by: CGVector(dx: 40, dy: 0), size: size, mount: .ceiling)
        #expect(v.fisheyeSetting(a) == .original, "Original has nothing to turn")
        v.setFisheyeMode(.ptz, for: a)
        v.turnFisheye(a, by: CGVector(dx: 40, dy: -20), size: size, mount: .ceiling)
        let want = FisheyeAim.default(for: .ceiling).dragged(
            by: CGVector(dx: 40, dy: -20), viewSize: size, mode: .ptz, mount: .ceiling)
        #expect(v.fisheyeSetting(a).aim == want)
        #expect(v.isZoomed(a), "a moved virtual camera has Reset Zoom")
        v.zoomFisheye(a, by: 2, mount: .ceiling)
        #expect(v.fisheyeSetting(a).aim?.fov == want.zoomed(by: 2, mount: .ceiling).fov)
        // The wheel does nothing in the panorama.
        v.setFisheyeMode(.panorama, for: b)
        v.zoomFisheye(b, by: 2, mount: .ceiling)
        #expect(v.fisheyeSetting(b).aim == nil)
        v.reset(a)
        #expect(v.fisheyeSetting(a) == FisheyeTileSetting(mode: .ptz, aim: nil), "back to the default view, same mode")
        #expect(!v.isZoomed(a))
        // Digital zoom and the dewarped view do not mix.
        v.setZoom(TileZoom(scale: 2, center: CGPoint(x: 0.5, y: 0.5)), for: a)
        #expect(!v.isZoomed(a), "the dewarped view has no digital zoom")
        v.setFisheyeMode(.original, for: a)
        #expect(v.isZoomed(a))
    }

    @Test func exportsNeverReadTheView() {
        // The requests the player's downloads and exports build take no view
        // state, and carry no region: they stay full frame.
        var v = PlayerTileViews()
        let options = ExportOptions(shareSafe: true)
        let range = ExportRequest.range(startMs: 1_000, endMs: 9_000, cameras: [a, b], name: nil, options: options)
        let stills = ExportRequest.stills(playback: 7, tiles: [1, 2], format: .png, options: options)
        v.zoom(a, by: 4, atUnit: CGPoint(x: 0.1, y: 0.1), base: base)
        v.setFisheyeMode(.quad, for: a)
        v.setFill(true, for: b)
        #expect(range == ExportRequest.range(startMs: 1_000, endMs: 9_000, cameras: [a, b], name: nil, options: options))
        #expect(stills == ExportRequest.stills(playback: 7, tiles: [1, 2], format: .png, options: options))
        for json in [range, stills] {
            for word in ["zoom", "crop", "region", "scale", "fisheye"] { #expect(!json.contains(word)) }
        }
    }
}
