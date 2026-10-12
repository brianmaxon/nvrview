import CryptoKit
import DetectorKit
import Foundation

/// The detector model the build pins (`scripts/detector-model.json`, copied
/// into the app's Resources by the packaging script): its name, the size
/// and SHA-256 of the published archive. The archive is never trusted
/// without them.
public struct ModelManifest: Sendable, Equatable, Decodable {
    public var name: String
    /// An explicit address of the archive; used only when the user has no
    /// update source to derive one from.
    public var url: String
    public var bytes: Int
    public var sha256: String

    public init(name: String, url: String = "", bytes: Int, sha256: String) {
        self.name = name
        self.url = url
        self.bytes = bytes
        self.sha256 = sha256
    }

    /// Whether the manifest pins a published archive.
    public var isPublished: Bool { bytes > 0 && sha256.count == 64 }

    public static func load(from file: URL) -> ModelManifest? {
        guard let data = try? Data(contentsOf: file) else { return nil }
        return try? JSONDecoder().decode(ModelManifest.self, from: data)
    }

    /// The pinned manifest in the app's Resources, if the build has one.
    public static func bundled(_ bundle: Bundle = .main) -> ModelManifest? {
        bundle.url(forResource: "detector-model", withExtension: "json").flatMap(load(from:))
    }

    /// Where the archive is, derived from the user's update source: the
    /// `models` folder next to the update manifest (`<source>/models/<name>.mlmodelc.zip`).
    /// The source is a folder path, a `file://` URL, or an `https://` URL (or
    /// an `http://` one to a host on the local network, as for updates); a
    /// URL may name the folder or its `updates.json`.
    public static func archiveURL(updateSource: String, name: String) -> URL? {
        let text = updateSource.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        var base: URL
        if text.hasPrefix("/") {
            base = URL(fileURLWithPath: text, isDirectory: true)
        } else if let url = URL(string: text), let scheme = url.scheme?.lowercased() {
            switch scheme {
            case "file", "https": base = url
            case "http": guard let host = url.host, isLocalHost(host) else { return nil }; base = url
            default: return nil
            }
            guard url.user == nil, url.password == nil else { return nil }
        } else {
            return nil
        }
        if base.lastPathComponent == "updates.json" { base.deleteLastPathComponent() }
        return base.appendingPathComponent("models", isDirectory: true)
            .appendingPathComponent("\(name).mlmodelc.zip")
    }

    /// Loopback, private, link-local, `localhost`, or a `.local` name.
    static func isLocalHost(_ host: String) -> Bool {
        let h = host.lowercased()
        if h == "localhost" || h.hasSuffix(".local") || h == "::1" { return true }
        let parts = h.split(separator: ".").compactMap { Int($0) }
        guard parts.count == 4, parts.allSatisfy({ (0...255).contains($0) }) else {
            return h.hasPrefix("fe80:") || h.hasPrefix("fc") || h.hasPrefix("fd")
        }
        switch (parts[0], parts[1]) {
        case (127, _), (10, _), (192, 168), (169, 254): return true
        case (172, 16...31): return true
        default: return false
        }
    }
}

/// Where the model stands, in words the settings can show.
public enum ModelStatus: Sendable, Equatable {
    case checking
    case ready(URL)
    case downloading
    /// No model and no way to get one now: the reason says what to do.
    case missing(String)
    /// A download or its check failed.
    case failed(String)

    public var url: URL? { if case .ready(let u) = self { u } else { nil } }

    public var message: String {
        switch self {
        case .checking: "Checking for the detector model."
        case .ready: "The detector model is ready."
        case .downloading: "Downloading the detector model."
        case .missing(let why): "No detector model: \(why)"
        case .failed(let why): "The detector model could not be installed: \(why)"
        }
    }
}

