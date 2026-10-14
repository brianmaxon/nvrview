import AppKit
import Foundation
import NVRCore
import VideoKit

/// Writes a rendered picture to a file (docs/CAPTURE.md "Save picture"):
/// renders it off the main thread, encodes it, and writes it without
/// replacing a file that is already there.
package enum PictureSaver {
    /// A picture written.
    package struct Saved: Sendable, Equatable {
        package var url: URL
        package var width: Int
        package var height: Int
        package var bytes: Int

        package init(url: URL, width: Int, height: Int, bytes: Int) {
            self.url = url
            self.width = width
            self.height = height
            self.bytes = bytes
        }
    }

    package enum Failure: Error, Sendable, Equatable {
        /// Rendering or encoding failed.
        case encode
        /// No free name in the folder.
        case noName
        /// The file could not be written; the name only, never the path.
        case write(fileName: String)
    }

    /// Saves under the next free name in `folder`: the shared naming rule.
    @concurrent
    package static func save(
        _ job: PictureRender, camera: String, date: Date, folder: URL, format: PictureFormat, shareSafe: Bool,
        renderer: PictureRenderer
    ) async -> Result<Saved, Failure> {
        guard let data = await renderer.encode(job, format: format.encoding) else { return .failure(.encode) }
        let manager = FileManager.default
        let ms = PictureNaming.localMillis(date)
        for _ in 0..<8 {
            guard
                let name = PictureNaming.fileName(
                    localMillis: ms, camera: camera, shareSafe: shareSafe, format: format,
                    taken: { manager.fileExists(atPath: folder.appendingPathComponent($0).path) })
            else { return .failure(.noName) }
            let url = folder.appendingPathComponent(name)
            do {
                try manager.createDirectory(at: folder, withIntermediateDirectories: true)
                try data.write(to: url, options: .withoutOverwriting)
                return .success(Saved(url: url, width: job.width, height: job.height, bytes: data.count))
            } catch let error as CocoaError where error.code == .fileWriteFileExists {
                continue  // taken meanwhile: the next name
            } catch {
                return .failure(.write(fileName: name))
            }
        }
        return .failure(.noName)
    }

    /// Saves to exactly `url` (a name the user chose in a save panel, which
    /// has already asked about replacing); the format follows the extension.
    @concurrent
    package static func save(
        _ job: PictureRender, to url: URL, defaultFormat: PictureFormat, renderer: PictureRenderer
    ) async -> Result<Saved, Failure> {
        let format = PictureFormat(extension: url.pathExtension) ?? defaultFormat
        guard let data = await renderer.encode(job, format: format.encoding) else { return .failure(.encode) }
        do {
            try data.write(to: url, options: .atomic)
            return .success(Saved(url: url, width: job.width, height: job.height, bytes: data.count))
        } catch {
            return .failure(.write(fileName: url.lastPathComponent))
        }
    }
}

extension PictureFormat {
    /// The encoder setting for this format.
    package var encoding: PictureEncoding {
        switch self {
        case .png: .png
        case .jpeg: .jpeg(quality: Self.jpegQuality)
        }
    }
}

/// The Save Picture As… panel and the default file name (shared by the live
/// tiles and the recording player).
@MainActor
package enum SavePicturePanel {
    /// The next free name in `folder`, for the save panel to start from.
    package static func suggestedName(
        camera: String, date: Date, folder: URL, format: PictureFormat, shareSafe: Bool
    ) -> String {
        PictureNaming.fileName(
            localMillis: PictureNaming.localMillis(date), camera: camera, shareSafe: shareSafe, format: format,
            taken: { FileManager.default.fileExists(atPath: folder.appendingPathComponent($0).path) })
            ?? "Picture.\(format.fileExtension)"
    }

    /// Save Picture As…: a save panel in the pictures folder with a format
    /// choice; `done` gets the chosen file, nil if canceled.
    package static func ask(
        suggested: String, folder: URL, format: PictureFormat, window: NSWindow?, done: @escaping (URL?) -> Void
    ) {
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
