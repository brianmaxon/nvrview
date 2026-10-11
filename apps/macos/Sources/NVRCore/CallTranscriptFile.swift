import Foundation

// Call transcripts saved as text files (docs/UX.md "Doorbell calls"), only
// when the user turned "Save call transcripts" on. The decisions are pure and
// tested headless, with the same vectors as the Windows app's
// TranscriptFileTests (NvrView.Tiles.Tests): the file's name and text, a
// name that does not collide, and which old files to delete. The app does the
// writing (atomic) and the deleting.

/// A saved call transcript: names, text, and the folder's bounds.
public enum CallTranscriptFile {
    /// At most this many transcript files are kept, newest first.
    public static let maxFiles = 500
    /// And at most this many bytes in all (50 MB).
    public static let maxBytes = 50 * 1024 * 1024
    /// The camera name's share of a file name, in Unicode scalars.
    public static let maxCameraName = 60

    /// The file's name: "2026-10-11 17.02.03 Front door.txt", the call's
    /// start in `timeZone`, then the camera's name made safe for a file name
    /// on macOS and Windows.
    public static func fileName(startedAt: Date, camera: String, timeZone: TimeZone) -> String {
        let f = formatter("yyyy-MM-dd HH.mm.ss", timeZone)
        return "\(f.string(from: startedAt)) \(sanitized(camera)).txt"
    }

    /// A camera name fit for a file name: no path separators, reserved or
    /// control characters (each becomes "-"), whitespace runs as one space,
    /// no leading or trailing spaces or dots, at most `maxCameraName`
    /// scalars; "Camera" when nothing is left.
    public static func sanitized(_ name: String) -> String {
        var out = String.UnicodeScalarView()
        var space = false
        for scalar in name.unicodeScalars {
            if scalar.properties.isWhitespace {
                space = true
                continue
            }
            if space, !out.isEmpty { out.append(" ") }
            space = false
            let reserved = "/\\:*?\"<>|".unicodeScalars.contains(scalar)
            out.append(reserved || scalar.value < 0x20 || scalar.value == 0x7F ? "-" : scalar)
        }
        var s = String(String.UnicodeScalarView(out.prefix(maxCameraName)))
        let trim = CharacterSet(charactersIn: " .")
        s = s.trimmingCharacters(in: trim)
        return s.isEmpty ? "Camera" : s
    }

    /// `name`, or "name 2.txt", "name 3.txt", ... if a file of that name is
    /// in `taken` (compared without regard to case, as both systems'
    /// default file systems do).
    public static func uniqueName(_ name: String, taken: Set<String>) -> String {
        let lower = Set(taken.map { $0.lowercased() })
        guard lower.contains(name.lowercased()) else { return name }
        let stem = name.hasSuffix(".txt") ? String(name.dropLast(4)) : name
        var n = 2
        while lower.contains("\(stem) \(n).txt".lowercased()) { n += 1 }
        return "\(stem) \(n).txt"
    }

    /// The file's text: a header (camera, kind of call, start and end in
    /// `timeZone` with its offset), a blank line, then each finished caption
    /// line with its time and side ("17:02:10 Visitor: Hello there."). Lines
    /// end with "\n".
    public static func text(
        camera: String, presses: Int, startedAt: Date, endedAt: Date, lines: [Transcript.Line], timeZone: TimeZone
    ) -> String {
        let full = formatter("yyyy-MM-dd HH:mm:ss", timeZone)
        let clock = formatter("HH:mm:ss", timeZone)
        let seconds = max(0, Int(endedAt.timeIntervalSince(startedAt).rounded()))
        let kind = presses == 0 ? "conversation started in the app" : "doorbell, \(presses) press\(presses == 1 ? "" : "es")"
        var out = "NVRView call transcript\n"
        out += "Camera: \(oneLine(camera))\n"
        out += "Call: \(kind)\n"
        out += "Started: \(full.string(from: startedAt)) \(offset(timeZone, at: startedAt))\n"
        out += "Ended: \(full.string(from: endedAt)) (\(seconds) s)\n"
        out += "\n"
        for line in lines {
            let time = line.at.map { clock.string(from: $0) } ?? "--:--:--"
            out += "\(time) \(line.source.label): \(oneLine(line.text))\n"
        }
        return out
    }

    /// A file in the transcripts folder, for `toDelete`.
    public struct StoredFile: Sendable, Equatable {
        public var name: String
        public var bytes: Int
        public var modified: Date

        public init(name: String, bytes: Int, modified: Date) {
            self.name = name
            self.bytes = bytes
            self.modified = modified
        }
    }

    /// The files to delete, oldest first, so that the newest ones kept are
    /// at most `maxFiles` and `maxBytes` together (the newest one is always
    /// kept). Newest by modification time, then by name.
    public static func toDelete(_ files: [StoredFile], maxFiles: Int = maxFiles, maxBytes: Int = maxBytes) -> [String] {
        let newestFirst = files.sorted { a, b in
            a.modified != b.modified ? a.modified > b.modified : a.name > b.name
        }
        var kept = 0
        var bytes = 0
        var cut = newestFirst.count
        for (i, f) in newestFirst.enumerated() {
            if kept > 0, kept >= maxFiles || bytes + f.bytes > maxBytes {
                cut = i
                break
            }
            kept += 1
            bytes += f.bytes
        }
        return newestFirst[cut...].reversed().map(\.name)
    }

    /// "UTC", "UTC+05:30", "UTC-04:00": the zone's offset at `date`.
    static func offset(_ zone: TimeZone, at date: Date) -> String {
        let s = zone.secondsFromGMT(for: date)
        guard s != 0 else { return "UTC" }
        let m = abs(s) / 60
        return String(format: "UTC%@%02d:%02d", s < 0 ? "-" : "+", m / 60, m % 60)
    }

    private static func oneLine(_ s: String) -> String {
        s.split(whereSeparator: \.isNewline).joined(separator: " ")
    }

    private static func formatter(_ format: String, _ zone: TimeZone) -> DateFormatter {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.calendar = Calendar(identifier: .gregorian)
        f.timeZone = zone
        f.dateFormat = format
        return f
    }
}

extension DoorbellCall {
    /// The saved transcript's file name and text when the call ends; nil
    /// when nothing was transcribed (no file is written).
    public func transcriptFile(cameraName: String, endedAt: Date, timeZone: TimeZone = .current) -> (name: String, text: String)? {
        guard !transcript.finals.isEmpty else { return nil }
        return (
            CallTranscriptFile.fileName(startedAt: startedAt, camera: cameraName, timeZone: timeZone),
            CallTranscriptFile.text(
                camera: cameraName, presses: presses, startedAt: startedAt, endedAt: endedAt, lines: transcript.finals,
                timeZone: timeZone)
        )
    }
}
