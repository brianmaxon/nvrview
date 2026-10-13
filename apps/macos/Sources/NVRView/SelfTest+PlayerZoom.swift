import AppKit
import FakeCore
import Foundation
import NVRCore
import VideoKit

// The recording player's picture features: digital zoom and pan, kept across
// seeks and layout changes, the region shown matching the zoom math, Save
// Picture and Save Zoomed Area, the fisheye view (the self-test's "recordings"
// steps; scripts/macos-selftest-areas.txt). Part of SelfTest (SelfTest.swift
// has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// The player's first tile is zoomed and panned with the wheel, a
    /// two-finger scroll and a pinch; the point under the pointer stays put;
    /// the part of the picture drawn is the zoom math's; a seek, a speed
    /// change and the large and single views keep the zoom; Save Zoomed Area
    /// writes the region shown (at the source's resolution) and Save Picture
    /// the whole frame; 0 resets, eased on the stage's clock; and the
    /// playback was never reopened by any of it.
    func playbackZoom(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-zoom
        let p = b.player
        guard let model = app.model, let stage = app.recordingsWindows.first?.stage, p.cells.count >= 2 else {
            return failPlayback("no player stage to zoom", b, fake)
        }
        let camera = p.cells[0].camera
        let opens = p.opens
        var clock = 100.0
        stage.now = { clock }
        whenPlayback(fake, { stage.items.count == p.cells.count && stage.pictureSizes[camera] != nil }, timeout: 3) { [self] in
            stage.drawNow()
            guard let item = stage.items.first(where: { $0.camera == camera }), let picture = stage.pictureSizes[camera] else {
                return failPlayback("the first tile has no picture size", b, fake)
            }
            func scroll(_ units: CGScrollEventUnit, dy: Int32, dx: Int32 = 0) -> NSEvent? {
                CGEvent(scrollWheelEvent2Source: nil, units: units, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0)
                    .flatMap(NSEvent.init(cgEvent:))
            }
            let at = CGPoint(x: item.rect.minX + item.rect.width * 0.7, y: item.rect.minY + item.rect.height * 0.3)
            let unit = TilePlacement.unit(at, in: item.rect)
            let base = stage.zoomBase(item)

            // A wheel notch zooms around the pointer, eased on the clock.
            guard let notch = scroll(.line, dy: 3), stage.handle(notch, at: at) else {
                return failPlayback("a wheel notch over the first tile was not taken", b, fake)
            }
            if !stage.isEasing { problems.append("player zoom: a wheel notch did not ease") }
            for _ in 0..<10 {
                clock += 0.1
                stage.tick(now: clock)
            }
            let wheeled = p.tileViews.zoom(camera)
            print("player zoom: a wheel notch zooms to \(wheeled.scale)x")
            if stage.isEasing || abs(wheeled.scale - 1.25 * 1.25 * 1.25) > 1e-6 {
                problems.append("player zoom: three notches made \(wheeled.scale)x, not \(1.25 * 1.25 * 1.25)x")
            }
            // A pinch zooms at once, and the picture point under the pointer
            // stays put on every axis the picture fills (the others stay
            // centered until it does: docs/UX.md "Digital zoom").
            let pointBefore = wheeled.picturePoint(atUnit: unit, in: base)
            stage.zoom(at: at, by: 1.6, eased: false)
            let pinched = p.tileViews.zoom(camera)
            let pointAfter = pinched.picturePoint(atUnit: unit, in: base)
            print("player zoom: a pinch zooms to \(pinched.scale)x, the point under the pointer \(pointBefore) -> \(pointAfter)")
            if abs(pinched.scale - wheeled.scale * 1.6) > 1e-6 { problems.append("player zoom: a pinch made \(pinched.scale)x") }
            if wheeled.scale * Double(base.width) > 1.0001, abs(pointAfter.x - pointBefore.x) > 1e-6 {
                problems.append("player zoom: the point under the pointer moved across from \(pointBefore) to \(pointAfter)")
            }
            if wheeled.scale * Double(base.height) > 1.0001, abs(pointAfter.y - pointBefore.y) > 1e-6 {
                problems.append("player zoom: the point under the pointer moved down from \(pointBefore) to \(pointAfter)")
            }
            guard let pan = scroll(.pixel, dy: 0, dx: -40), stage.handle(pan, at: at) else {
                return failPlayback("a two-finger scroll over a zoomed tile was not taken", b, fake)
            }
            let panned = p.tileViews.zoom(camera)
            if panned.center == pinched.center { problems.append("player zoom: a two-finger scroll did not pan") }
            stage.pan(camera: camera, by: CGVector(dx: 25, dy: -10))
            let zoom = p.tileViews.zoom(camera)
            if zoom.center == panned.center { problems.append("player zoom: a drag did not pan") }
            if !stage.picturePans(camera) { problems.append("player zoom: a zoomed tile does not take a drag") }
            stage.drawNow()
            if !stage.zoomMapShown(camera) { problems.append("player zoom: no zoom map on a zoomed tile") }
            if p.tileViews.zoom(p.cells[1].camera) != .none { problems.append("player zoom: another tile zoomed too") }

            // The part drawn is the zoom math's.
            expectRegion(stage, p, camera, "player zoom")
            let want = expectedSource(stage, p, camera)
            if let want { print("player zoom: \(zoom.scale)x shows \(want) of \(picture)") }
            if let want, want.width >= picture.width || want.height >= picture.height {
                problems.append("player zoom: the region \(want.size) is not smaller than the picture \(picture)")
            }

            // Seeks, speed and layout changes keep the view and do not reopen.
            p.pause()
            whenPlayback(fake, { p.state == .paused }, timeout: 2) { [self] in
                p.seek(to: min(p.positionMs + 3_000, p.endMs))
                p.setRate(2)
                p.setRate(1)
                p.click(0)
                p.doubleClick(0)
                whenPlayback(fake, { stage.items.count == 1 }, timeout: 3) { [self] in
                    stage.drawNow()
                    let single = p.tileViews.zoom(camera)
                    print("player zoom: after a seek, speed changes and the single view the zoom is \(single.scale)x")
                    if single != zoom { problems.append("player zoom: the view changed to \(single) from \(zoom)") }
                    if p.opens != opens { problems.append("player zoom: the playback was reopened") }
                    expectRegion(stage, p, camera, "player zoom (single view)")
                    p.mode = .grid
                    p.play()
                    whenPlayback(fake, { stage.items.count == p.cells.count && p.state == .playing }, timeout: 3) { [self] in
                        stage.drawNow()
                        if p.tileViews.zoom(camera) != zoom { problems.append("player zoom: the grid changed the view") }
                        expectRegion(stage, p, camera, "player zoom (grid)")
                        zoomSaves(b, fake, rec, stage, model, camera, zoom, clock: clock)
                    }
                }
            }
        }
    }

    /// The region of the picture the zoom math says the tile shows, in
    /// buffer pixels, derived without `PictureRegion`.
    private func expectedSource(_ stage: PlayerStageView, _ p: RecordingsPlayer, _ camera: RecordingCamera) -> CGRect? {
        guard let item = stage.items.first(where: { $0.camera == camera }), let cell = p.cells.first(where: { $0.camera == camera }),
            let decoded = cell.tile.latestPicture()
        else { return nil }
        let mapping = PictureMapping(
            snapshot: cell.tile.snapshot,
            pictureSize: PixelSize(width: UInt32(decoded.width), height: UInt32(decoded.height)))
        let picture = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
        let base = ZoomBase(picture: picture, tile: item.rect.size, fill: p.tileViews.fills(camera))
        let v = p.tileViews.zoom(camera).visible(in: base)
        return CGRect(
            x: v.minX * picture.width + CGFloat(mapping.offset.x), y: v.minY * picture.height + CGFloat(mapping.offset.y),
            width: v.width * picture.width, height: v.height * picture.height)
    }

    private func expectRegion(_ stage: PlayerStageView, _ p: RecordingsPlayer, _ camera: RecordingCamera, _ what: String) {
        guard let want = expectedSource(stage, p, camera), let got = stage.renderedSource(of: camera) else {
            problems.append("\(what): the tile drew no source rectangle")
            return
        }
        let close = abs(got.minX - want.minX) <= 1 && abs(got.minY - want.minY) <= 1 && abs(got.width - want.width) <= 1
            && abs(got.height - want.height) <= 1
        if !close { problems.append("\(what): the renderer samples \(got), the zoom says \(want)") }
    }

    /// Save Zoomed Area saves the region shown, Save Picture the whole
    /// frame; 0 resets the view, eased on the clock.
    private func zoomSaves(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ stage: PlayerStageView, _ model: AppModel,
        _ camera: RecordingCamera, _ zoom: TileZoom, clock: Double
    ) {
        let p = b.player
        let folder = freshPictureFolder(model)
        let name = PictureNaming.sanitize(
            model.name(of: CameraRef(device: camera.device, channel: camera.channel, lens: camera.lens)))
        guard let want = expectedSource(stage, p, camera), let full = stage.pictureSizes[camera] else {
            return failPlayback("no region to save", b, fake)
        }
        var t = clock
        let before = p.pictureSaves
        p.hovered = 0
        // P saves the whole frame, whatever the zoom.
        p.perform(.savePicture, at: 0)
        when({ p.pictureSaves > before }, timeout: 8) { [self] in
            let record = p.pictureSaves > before ? p.lastPictureSave : nil
            checkSaved(
                record, "player picture", ext: "png", nameEnds: " " + name, size: full, tolerance: 3,
                confirmation: (p.toast?.text, p.toast?.actionTitle))
            let again = p.pictureSaves
            p.perform(.saveZoomedArea, at: 0)
            when({ p.pictureSaves > again }, timeout: 8) { [self] in
                let record = p.pictureSaves > again ? p.lastPictureSave : nil
                checkSaved(
                    record, "player zoomed area", ext: "png", nameEnds: " " + name, size: want.size, tolerance: 3,
                    confirmation: (p.toast?.text, p.toast?.actionTitle))
                if let r = record?.region, abs(r.minX - want.minX) > 1 || abs(r.minY - want.minY) > 1 {
                    problems.append("player zoomed area: the region \(r) is not the one shown, \(want)")
                }
                // 0 resets the view, eased.
                p.resetSelectedView()
                for _ in 0..<40 {
                    t += 0.1
                    stage.tick(now: t)
                }
                stage.drawNow()
                if p.tileViews.zoom(camera) != .none || stage.zoomMapShown(camera) {
                    problems.append("player zoom: 0 left \(p.tileViews.zoom(camera))")
                }
                try? FileManager.default.removeItem(at: folder)
                stage.now = { ProcessInfo.processInfo.systemUptime }
                playbackFill(b, fake, rec, stage, model, camera)
            }
        }
    }

    /// Fill Tile crops the picture to the tile and a drag moves the crop;
    /// Reset Position brings it back; the header model offers the right
    /// items for the tile.
    private func playbackFill(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ stage: PlayerStageView, _ model: AppModel,
        _ camera: RecordingCamera
    ) {
        let p = b.player
        guard let index = p.index(of: camera) else { return failPlayback("the camera left the player", b, fake) }
        // The header model: picture items only.
        let plain = TileHeaderLayout.menu(for: p.headerTile(index)).map(\.item)
        let allowed = TileHeaderLayout.playbackItems
        if plain.contains(where: { !allowed.contains($0) }) || !plain.contains(.savePicture) || !plain.contains(.fillTile) {
            problems.append("player header: the menu holds \(plain)")
        }
        if plain.contains(.resetZoom) || plain.contains(.saveZoomedArea) {
            problems.append("player header: Reset Zoom or Save Zoomed Area at 1x")
        }
        p.perform(.fillTile, at: index)
        stage.drawNow()
        guard let item = stage.items.first(where: { $0.camera == camera }), let picture = stage.pictureSizes[camera] else {
            return failPlayback("no picture to fill", b, fake)
        }
        let shape = picture.width / picture.height != item.rect.width / item.rect.height
        if !p.tileViews.fills(camera) { problems.append("player fill: Fill Tile did not fill") }
        if shape {
            let base = stage.zoomBase(item)
            if !p.tileViews.canPan(camera, base: base) { problems.append("player fill: a filled picture of another shape cannot be moved") }
            stage.pan(camera: camera, by: CGVector(dx: 20, dy: 20))
            stage.drawNow()
            if !p.tileViews.isPanned(camera) { problems.append("player fill: a drag did not move the crop") }
            expectRegion(stage, p, camera, "player fill")
            let withCrop = TileHeaderLayout.menu(for: p.headerTile(index)).map(\.item)
            if !withCrop.contains(.resetPosition) { problems.append("player fill: no Reset Position for a moved crop") }
            p.perform(.resetPosition, at: index)
            stage.reset(camera, eased: false)
            if p.tileViews.zoom(camera) != .none { problems.append("player fill: Reset Position left the crop at \(p.tileViews.zoom(camera))") }
        } else {
            print("player fill: the picture has the tile's shape; no crop to move")
        }
        p.perform(.fillTile, at: index)
        if p.tileViews.fills(camera) { problems.append("player fill: Fill Tile again did not fit") }
        playbackFisheye(b, fake, rec, stage, model)
    }

    /// A fisheye camera joins the player: Panorama dewarps through the same
    /// pipeline as a live tile, a drag turns the view, 0 resets it, and the
    /// saved picture is the dewarped view (Save Original Fisheye Picture the
    /// circle). The camera is taken out again afterwards, so the steps after
    /// this one see the incident's cameras.
    private func playbackFisheye(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ stage: PlayerStageView, _ model: AppModel
    ) {
        let p = b.player
        guard let ref = model.allCameras.first(where: { model.isFisheye($0) }) else {
            print("player fisheye: no fisheye camera (run with the fisheye fixture), skipped")
            return playbackQuality(b, fake, rec)
        }
        let camera = RecordingCamera(device: ref.device, channel: ref.channel, lens: ref.lens)
        // The fake core lets a playback add only a camera of a device whose
        // recordings are on; its own switch is used so the window's lanes
        // stay as they are.
        let enable = RecordingsRequest.enable([camera], utcOffset: rec.utcOffset)
        try? fake.recordingsEnable(deviceKey: camera.device, json: enable)
        p.add(camera)
        whenPlayback(fake, {
            stage.items.contains { $0.camera == camera } && p.cells.first { $0.camera == camera }?.tile.latestPicture() != nil
        }, timeout: 4) { [self] in
            guard p.cameras.contains(camera), p.cells.first(where: { $0.camera == camera })?.tile.latestPicture() != nil else {
                print("player fisheye: the fisheye camera has no recording picture here, skipped (cameras \(p.cameras.count), items \(stage.items.count), states \(p.cells.map(\.state)))")
                p.remove(camera)
                try? fake.recordingsEnable(deviceKey: camera.device, json: nil)
                return playbackQuality(b, fake, rec)
            }
            stage.drawNow()
            if !stage.isFisheye(camera) { problems.append("player fisheye: the camera is not a fisheye camera to the player") }
            let index = p.index(of: camera) ?? 0
            let items = TileHeaderLayout.menu(for: p.headerTile(index)).map(\.item)
            if !items.contains(.fisheye) || !items.contains(.saveOriginalFisheye) {
                problems.append("player fisheye: the menu holds \(items)")
            }
            p.setFisheyeMode(.panorama, for: camera)
            // The circle is measured from the first pictures, on the app's clock.
            for _ in 0..<FisheyeCircleTracker.maxTries where stage.renderedDewarp(of: camera) == nil {
                stage.drawNow()
                advance(FisheyeCircleTracker.interval)
            }
            if stage.renderedDewarp(of: camera) == nil { problems.append("player fisheye: the panorama is not drawn through the dewarp") }
            stage.pan(camera: camera, by: CGVector(dx: 40, dy: 0))
            if p.tileViews.fisheyeSetting(camera).aim == nil { problems.append("player fisheye: a drag did not turn the view") }
            stage.drawNow()
            let folder = freshPictureFolder(model)
            let name = PictureNaming.sanitize(model.name(of: ref))
            let before = p.pictureSaves
            p.perform(.savePicture, at: index)
            when({ p.pictureSaves > before }, timeout: 8) { [self] in
                let record = p.pictureSaves > before ? p.lastPictureSave : nil
                let image = checkSaved(
                    record, "player fisheye view", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3,
                    confirmation: (p.toast?.text, p.toast?.actionTitle))
                // The pixels are the fixture's colors where the reference
                // mapping says, as for a live tile's saved view.
                if let image, let record,
                    let dewarp = stage.dewarp(
                        camera, picture: record.picture, tileSize: CGSize(width: image.width, height: image.height))
                {
                    let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
                    let (checked, matched) = Self.fisheyeMatches(dewarp, in: rect) { image.rgb(x: $0, y: $1) }
                    print("player fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                    if checked < 20 || Double(matched) < 0.9 * Double(checked) {
                        problems.append("player fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                    }
                }
                if record?.dewarped != true { problems.append("player fisheye: the saved picture was not dewarped") }
                let again = p.pictureSaves
                p.perform(.saveOriginalFisheye, at: index)
                when({ p.pictureSaves > again }, timeout: 8) { [self] in
                    let original = p.pictureSaves > again ? p.lastPictureSave : nil
                    checkSaved(
                        original, "player original fisheye", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3,
                        confirmation: (p.toast?.text, p.toast?.actionTitle))
                    if original?.dewarped != false { problems.append("player fisheye: the original picture was dewarped") }
                    p.resetView(camera)
                    if p.tileViews.fisheyeSetting(camera).aim != nil { problems.append("player fisheye: 0 did not reset the view") }
                    // A camera that leaves the player forgets its view.
                    p.remove(camera)
                    if p.tileViews.fisheyeSetting(camera) != .original { problems.append("player fisheye: the removed camera kept its view") }
                    try? FileManager.default.removeItem(at: folder)
                    try? fake.recordingsEnable(deviceKey: camera.device, json: nil)
                    playbackQuality(b, fake, rec)
                }
            }
        }
    }
}
