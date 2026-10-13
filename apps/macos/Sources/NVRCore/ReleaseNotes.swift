import Foundation

/// One version's release notes.
public struct ReleaseNote: Equatable, Sendable {
    public var version: String
    public var notes: String

    public init(version: String, notes: String) {
        self.version = version
        self.notes = notes
    }
}

/// Dotted version numbers, as in `VERSION` ("1.2.3"; a "-suffix" or "+build"
/// is ignored for the order). Mirrored by `AppVersion` in NvrView.Shell.
public enum AppVersion {
    /// The numeric parts, or nil when the text is not dotted numbers.
    public static func parts(_ text: String) -> [Int]? {
        let core = text.trimmingCharacters(in: .whitespacesAndNewlines).prefix { $0 != "-" && $0 != "+" }
        let pieces = core.split(separator: ".", omittingEmptySubsequences: false)
        guard !pieces.isEmpty, pieces.count <= 4 else { return nil }
        var out: [Int] = []
        for p in pieces {
            guard !p.isEmpty, p.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(p) else { return nil }
            out.append(n)
        }
        return out
    }

    /// -1, 0, or 1; nil when either is not a version. Missing parts count as 0.
    public static func compare(_ a: String, _ b: String) -> Int? {
        guard let x = parts(a), let y = parts(b) else { return nil }
        for i in 0..<max(x.count, y.count) {
            let l = i < x.count ? x[i] : 0
            let r = i < y.count ? y[i] : 0
            if l != r { return l < r ? -1 : 1 }
        }
        return 0
    }
}

/// The release notes a build bundles (docs/UPDATES.md, "Bundled release
/// notes"): `release-notes.json`, written by the packaging scripts from notes
/// kept outside the repository. A development build has none.
public struct BundledReleaseNotes: Equatable, Sendable {
    /// The file's name: in the app's Resources folder on macOS, next to the
    /// executable on Windows.
    public static let fileName = "release-notes.json"
    /// The most characters kept of one version's notes (the manifest's limit).
    public static let maxCharacters = 2000

    /// Newest first.
    public var releases: [ReleaseNote]

    public init(releases: [ReleaseNote] = []) {
        self.releases = releases
    }

    public static let none = BundledReleaseNotes()

    public var isEmpty: Bool { releases.isEmpty }

    public func notes(for version: String) -> ReleaseNote? {
        releases.first { AppVersion.compare($0.version, version) == 0 }
    }

    /// Reads the file. Lenient: malformed JSON gives nothing; an entry with a
    /// bad version or no text is skipped, a repeated version keeps its first
    /// entry, and the result is ordered newest first whatever the file's order.
    public static func parse(_ data: Data) -> BundledReleaseNotes {
        guard let o = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
            let list = o["releases"] as? [Any]
        else { return .none }
        var out: [ReleaseNote] = []
        for case let entry as [String: Any] in list {
            guard let version = (entry["version"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                AppVersion.parts(version) != nil,
                let raw = entry["notes"] as? String
            else { continue }
            let text = ReleaseNotesText.clean(raw)
            guard !text.isEmpty, !out.contains(where: { AppVersion.compare($0.version, version) == 0 }) else { continue }
            out.append(ReleaseNote(version: version, notes: text))
        }
        // Stable: equal versions cannot occur (deduplicated above).
        out.sort { (AppVersion.compare($0.version, $1.version) ?? 0) > 0 }
        return BundledReleaseNotes(releases: out)
    }

    /// The notes file in `directory`, or none.
    public static func load(from directory: URL?) -> BundledReleaseNotes {
        guard let directory, let data = try? Data(contentsOf: directory.appendingPathComponent(fileName)),
            data.count <= 1024 * 1024
        else { return .none }
        return parse(data)
    }

    /// Every version's notes as one text, newest first (Help > Release Notes).
    public var displayText: String {
        releases.map { "Version \($0.version)\n\n\(ReleaseNotesText.format($0.notes))" }.joined(separator: "\n\n\n")
    }
}

/// Release notes are plain text: lines, and "- " bullets.
public enum ReleaseNotesText {
    /// Normalizes line endings, trims trailing spaces, and cuts to the limit.
    public static func clean(_ raw: String) -> String {
        let lines = raw.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { line -> String in
                var s = String(line)
                while let last = s.last, last == " " || last == "\t" { s.removeLast() }
                return s
            }
        let text = lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return String(text.prefix(BundledReleaseNotes.maxCharacters))
    }

    /// The text as shown: a "- " or "* " bullet becomes "•", and runs of
    /// blank lines shrink to one.
    public static func format(_ notes: String) -> String {
        var out: [String] = []
        var blank = false
        for line in clean(notes).split(separator: "\n", omittingEmptySubsequences: false).map(String.init) {
            if line.isEmpty {
                if !blank { out.append("") }
                blank = true
                continue
            }
            blank = false
            let indent = line.prefix { $0 == " " }
            let rest = line.dropFirst(indent.count)
            if rest.hasPrefix("- ") || rest.hasPrefix("* ") {
                out.append(indent + "\u{2022} " + rest.dropFirst(2).trimmingCharacters(in: .whitespaces))
            } else {
                out.append(line)
            }
        }
        return out.joined(separator: "\n")
    }
}

/// What to do about the "What's New" window at launch (docs/UX.md, "Release
/// notes"). Mirrored by `WhatsNew` in NvrView.Shell.
public enum WhatsNew {
    public struct Decision: Equatable, Sendable {
        /// The notes to show now, or nil.
        public var show: ReleaseNote?
        /// The version to store as the last one that ran, or nil to leave it.
        public var remember: String?

        public init(show: ReleaseNote?, remember: String?) {
            self.show = show
            self.remember = remember
        }
    }

    /// - A first run (nothing stored) shows nothing and remembers this version.
    /// - A newer version than the stored one shows its notes, if the build has
    ///   them, and remembers it.
    /// - The same version shows nothing; an older one (a downgrade) shows
    ///   nothing and remembers the older one, so going up again shows the notes.
    /// - A current version that is not a version (a development build) does
    ///   nothing at all.
    public static func decide(current: String, lastRun: String?, bundled: BundledReleaseNotes) -> Decision {
        guard AppVersion.parts(current) != nil else { return Decision(show: nil, remember: nil) }
        let now = current.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let lastRun, let order = AppVersion.compare(now, lastRun) else {
            return Decision(show: nil, remember: now)
        }
        if order > 0 { return Decision(show: bundled.notes(for: now), remember: now) }
        return Decision(show: nil, remember: lastRun == now ? nil : now)
    }
}
