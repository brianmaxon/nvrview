import AppKit
import NVRCore
import VideoKit
import AppBase

// The recording player's picture features (docs/UX.md "Recordings"): the
// same header model, menus, keys and saves as a live tile, for the pictures
// a playback shows. Every picture rule is the live tiles' own
// (`TileHeaderLayout`, `PlayerTileViews` over `TileViews`, `PictureRenderBuilder`,
// `PictureSaver`); what is here only connects them to the player's cells.

/// A short message over the player.
package struct PlayerToast: Equatable, Identifiable {
    package let id: Int
    package var text: String
    package var actionTitle: String?
    package var url: URL?

    package init(id: Int, text: String, actionTitle: String? = nil, url: URL? = nil) {
        self.id = id
        self.text = text
        self.actionTitle = actionTitle
        self.url = url
    }
}

extension RecordingsPlayer {
    /// The cell showing `camera`.
    package func index(of camera: RecordingCamera) -> Int? { cells.firstIndex { $0.camera == camera } }

    /// What the header model needs to know about the cell `index`: a picture
    /// to save, a fisheye camera, the zoom and the crop.
    package func headerTile(_ index: Int) -> TileHeaderTile {
        guard cells.indices.contains(index) else { return TileHeaderTile(playback: true) }
        let cell = cells[index]
        return TileHeaderTile(
            fisheye: stage?.isFisheye(cell.camera) == true, picture: cell.tile.latestPicture() != nil,
            zoomed: tileViews.isZoomed(cell.camera), panned: tileViews.isPanned(cell.camera), playback: true)
    }

    /// The cell the keys act on: the one under the pointer, else the large
    /// (or first) one, the one whose sound plays.
    package var selectedIndex: Int? {
        if let hovered, cells.indices.contains(hovered) { return hovered }
        return PlayerGrid.audibleIndex(mode, count: cells.count)
    }

    /// A key, the header or a menu for the cell `index`.
    package func perform(_ item: TileHeaderItem, at index: Int) {
        guard cells.indices.contains(index) else { return }
        let camera = cells[index].camera
        switch item {
        case .savePicture: savePicture(index, kind: .view, ask: false)
        case .savePictureAs: savePicture(index, kind: .view, ask: true)
        case .saveZoomedArea: savePicture(index, kind: .zoomedArea, ask: false)
        case .saveOriginalFisheye: savePicture(index, kind: .originalFisheye, ask: false)
        case .resetZoom, .resetPosition: resetView(camera)
        case .fillTile: setFill(!tileViews.fills(camera), for: camera)
        default: break
        }
    }

    /// 0: back to 1x on the selected cell, a filled crop to the middle, a
    /// dewarped view to its default direction.
    package func resetView(_ camera: RecordingCamera, eased: Bool = true) {
        if let stage { stage.reset(camera, eased: eased) } else { tileViews.reset(camera) }
    }

    package func resetSelectedView() {
        guard let i = selectedIndex else { return }
        resetView(cells[i].camera)
    }

    package func setFill(_ fills: Bool, for camera: RecordingCamera) {
        tileViews.setFill(fills, for: camera)
    }

    package func setFisheyeMode(_ mode: FisheyeViewMode, for camera: RecordingCamera) {
        tileViews.setFisheyeMode(mode, for: camera)
    }

    package func setFisheyeMount(_ mount: FisheyeMount, for camera: RecordingCamera) {
        guard let app else { return }
        app.setFisheyeMount(CameraRef(device: camera.device, channel: camera.channel, lens: camera.lens), mount)
    }

    package func mount(of camera: RecordingCamera) -> FisheyeMount { stage?.mount(camera) ?? .ceiling }

    // MARK: Messages

    package func showToast(_ text: String, actionTitle: String? = nil, url: URL? = nil) {
        toast = PlayerToast(id: (toast?.id ?? 0) + 1, text: text, actionTitle: actionTitle, url: url)
    }

    // MARK: Save picture

    /// Saves the picture of the cell `index` (docs/CAPTURE.md "Save
    /// picture"): to the pictures folder, or with `ask` where a save panel
    /// says. The picture is taken now; the confirmation comes when the file
    /// is written. The region and the file names follow the live tiles' rules
    /// (the share-safe setting included).
    package func savePicture(_ index: Int, kind: PictureKind, ask: Bool) {
        guard cells.indices.contains(index), let app, let stage else { return }
        let camera = cells[index].camera
        guard let job = stage.pictureRender(camera: camera, kind: kind) else {
            showToast("No picture to save")
            return
        }
        let name = app.name(of: CameraRef(device: camera.device, channel: camera.channel, lens: camera.lens))
        let folder = app.picturesFolder, format = app.pictureFormat, shareSafe = app.shareSafeNames
        let renderer = app.pictureRenderer
        let date = Date()
        flashCamera = camera
        flashCount += 1
        let record = { [weak self] (result: Result<PictureSaver.Saved, PictureSaver.Failure>) in
            self?.finishSave(result, job: job, kind: kind)
        }
        if ask {
            let suggested = SavePicturePanel.suggestedName(
                camera: name, date: date, folder: folder, format: format, shareSafe: shareSafe)
            SavePicturePanel.ask(suggested: suggested, folder: folder, format: format, window: stage.window) { url in
                guard let url else { return }
                Task { @MainActor in
                    record(await PictureSaver.save(job, to: url, defaultFormat: format, renderer: renderer))
                }
            }
            return
        }
        Task { @MainActor in
            record(
                await PictureSaver.save(
                    job, camera: name, date: date, folder: folder, format: format, shareSafe: shareSafe,
                    renderer: renderer))
        }
    }

    private func finishSave(
        _ result: Result<PictureSaver.Saved, PictureSaver.Failure>, job: PictureRender, kind: PictureKind
    ) {
        lastPictureSave = PictureSaveRecord(
            result: result, kind: kind, picture: job.picture, color: job.color, region: job.source,
            dewarped: job.dewarp != nil)
        pictureSaves += 1
        switch result {
        case .success(let saved): showToast("Picture saved", actionTitle: "Show in Finder", url: saved.url)
        case .failure(.write(let fileName)): showToast("Couldn't save \(fileName)")
        case .failure: showToast("Couldn't save the picture")
        }
    }
}
