import AppKit
import NVRCore
import UniformTypeIdentifiers
import VideoKit

// Save picture (docs/CAPTURE.md "Save picture", docs/UX.md "Save picture"):
// the tile's current decoded picture, at the full size of the stream it
// shows, written as PNG or JPEG to the pictures folder. The picture is the
// tile's own pixel buffer (no extra decode, no stream switch); rendering,
// encoding and writing happen off the main thread.

/// Which picture a save makes.
enum PictureKind {
    /// What the tile shows, at the source's size: a fisheye tile's dewarped
    /// view; otherwise the whole picture, whatever the zoom.
    case view
    /// The visible region of a zoomed (or cropped) tile.
    case zoomedArea
    /// A fisheye camera's circle as received, not dewarped.
    case originalFisheye
}

/// A finished save, with what it was made from (self-test).
struct PictureSaveRecord {
    var result: Result<PictureSaver.Saved, PictureSaver.Failure>
    var kind: PictureKind
    var picture: DecodedPicture
    var color: YCbCrToRGB
    var region: CGRect
    var dewarped: Bool
}

extension StageView {
    /// What to draw for `kind` from the tile at `path` now: its newest
    /// decoded picture, the color matrix, and the region or the dewarp.
    /// Nil without a picture.
    func pictureRender(at path: NodePath, kind: PictureKind) -> PictureRender? {
        guard let camera = state.tree[path]?.camera, let tile = model.leases.tile(for: camera),
            let picture = tile.latestPicture()
        else { return nil }
        let snapshot = tile.snapshot
        let mapping = PictureMapping(
            snapshot: snapshot, pictureSize: PixelSize(width: UInt32(picture.width), height: UInt32(picture.height)))
        let layout = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
        let offset = CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y))
        guard layout.width >= 1, layout.height >= 1 else { return nil }
        let color = snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
            ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
        let whole = PictureRegion.whole(layoutSize: layout, offset: offset)
        // A dewarped fisheye view is drawn at the source's resolution.
        if kind != .originalFisheye,
            let dewarp = fisheyeDewarp(camera: camera, path: path, picture: picture, tileSize: layout)
        {
            return PictureRender(
                picture: picture, color: color, source: whole.rect, dewarp: dewarp.constants, width: whole.width,
                height: whole.height)
        }
        var region = whole
        if kind == .zoomedArea, let leaf = frames.leaves.first(where: { $0.path == path }) {
            region = PictureRegion.visible(
                layoutSize: layout, offset: offset, tile: leaf.rect.size, fill: fills(atTile: path),
                zoom: zoom(atTile: path)) ?? whole
        }
        return PictureRender(
            picture: picture, color: color, source: region.rect, width: region.width, height: region.height)
    }

    /// Saves the picture of the tile at `path`: to the pictures folder, or
    /// with `ask` where a save panel says. The picture is taken now; the
    /// confirmation comes when the file is written.
    func savePicture(at path: NodePath, kind: PictureKind, ask: Bool) {
        guard let camera = state.tree[path]?.camera, let job = pictureRender(at: path, kind: kind) else {
            showToast("No picture to save")
            return
        }
        let name = model.name(of: camera)
        let folder = model.picturesFolder, format = model.pictureFormat, shareSafe = model.shareSafeNames
        let renderer = model.pictureRenderer
        let date = Date()
        flash(path)
        let record = { [weak self] (result: Result<PictureSaver.Saved, PictureSaver.Failure>) in
            self?.finishSave(result, job: job, kind: kind)
        }
        if ask {
            let suggested = Self.suggestedName(
                camera: name, date: date, folder: folder, format: format, shareSafe: shareSafe)
            askWhere(suggested: suggested, folder: folder, format: format) { url in
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
        case .success(let saved):
            showToast(
                "Picture saved",
                action: ("Show in Finder", { NSWorkspace.shared.activateFileViewerSelecting([saved.url]) }))
        case .failure(.write(let fileName)):
            showToast("Couldn't save \(fileName)")
        case .failure:
            showToast("Couldn't save the picture")
        }
    }

    /// The next free name in `folder`, for the save panel to start from.
    private static func suggestedName(
        camera: String, date: Date, folder: URL, format: PictureFormat, shareSafe: Bool
    ) -> String {
        PictureNaming.fileName(
            localMillis: PictureNaming.localMillis(date), camera: camera, shareSafe: shareSafe, format: format,
            taken: { FileManager.default.fileExists(atPath: folder.appendingPathComponent($0).path) })
            ?? "Picture.\(format.fileExtension)"
    }

    /// Save Picture As…: a save panel in the pictures folder with a format
    /// choice; `done` gets the chosen file, nil if canceled.
    private func askWhere(suggested: String, folder: URL, format: PictureFormat, done: @escaping (URL?) -> Void) {
        let panel = NSSavePanel()
        panel.title = "Save Picture As"
        panel.canCreateDirectories = true
        panel.isExtensionHidden = false
        panel.allowedContentTypes = [.png, .jpeg]
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        panel.directoryURL = folder
        panel.nameFieldStringValue = suggested
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.addItems(withTitles: PictureFormat.allCases.map(\.title))
        popup.selectItem(at: PictureFormat.allCases.firstIndex(of: format) ?? 0)
        let label = NSTextField(labelWithString: "Format:")
        let row = NSStackView(views: [label, popup])
        row.spacing = 8
        row.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 8, right: 12)
        panel.accessoryView = row
        let handler = PanelFormatHandler(panel: panel, popup: popup)
        popup.target = handler
        popup.action = #selector(PanelFormatHandler.changed(_:))
        let finish: (NSApplication.ModalResponse) -> Void = { response in
            withExtendedLifetime(handler) { done(response == .OK ? panel.url : nil) }
        }
        if let window {
            panel.beginSheetModal(for: window, completionHandler: finish)
        } else {
            panel.begin(completionHandler: finish)
        }
    }

    /// A brief white flash over the tile, as a shutter (not under Reduce
    /// Motion; the confirmation still shows).
    private func flash(_ path: NodePath) {
        guard !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion, let chrome = chromes[path] else { return }
        let view = NSView(frame: chrome.frame)
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.white.withAlphaComponent(0.55).cgColor
        view.setAccessibilityElement(false)
        addSubview(view, positioned: .above, relativeTo: nil)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.25
            view.animator().alphaValue = 0
        } completionHandler: {
            view.removeFromSuperview()
        }
        // The toast stays on top of the flash.
        if !toast.isHidden { addSubview(toast, positioned: .above, relativeTo: nil) }
    }

    /// Opens the pictures folder in the Finder (made first if missing).
    func openPicturesFolder() {
        let folder = model.picturesFolder
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        NSWorkspace.shared.open(folder)
    }
}

/// Keeps a save panel's file name extension in step with its format menu.
@MainActor
private final class PanelFormatHandler: NSObject {
    let panel: NSSavePanel
    let popup: NSPopUpButton

    init(panel: NSSavePanel, popup: NSPopUpButton) {
        self.panel = panel
        self.popup = popup
    }

    @objc func changed(_ sender: Any?) {
        let format = PictureFormat.allCases[max(popup.indexOfSelectedItem, 0)]
        let stem = (panel.nameFieldStringValue as NSString).deletingPathExtension
        panel.nameFieldStringValue = "\(stem).\(format.fileExtension)"
    }
}
