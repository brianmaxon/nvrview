import AppKit
import Foundation
import NVRCore

/// The folder of saved call transcripts (docs/UX.md "Doorbell calls"):
/// `~/Library/Application Support/NVRView/Transcripts` (under `--data-dir`
/// when given; a temporary folder in the self-test). Each file is written
/// atomically; then the folder is trimmed to the newest
/// `CallTranscriptFile.maxFiles` files and `maxBytes` bytes, oldest first.
/// Only used while "Save call transcripts" is on.
@MainActor
final class TranscriptStore {
    var folder: URL

    init(folder: URL) { self.folder = folder }

    /// Writes one transcript under a name not yet taken, then trims the
    /// folder. Returns the file, or nil when it could not be written.
    func save(name: String, text: String) -> URL? {
        let fm = FileManager.default
        do {
            try fm.createDirectory(at: folder, withIntermediateDirectories: true)
            let taken = Set((try? fm.contentsOfDirectory(atPath: folder.path)) ?? [])
            let file = folder.appendingPathComponent(CallTranscriptFile.uniqueName(name, taken: taken))
            try Data(text.utf8).write(to: file, options: .atomic)
            prune()
            return file
        } catch {
            NSLog("saving a call transcript failed: %@", error.localizedDescription)
            return nil
        }
    }

    /// Deletes the oldest transcripts beyond the bounds (only `.txt` files;
    /// anything else in the folder is left alone).
    func prune() {
        let fm = FileManager.default
        let keys: [URLResourceKey] = [.fileSizeKey, .contentModificationDateKey, .isRegularFileKey]
        guard let urls = try? fm.contentsOfDirectory(at: folder, includingPropertiesForKeys: keys) else { return }
        var files: [CallTranscriptFile.StoredFile] = []
        for url in urls where url.pathExtension.lowercased() == "txt" {
            guard let v = try? url.resourceValues(forKeys: Set(keys)), v.isRegularFile == true else { continue }
            files.append(
                .init(name: url.lastPathComponent, bytes: v.fileSize ?? 0, modified: v.contentModificationDate ?? .distantPast))
        }
        for name in CallTranscriptFile.toDelete(files) {
            try? fm.removeItem(at: folder.appendingPathComponent(name))
        }
    }

    /// The saved transcripts, by name (self-test).
    func names() -> [String] {
        ((try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []).filter { $0.hasSuffix(".txt") }.sorted()
    }

    /// "Open Transcripts Folder": creates it if needed and shows it in the Finder.
    func openFolder() {
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        NSWorkspace.shared.open(folder)
    }
}
