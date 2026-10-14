import AppKit
import NVRCore
import UniformTypeIdentifiers
import AppBase

// A kept verification picture taken out of the Events window (docs/UX.md
// "Events"): Open in Preview, Save Picture As…, Copy, Show in Finder, and a
// drag to the Finder or another app. The picture is the kept frame, with or
// without every shown method's boxes drawn over it at the frame's own size;
// the files carry no metadata beyond the share-safe name (the camera is
// "Camera A" with share-safe names on), as Save Picture does.

/// What is drawn: one kept frame with the boxes of the methods that are on.
/// Immutable; the pieces are used from a drag's background thread only to
/// read, so it is passed across without a copy.
struct EvidenceSubject: @unchecked Sendable {
    var image: NSImage
    var evidence: AlertEvidence
    var frame: Int
    var visible: Set<String>
    var camera: String
    /// When the frame was taken (its file name's time).
    var frameMs: Int64

    /// Some shown method has a box in this frame.
    var hasBoxes: Bool {
        evidence.methods.contains { visible.contains($0.name) && !($0.boxes[frame] ?? []).isEmpty }
    }
}

/// Draws a subject into an encoded picture.
enum EvidenceRendering {
    struct Rendered {
        var data: Data
        var width: Int
        var height: Int
    }

    /// The box colors (the same order as the pane's `EvidenceView.colors`).
    static let colors: [NSColor] = [.systemOrange, .systemCyan, .systemPink, .systemYellow, .systemMint]

    /// The frame at its own pixel size, with the boxes when `withBoxes`.
    /// Boxes are placed by `EvidenceOverlay`, the pane's mapping, over the
    /// whole picture.
    static func render(_ subject: EvidenceSubject, withBoxes: Bool, format: PictureFormat) -> Rendered? {
        guard let cg = subject.image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        let w = cg.width, h = cg.height
        guard w > 0, h > 0,
            let space = CGColorSpace(name: CGColorSpace.sRGB),
            let ctx = CGContext(
                data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        let size = CGSize(width: w, height: h)
        if withBoxes {
            let drawn = EvidenceOverlay.layout(
                subject.evidence, frame: subject.frame, visible: subject.visible, picture: size, in: size)
            drawBoxes(drawn, in: ctx, size: size)
        }
        guard let out = ctx.makeImage() else { return nil }
        let rep = NSBitmapImageRep(cgImage: out)
        let data: Data?
        switch format {
        case .png: data = rep.representation(using: .png, properties: [:])
        case .jpeg: data = rep.representation(using: .jpeg, properties: [.compressionFactor: PictureFormat.jpegQuality])
        }
        return data.map { Rendered(data: $0, width: w, height: h) }
    }

    private static func drawBoxes(_ drawn: [EvidenceOverlay.Drawn], in ctx: CGContext, size: CGSize) {
        guard !drawn.isEmpty else { return }
        // Line and label sizes grow with the picture, so a large frame reads
        // like the pane.
        let k = max(1, size.width / 640)
        let graphics = NSGraphicsContext(cgContext: ctx, flipped: false)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = graphics
        defer { NSGraphicsContext.restoreGraphicsState() }
        for d in drawn {
            let color = colors[d.colorIndex % colors.count]
            // `rect` is from the top left; the context's origin is bottom left.
            let r = CGRect(x: d.rect.minX, y: size.height - d.rect.maxY, width: d.rect.width, height: d.rect.height)
            ctx.saveGState()
            ctx.setStrokeColor(color.cgColor)
            ctx.setLineWidth(2 * k)
            ctx.setLineDash(phase: 0, lengths: d.colorIndex % 2 == 0 ? [] : [6 * k, 3 * k])
            ctx.stroke(r.insetBy(dx: k, dy: k))
            ctx.restoreGState()
            let text = NSAttributedString(
                string: " \(d.caption) ",
                attributes: [
                    .font: NSFont.boldSystemFont(ofSize: 11 * k), .foregroundColor: NSColor.black,
                    .backgroundColor: color,
                ])
            let label = text.size()
            // Above the box, or inside its top when there is no room.
            let y = r.maxY + label.height <= size.height ? r.maxY : r.maxY - label.height
            text.draw(at: CGPoint(x: max(r.minX, 0), y: y))
        }
    }
}

extension SmartAlertEventsModel {
    /// The shown frame as a subject, nil without a picture.
    var subject: EvidenceSubject? {
        guard let evidence, let image = evidenceImages[frame] else { return nil }
        let at = evidence.frames.first { $0.index == frame }?.atMs ?? 0
        return EvidenceSubject(
            image: image, evidence: evidence, frame: frame, visible: visibleMethods,
            camera: selected.map(place) ?? "Camera", frameMs: at > 0 ? at : evidence.createdMs)
    }