/// Finds the detector model, or downloads it the first time smart alerts are
/// used: from the folder next to the user's update source, checked against
/// the pinned size and SHA-256 before it is unpacked, and stored in the
/// per-user Models folder. Without a model the host answers nothing (the
/// core fails open as designed).
public actor ModelInstaller {
    /// Fetches `url` into a temporary file and returns it.
    public typealias Fetch = @Sendable (URL) async throws -> URL
    /// Unpacks the archive into the folder.
    public typealias Unpack = @Sendable (_ archive: URL, _ into: URL) throws -> Void

    public private(set) var status: ModelStatus = .checking
    private let manifest: ModelManifest?
    private let folder: URL
    private let locate: @Sendable () -> URL?
    private let fetch: Fetch
    private let unpack: Unpack

    /// `folder` is the Models folder the downloaded model goes in (default:
    /// Application Support/NVRView/Models, where `DetectorModelLocator`
    /// looks).
    public init(
        manifest: ModelManifest?, folder: URL? = nil,
        locate: @escaping @Sendable () -> URL? = { DetectorModelLocator.locate() },
        fetch: @escaping Fetch = ModelInstaller.defaultFetch,
        unpack: @escaping Unpack = ModelInstaller.dittoUnpack
    ) {
        self.manifest = manifest
        self.folder =
            folder
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NVRView/Models", isDirectory: true)
        self.locate = locate
        self.fetch = fetch
        self.unpack = unpack
    }

    /// The model's location now, downloading it when it is not there and a
    /// source is set.
    @discardableResult
    public func ensure(updateSource: String?) async -> ModelStatus {
        if let url = locate() {
            status = .ready(url)
            return status
        }
        guard let manifest, manifest.isPublished else {
            status = .missing("this build pins no published model; place \(DetectorModelLocator.modelName).mlmodelc in the Models folder.")
            return status
        }
        let source = (updateSource ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let archive: URL?
        if !source.isEmpty {
            archive = ModelManifest.archiveURL(updateSource: source, name: manifest.name)
            if archive == nil {
                status = .missing("the update source cannot be used to find the model.")
                return status
            }
        } else if !manifest.url.isEmpty {
            archive = URL(string: manifest.url)
        } else {
            archive = nil
        }
        guard let archive else {
            status = .missing("set an update source in Settings, Updates; the model is fetched from its models folder.")
            return status
        }
        status = .downloading
        do {
            status = .ready(try await install(from: archive, manifest: manifest))
        } catch let error as InstallError {
            status = .failed(error.text)
        } catch {
            status = .failed("the download failed.")
        }
        return status
    }

    private enum InstallError: Error {
        case size, digest, contents, unpack
        var text: String {
            switch self {
            case .size: "the file's size differs from the pinned one."
            case .digest: "the file's checksum differs from the pinned one."
            case .contents: "the archive has no model in it."
            case .unpack: "the archive could not be unpacked."
            }
        }
    }

    private func install(from archive: URL, manifest: ModelManifest) async throws -> URL {
        let fm = FileManager.default
        let downloaded = try await fetch(archive)
        defer { try? fm.removeItem(at: downloaded) }
        // Checked before anything is unpacked.
        let size = (try? downloaded.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? -1
        guard size == manifest.bytes else { throw InstallError.size }
        guard Self.sha256(of: downloaded) == manifest.sha256.lowercased() else { throw InstallError.digest }
        try fm.createDirectory(at: folder, withIntermediateDirectories: true)
        let staging = folder.appendingPathComponent(".incoming-\(UUID().uuidString)", isDirectory: true)
        defer { try? fm.removeItem(at: staging) }
        do { try unpack(downloaded, staging) } catch { throw InstallError.unpack }
        let unpacked = staging.appendingPathComponent("\(manifest.name).mlmodelc", isDirectory: true)
        var isDirectory: ObjCBool = false
        guard fm.fileExists(atPath: unpacked.path, isDirectory: &isDirectory), isDirectory.boolValue else {
            throw InstallError.contents
        }
        let target = folder.appendingPathComponent("\(manifest.name).mlmodelc", isDirectory: true)
        try? fm.removeItem(at: target)
        try fm.moveItem(at: unpacked, to: target)
        return target
    }

    /// Streams the file through SHA-256 (the archive is tens of megabytes).
    static func sha256(of file: URL) -> String? {
        guard let handle = try? FileHandle(forReadingFrom: file) else { return nil }
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try? handle.read(upToCount: 1 << 20), !chunk.isEmpty { hasher.update(data: chunk) }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }

    // MARK: Defaults

    /// A folder or `file://` archive is copied; `https://` (or local `http://`)
    /// is downloaded, never following a redirect to plain `http://` from
    /// `https://`.
    @Sendable public static func defaultFetch(_ url: URL) async throws -> URL {
        let fm = FileManager.default
        let temp = fm.temporaryDirectory.appendingPathComponent("nvrview-model-\(UUID().uuidString).zip")
        if url.isFileURL {
            try fm.copyItem(at: url, to: temp)
            return temp
        }
        let session = URLSession(configuration: .ephemeral, delegate: RedirectGuard(), delegateQueue: nil)
        defer { session.finishTasksAndInvalidate() }
        let (file, response) = try await session.download(from: url)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            try? fm.removeItem(at: file)
            throw URLError(.badServerResponse)
        }
        try fm.moveItem(at: file, to: temp)
        return temp
    }

    /// `ditto -x -k`, as `scripts/detector-model.sh fetch` does.
    @Sendable public static func dittoUnpack(_ archive: URL, _ into: URL) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        process.arguments = ["-x", "-k", archive.path, into.path]
        process.standardError = FileHandle.nullDevice
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { throw URLError(.cannotDecodeContentData) }
    }

    private final class RedirectGuard: NSObject, URLSessionTaskDelegate {
        func urlSession(
            _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
            newRequest request: URLRequest
        ) async -> URLRequest? {
            let from = task.originalRequest?.url?.scheme?.lowercased()
            let to = request.url?.scheme?.lowercased()
            if from == "https" && to != "https" { return nil }
            if to == "http", let host = request.url?.host, !ModelManifest.isLocalHost(host) { return nil }
            return request
        }
    }
}
