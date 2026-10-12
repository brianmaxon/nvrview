import Foundation
import NVRCore
import VideoKit

/// Writes a rendered picture to a file (docs/CAPTURE.md "Save picture"):
/// renders it off the main thread, encodes it, and writes it without
/// replacing a file that is already there.
enum PictureSaver {
    /// A picture written.
    struct Saved: Sendable, Equatable {
        var url: URL
        var width: Int
        var height: Int
        var bytes: Int
    }

    enum Failure: Error, Sendable, Equatable {
        /// Rendering or encoding failed.
        case encode
        /// No free name in the folder.
        case noName
        /// The file could not be written; the name only, never the path.
        case write(fileName: String)
    }

    /// Saves under the next free name in `folder`: the shared naming rule.
    @concurrent
    static func save(
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
    static func save(
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
    var encoding: PictureEncoding {
        switch self {
        case .png: .png
        case .jpeg: .jpeg(quality: Self.jpegQuality)
        }
    }
}