    /// Boxes are drawn "as shown": when any method's boxes are on.
    var showsBoxes: Bool { subject?.hasBoxes ?? false }

    /// The suggested name, in `folder` or for a single-use folder.
    func pictureFileName(_ subject: EvidenceSubject, format: PictureFormat, in folder: URL?) -> String {
        EvidencePictureFiles.fileName(
            camera: subject.camera, frameMs: subject.frameMs, shareSafe: app.shareSafeNames, format: format,
            taken: { name in folder.map { FileManager.default.fileExists(atPath: $0.appendingPathComponent(name).path) } ?? false }
        ) ?? "Picture.\(format.fileExtension)"
    }

    /// Where temporary files go (Open in Preview and drags).
    nonisolated static var defaultTempFolder: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NVRView", isDirectory: true)
            .appendingPathComponent("EvidencePictures", isDirectory: true)
    }

    // MARK: Save, Copy, Preview, Finder

    /// Writes the shown frame to exactly `url` (the format follows its extension).
    @discardableResult
    func writePicture(to url: URL, withBoxes: Bool) -> Bool {
        guard let subject else { return false }
        let format = PictureFormat(extension: url.pathExtension) ?? app.pictureFormat
        guard let r = EvidenceRendering.render(subject, withBoxes: withBoxes, format: format) else { return false }
        do {
            try r.data.write(to: url, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    /// Save Picture As… with the panel Save Picture uses.
    func savePictureAs(withBoxes: Bool, window: NSWindow?) {
        guard let subject else { return }
        let format = app.pictureFormat, folder = app.picturesFolder
        let suggested = SavePicturePanel.suggestedName(
            camera: subject.camera, date: Date(timeIntervalSince1970: Double(subject.frameMs) / 1000), folder: folder,
            format: format, shareSafe: app.shareSafeNames)
        SavePicturePanel.ask(suggested: suggested, folder: folder, format: format, window: window) { [weak self] url in
            guard let self, let url else { return }
            note = writePicture(to: url, withBoxes: withBoxes) ? "Picture saved." : "The picture could not be saved."
        }
    }

    /// The shown frame as a file under `tempFolder`, in a folder of its own
    /// so the name is the clean one. Nil when nothing could be written.
    func writeTemporaryPicture(withBoxes: Bool) -> URL? {
        guard let subject else { return nil }
        return Self.writeTemporary(subject, withBoxes: withBoxes, format: app.pictureFormat, folder: tempFolder, name: pictureFileName(subject, format: app.pictureFormat, in: nil))
    }

    nonisolated static func writeTemporary(
        _ subject: EvidenceSubject, withBoxes: Bool, format: PictureFormat, folder: URL, name: String
    ) -> URL? {
        guard let r = EvidenceRendering.render(subject, withBoxes: withBoxes, format: format) else { return nil }
        let dir = folder.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let url = dir.appendingPathComponent(name)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try r.data.write(to: url, options: .atomic)
            return url
        } catch {
            return nil
        }
    }

    /// Open in Preview: a temporary file (removed after 24 hours).
    func openInPreview(withBoxes: Bool? = nil) {
        EvidencePictureFiles.removeOld(in: tempFolder, now: Date())
        guard let url = writeTemporaryPicture(withBoxes: withBoxes ?? showsBoxes) else {
            note = "The picture could not be opened."
            return
        }
        opener(url)
    }

    /// Copy: the picture as shown (boxes when any are on).
    @discardableResult
    func copyPicture(to pasteboard: NSPasteboard = .general) -> Bool {
        guard let subject, let r = EvidenceRendering.render(subject, withBoxes: subject.hasBoxes, format: .png) else {
            return false
        }
        pasteboard.clearContents()
        return pasteboard.setData(r.data, forType: .png)
    }

    /// A pinned set's folder under the core's data directory, if it is there.
    var pinnedFolder: URL? {
        guard let evidence, evidence.pinned, let base = app.coreDataDirectory else { return nil }
        let folder = base.appendingPathComponent("alerts/evidence/pinned/\(evidence.triggerID)", isDirectory: true)
        return FileManager.default.fileExists(atPath: folder.path) ? folder : nil
    }

    func showInFinder() {
        guard let folder = pinnedFolder else { return }
        NSWorkspace.shared.activateFileViewerSelecting([folder])
    }

    // MARK: Drag out

    /// A file promise for the shown frame: the file is made when the drop
    /// asks for it, named as Save Picture names it, with boxes when shown.
    func dragProvider() -> NSItemProvider? {
        guard let subject else { return nil }
        let withBoxes = subject.hasBoxes
        let format = app.pictureFormat
        let folder = tempFolder
        let name = pictureFileName(subject, format: format, in: nil)
        let provider = NSItemProvider()
        provider.suggestedName = (name as NSString).deletingPathExtension
        let type = format == .png ? UTType.png : UTType.jpeg
        provider.registerFileRepresentation(forTypeIdentifier: type.identifier, fileOptions: [], visibility: .all) { done in
            if let url = Self.writeTemporary(subject, withBoxes: withBoxes, format: format, folder: folder, name: name) {
                done(url, false, nil)
            } else {
                done(nil, false, CocoaError(.fileWriteUnknown))
            }
            return nil
        }
        return provider
    }

    // MARK: Menu

    /// The picture's context menu (the pane's and the viewer's).
    func pictureMenu(window: @escaping () -> NSWindow?) -> NSMenu {
        let menu = NSMenu()
        let has = subject != nil
        func add(_ title: String, enabled: Bool = true, _ action: @escaping () -> Void) {
            let item = ClosureMenuItem(title: title, action: action)
            item.isEnabled = has && enabled
            menu.addItem(item)
        }
        add("Open in Preview") { [weak self] in self?.openInPreview() }
        add("Save Picture With Detection Boxes As…", enabled: subject?.hasBoxes ?? false) { [weak self] in
            self?.savePictureAs(withBoxes: true, window: window())
        }
        add("Save Picture Only As…") { [weak self] in self?.savePictureAs(withBoxes: false, window: window()) }
        add("Copy") { [weak self] in self?.copyPicture() }
        if pinnedFolder != nil {
            menu.addItem(.separator())
            add("Show in Finder") { [weak self] in self?.showInFinder() }
        }
        return menu
    }
}

/// Removes temporary pictures after 24 hours: at launch, then hourly.
@MainActor
enum EvidenceTempSweeper {
    private static var timer: Timer?

    static func start() {
        sweep()
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 3600, repeats: true) { _ in
            MainActor.assumeIsolated { sweep() }
        }
    }

    static func sweep() {
        EvidencePictureFiles.removeOld(in: SmartAlertEventsModel.defaultTempFolder, now: Date())
    }
}

extension SmartAlertEventsModel {
    /// Opens a file in Preview.
    static func openInPreviewApp(_ url: URL) {
        if let preview = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Preview") {
            NSWorkspace.shared.open([url], withApplicationAt: preview, configuration: NSWorkspace.OpenConfiguration())
        } else {
            NSWorkspace.shared.open(url)
        }
    }
}
