import AppKit
import NVRCore
import Observation
import UniformTypeIdentifiers
import VideoKit
import AppBase

// Export from the Recordings window (docs/UX.md "Recordings", "Export"): in
// and out points, trims, a follow cut or grid as an MP4, range downloads,
// and still pictures. The core plans (FFI-RECORDINGS.md section 7a); the
// writers are NVRCore's `TrimExporter` and `EditExporter` and the picture
// encoder of Save Picture. Every export writes where the user chose in a
// save panel; the folder is remembered with a security-scoped bookmark.

/// The folder the last export went to, kept between launches as a
/// security-scoped bookmark (a sandboxed build keeps its access).
package struct ExportFolder {
    package static let key = "exportFolderBookmark"
    private let defaults: UserDefaults

    package init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    /// The remembered folder, if it still exists.
    package var url: URL? {
        guard let data = defaults.data(forKey: Self.key) else { return nil }
        var stale = false
        guard
            let url = (try? URL(resolvingBookmarkData: data, options: .withSecurityScope, relativeTo: nil, bookmarkDataIsStale: &stale))
                ?? (try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil, bookmarkDataIsStale: &stale))
        else { return nil }
        _ = url.startAccessingSecurityScopedResource()
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), isDirectory.boolValue else { return nil }
        if stale { remember(url) }
        return url
    }

    package func forget() { defaults.removeObject(forKey: Self.key) }

    package func remember(_ folder: URL) {
        let data = (try? folder.bookmarkData(options: .withSecurityScope, includingResourceValuesForKeys: nil, relativeTo: nil))
            ?? (try? folder.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil))
        if let data { defaults.set(data, forKey: Self.key) }
    }
}

/// The system panels an export asks its destination with; the self-test
/// replaces them.
package enum ExportPanels {
    @MainActor
    package static func save(name: String, directory: URL?, type: UTType) -> URL? {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = name
        panel.directoryURL = directory
        panel.allowedContentTypes = [type]
        panel.canCreateDirectories = true
        return panel.runModal() == .OK ? panel.url : nil
    }

    @MainActor
    package static func folder(message: String, directory: URL?) -> URL? {
        let panel = NSOpenPanel()
        panel.message = message
        panel.prompt = "Save Here"
        panel.directoryURL = directory
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        return panel.runModal() == .OK ? panel.url : nil
    }
}

/// One export running (or just ended), for the strip above the transport.
@MainActor
@Observable
package final class ExportJob {
    package let title: String
    package let control = ExportControl()
    package private(set) var fraction = 0.0
    package private(set) var running = true
    /// What happened, once it ended.
    package private(set) var result: String?
    /// The files written.
    package private(set) var files: [URL] = []
    @ObservationIgnored private var timer: Timer?

    package init(title: String) {
        self.title = title
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func poll() {
        if running { fraction = control.fraction }
    }

    package func cancel() { control.cancel() }

    package func finish(_ outcomes: [ExportOutcome]) {
        timer?.invalidate()
        timer = nil
        running = false
        files = outcomes.filter { $0.result == .done }.map(\.url)
        fraction = files.isEmpty ? fraction : 1
        if let failure = outcomes.compactMap({ o -> String? in if case .failed(let why) = o.result { why } else { nil } }).first {
            result = failure
        } else if outcomes.contains(where: { $0.result == .canceled }) {
            result = "Export canceled"
        } else {
            result = files.count == 1 ? "Saved \(files[0].lastPathComponent)" : "Saved \(files.count) files"
        }
    }
}

/// A range download being set up: the Export tab's range and checked cameras.
package struct RangeDownloadRequest: Equatable {
    package var startMs: Int64
    package var endMs: Int64
    package var cameras: Set<RecordingCamera>
    package var name = ""
}

/// What the export menu and keys keep: the points, the switches, the job.
@MainActor
@Observable
package final class ExportModel {
    /// The points the user set (device-local milliseconds); the window shows
    /// only those inside the scrubber's range (`RecordingsBrowser.points`).
    package var points = InOutPoints()
    /// No metadata of the app's own and neutral camera names in what is written.
    package var shareSafe: Bool { didSet { defaults.set(shareSafe, forKey: Self.shareSafeKey) } }
    /// Trim by re-encoding to start at the exact picture, instead of copying from the keyframe.
    package var trimExact = false
    package var codec = ExportCodec.h264
    package private(set) var job: ExportJob?
    @ObservationIgnored package var folder = ExportFolder() { didSet { folderPath = directFolder?.path } }
    /// The Export tab's choices: the kind (nil: the first that applies), the
    /// cameras checked (nil: all), the stream (nil: Settings' choice), sound.
    package var kind: ExportKind?
    package var cameras: Set<RecordingCamera>?
    package var qualityChoice: ExportQuality?
    package var sound = true
    /// Exports go straight into the remembered folder, with no save panel
    /// (set by "Choose…" in the tab; "Ask Each Time" clears it).
    package var direct: Bool { didSet { defaults.set(direct, forKey: Self.directKey); folderPath = directFolder?.path } }
    /// The folder shown in the tab, when exports go straight into one.
    package private(set) var folderPath: String?
    /// Settings > Capture's "Export from" (the app sets it).
    @ObservationIgnored package var defaultQuality: () -> ExportQuality = { .main }
    /// The save and folder panels (the self-test answers these itself).
    @ObservationIgnored package var saveURL: @MainActor (_ name: String, _ directory: URL?, _ type: UTType) -> URL? = ExportPanels.save
    @ObservationIgnored package var chooseFolder: @MainActor (_ message: String, _ directory: URL?) -> URL? = ExportPanels.folder
    @ObservationIgnored private let defaults: UserDefaults
    package static let shareSafeKey = "exportShareSafe"
    package static let directKey = "exportDirect"

    /// The stream a download reads: the tab's choice, else the setting.
    package var quality: ExportQuality { qualityChoice ?? defaultQuality() }

    /// The remembered folder when exports go straight into it.
    package var directFolder: URL? { direct ? folder.url : nil }

    /// "Choose…" in the tab: exports go straight into `url` from now on.
    package func choose(folder url: URL) {
        folder.remember(url)
        direct = true
        folderPath = url.path
    }

    /// "Ask Each Time": the save panels come back.
    package func askEachTime() { direct = false }

    package init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        shareSafe = defaults.bool(forKey: Self.shareSafeKey)
        direct = defaults.bool(forKey: Self.directKey)
        folderPath = direct ? ExportFolder(defaults: defaults).url?.path : nil
    }

    package var busy: Bool { job?.running == true }

    package func begin(_ title: String) -> ExportJob? {
        guard !busy else { return nil }
        let j = ExportJob(title: title)
        job = j
        return j
    }

    package func dismissJob() {
        if job?.running == false { job = nil }
    }

    /// The options every request carries: the share-safe switch and the
    /// remembered folder (`dest`), which the core checks.
    package func options(useFolder: Bool = true) -> ExportOptions {
        ExportOptions(dest: useFolder ? folder.url?.path : nil, shareSafe: shareSafe)
    }
}

extension RecordingsBrowser {
    // MARK: In and out points

    /// The scrubber's range as a closed range, if it spans anything.
    package var scrubSpan: ClosedRange<Int64>? {
        guard let r = scrubRange, r.end > r.start else { return nil }
        return r.start...r.end
    }

    /// The points inside the scrubber's range.
    package var points: InOutPoints { scrubSpan.map { export.points.kept(within: $0) } ?? InOutPoints() }

    /// The span the points select in the scrubber's range, nil when none is set or it is too short.
    package var pointsWindow: (start: Int64, end: Int64)? {
        guard let span = scrubSpan, !points.isEmpty else { return nil }
        return points.window(in: span)
    }

    package func markIn() {
        guard let span = scrubSpan else { return }
        export.points = points
        export.points.setIn(driver.transport.positionMs, within: span)
    }

    package func markOut() {
        guard let span = scrubSpan else { return }
        export.points = points
        export.points.setOut(driver.transport.positionMs, within: span)
    }

    package func clearPoints() { export.points.clear() }

    /// A press or drag on the scrubber moves a point.
    package func movePoint(_ handle: InOutPoints.Handle, to ms: Int64) {
        guard let span = scrubSpan else { return }
        export.points = points
        export.points.move(handle, to: ms, within: span)
    }

    // MARK: Destinations

    /// Where an export goes: one file by a save panel, several by a folder
    /// panel (their names from `suggested`); the folder is remembered.
    /// Names already taken in a chosen folder get " (2)" and so on.
    package func destinations(suggested: [String], type: UTType, message: String) -> [URL]? {
        guard let first = suggested.first else { return nil }
        if let folder = export.directFolder {
            var taken = Set<String>()
            return suggested.map { path in
                let url = Self.free(folder.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent), taken: taken)
                taken.insert(url.lastPathComponent)
                return url
            }
        }
        let startFolder = export.folder.url ?? URL(fileURLWithPath: first).deletingLastPathComponent()
        let directory = FileManager.default.fileExists(atPath: startFolder.path) ? startFolder : nil
        if suggested.count == 1 {
            guard let url = export.saveURL(URL(fileURLWithPath: first).lastPathComponent, directory, type) else { return nil }
            export.folder.remember(url.deletingLastPathComponent())
            return [url]
        }
        guard let folder = export.chooseFolder(message, directory) else { return nil }
        export.folder.remember(folder)
        var taken = Set<String>()
        return suggested.map { path in
            let name = URL(fileURLWithPath: path).lastPathComponent
            let url = Self.free(folder.appendingPathComponent(name), taken: taken)
            taken.insert(url.lastPathComponent)
            return url
        }
    }

    package static func free(_ url: URL, taken: Set<String>) -> URL {
        let stem = url.deletingPathExtension().lastPathComponent, ext = url.pathExtension
        var candidate = url
        var n = 1
        while FileManager.default.fileExists(atPath: candidate.path) || taken.contains(candidate.lastPathComponent) {
            n += 1
            candidate = url.deletingLastPathComponent().appendingPathComponent("\(stem) (\(n))" + (ext.isEmpty ? "" : "." + ext))
        }
        return candidate
    }

    /// Runs `work` as the window's export: off the main thread, with a
    /// progress strip and a cancel.
    package func run(_ title: String, work: @escaping @Sendable (ExportControl) async -> [ExportOutcome]) {
        guard let job = export.begin(title) else {
            message = "Wait for the export in progress, or cancel it"
            return
        }
        let control = job.control
        Task { @MainActor in
            let outcomes = await Task.detached(priority: .userInitiated) { await work(control) }.value
            job.finish(outcomes)
            exportFinished(job)
        }
    }

    private func exportFinished(_ job: ExportJob) {
        if let result = job.result { model.notice = result }
    }

    // MARK: Trim

    /// Trims the cameras (all of the incident's, when nil) to the in and out
    /// points. From the incident's download when it has one; otherwise the
    /// range is downloaded first, then trimmed.
    package func trim(cameras chosen: [RecordingCamera]?) {
        guard let window = pointsWindow ?? scrubSpan.map({ ($0.lowerBound, $0.upperBound) }) else {
            message = "Set an in point and an out point first (I and O)"
            return
        }
        guard !export.busy else {
            message = "Wait for the export in progress, or cancel it"
            return
        }
        if let e = editable {
            startTrim(download: e.download.download, cameras: chosen, window: window)
            return
        }
        let cameras = chosen ?? player.cameras
        guard !cameras.isEmpty else {
            message = "Play an incident or a camera to trim it"
            return
        }
        guard let queued = model.downloadRange(
            cameras: cameras, startMs: window.start, endMs: window.end, name: nil, options: ExportOptions(shareSafe: export.shareSafe),
            quality: export.quality)
        else { return }
        message = "Downloading that range first; the trim follows when it is done"
        model.whenFinished(queued.download) { [weak self] d in
            guard let self else { return }
            if d.state == "done" {
                self.message = nil
                self.startTrim(download: d.download, cameras: chosen, window: window)
            } else {
                self.message = "The range download failed, so there is nothing to trim"
            }
        }
    }

    /// Plans the trim with the core and writes it where the user says.
    package func startTrim(download: String, cameras: [RecordingCamera]?, window: (start: Int64, end: Int64)) {
        func request(_ options: ExportOptions) -> String {
            ExportRequest.trim(
                download: download, cameras: cameras, inMs: window.start, outMs: window.end, exact: export.trimExact, options: options)
        }
        var planned = model.trimPlan(json: request(export.options()))
        if case .failure = planned, export.options().dest != nil {
            planned = model.trimPlan(json: request(export.options(useFolder: false)))
        }
        let plan: TrimPlan
        switch planned {
        case .failure(let refusal):
            message = refusal.text
            return
        case .success(let p): plan = p
        }
        guard let urls = destinations(suggested: plan.outputs.map(\.suggestedFile), type: .mpeg4Movie, message: "Choose where to save the trimmed files"),
            urls.count == plan.outputs.count
        else { return }
        let exact = export.trimExact, shareSafe = export.shareSafe
        let jobs = Array(zip(plan.outputs, urls))
        run(jobs.count == 1 ? "Trimming" : "Trimming \(jobs.count) cameras") { control in
            var outcomes: [ExportOutcome] = []
            for (i, (output, url)) in jobs.enumerated() {
                let one = ExportControl()
                let watcher = Task {
                    while !Task.isCancelled {
                        if control.isCanceled { one.cancel() }
                        control.fraction = (Double(i) + one.fraction) / Double(jobs.count)
                        try? await Task.sleep(for: .milliseconds(50))
                    }
                }
                let outcome = await TrimExporter.export(output, exact: exact, to: url, shareSafe: shareSafe, control: one)
                watcher.cancel()
                outcomes.append(outcome)
                if outcome.result != .done { break }
            }
            return outcomes
        }
    }

    // MARK: Follow cut and grid

    /// Writes the follow cut or a grid of the incident's downloaded files,
    /// cut to the points when they are set, as an MP4.
    package func exportEdit(_ view: PlayerView) {
        guard let kind = view.edit else { return }
        guard let e = editable else {
            message = "Download the incident to export a follow cut or a grid"
            return
        }
        guard !export.busy else {
            message = "Wait for the export in progress, or cancel it"
            return
        }
        let layout: String? = if case .grid(let l) = kind { l } else { nil }
        let window = pointsWindow
        func request(_ options: ExportOptions) -> String {
            ExportRequest.edit(grid: layout, download: e.download.download, window: window, options: options)
        }
        var planned = model.editPlan(json: request(export.options()))
        if case .failure = planned, export.options().dest != nil {
            planned = model.editPlan(json: request(export.options(useFolder: false)))
        }
        let plan: EditPlan
        switch planned {
        case .failure(let refusal):
            message = refusal.text
            return
        case .success(let p): plan = p
        }
        guard let url = destinations(suggested: [plan.suggestedFile], type: .mpeg4Movie, message: "Choose where to save the video")?.first
        else { return }
        let settings = EditExporter.Settings(codec: export.codec, shareSafe: export.shareSafe, gridCell: editPlayer.gridSound, sound: export.sound)
        run(view == .follow ? "Exporting the follow cut" : "Exporting the \(view.rawValue) grid") { control in
            [await EditExporter.export(plan: plan, to: url, settings: settings, control: control)]
        }
    }

    // MARK: Range download

    /// Queues the Export tab's download; the Downloads tab lists it.
    package func downloadRange(_ request: RangeDownloadRequest) {
        let cameras = self.cameras.filter { request.cameras.contains($0) }
        var options = ExportOptions(shareSafe: export.shareSafe)
        if let folder = export.directFolder { options.dest = folder.path }
        guard !cameras.isEmpty else {
            message = "Choose at least one camera"
            return
        }
        if model.downloadRange(
            cameras: cameras, startMs: request.startMs, endMs: request.endMs, name: request.name, options: options,
            quality: export.quality) != nil
        {
            message = "Download queued; its progress is in Downloads"
        }
    }

    // MARK: Still pictures

    /// Saves the current picture, or one per camera, as PNG or JPEG.
    package func saveStills(eachCamera: Bool) {
        guard !export.busy else {
            message = "Wait for the export in progress, or cancel it"
            return
        }
        if playerView == .cameras { saveDeviceStills(eachCamera: eachCamera) } else { saveEditStills(eachCamera: eachCamera) }
    }

    private var stillFormat: PictureFormat { model.app?.pictureFormat ?? .png }

    private func stillPlan(_ json: String) -> StillPlan? {
        switch model.stillPlan(json: json) {
        case .failure(let refusal):
            message = refusal.text
            return nil
        case .success(let p): return p
        }
    }

    private func stillType(_ suggested: String) -> UTType { PictureFormat(extension: URL(fileURLWithPath: suggested).pathExtension) == .jpeg ? .jpeg : .png }

    /// The pictures the device playback's tiles show now.
    private func saveDeviceStills(eachCamera: Bool) {
        guard let playback = player.playback, !player.cells.isEmpty else {
            message = "Play an incident or a camera to save its picture"
            return
        }
        let shown = PlayerGrid.audibleIndex(player.mode, count: player.cells.count) ?? 0
        let checked = exportChecked
        let cells = eachCamera ? player.cells.filter { checked.contains($0.camera) } : [player.cells[min(shown, player.cells.count - 1)]]
        let tiles = cells.map(\.tile.id)
        guard let plan = stillPlan(ExportRequest.stills(playback: playback, tiles: tiles, format: stillFormat, options: export.options(useFolder: false))),
            let urls = destinations(suggested: plan.stills.map(\.suggestedFile), type: stillType(plan.stills[0].suggestedFile), message: "Choose where to save the pictures")
        else { return }
        var jobs: [(PictureRender, URL)] = []
        for (still, url) in zip(plan.stills, urls) {
            guard let cell = cells.first(where: { $0.tile.id == still.tile }), let picture = cell.tile.latestPicture() else { continue }
            let color = cell.tile.snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            jobs.append((
                PictureRender(
                    picture: picture, color: color, source: CGRect(x: 0, y: 0, width: picture.width, height: picture.height),
                    width: picture.width, height: picture.height), url
            ))
        }
        writeStills(jobs)
    }

    /// The edit's picture at the playhead, or each camera's picture there.
    private func saveEditStills(eachCamera: Bool) {
        guard let plan = editPlayer.plan, let session = editPlayer.session else {
            message = "The follow cut or grid is not open"
            return
        }
        let at = editPlayer.positionMs
        let format = stillFormat
        if !eachCamera {
            let stem = URL(fileURLWithPath: plan.suggestedFile).deletingPathExtension().lastPathComponent
            let name = "\(stem) \(TransportState.clock(at).replacingOccurrences(of: ":", with: ".")).\(format.fileExtension)"
            let folder = export.folder.url ?? URL(fileURLWithPath: plan.suggestedFile).deletingLastPathComponent()
            guard let url = destinations(suggested: [folder.appendingPathComponent(name).path], type: format == .jpeg ? .jpeg : .png, message: "")?.first
            else { return }
            let size = ExportGeometry.outputSize(plan)
            run("Saving the picture") { control in
                [await Task.detached { () -> ExportOutcome in
                    guard let renderer = try? GridRenderer(),
                        let image = session.render(at: at, width: size.width, height: size.height, with: renderer),
                        let data = PictureEncoder.encode(image, as: PictureFormat(extension: url.pathExtension)?.encoding ?? .png)
                    else { return ExportOutcome(result: .failed("The picture could not be drawn"), url: url) }
                    do {
                        try data.write(to: url, options: .atomic)
                        return ExportOutcome(result: .done, url: url, width: image.width, height: image.height, bytes: Int64(data.count))
                    } catch {
                        return ExportOutcome(result: .failed("The picture could not be written"), url: url)
                    }
                }.value]
            }
            return
        }
        var stills: [StillPlan.Still] = []
        let checked = exportChecked
        for source in plan.sources where checked.contains(source.camera) {
            let request = ExportRequest.still(
                download: plan.download, camera: source.camera, atMs: at, exact: true, format: format, options: export.options(useFolder: false))
            if let p = stillPlan(request) { stills += p.stills }
        }
        guard !stills.isEmpty,
            let urls = destinations(suggested: stills.map(\.suggestedFile), type: stillType(stills[0].suggestedFile), message: "Choose where to save the pictures")
        else { return }
        let items = Array(zip(stills, urls))
        run("Saving \(items.count) pictures") { control in
            var outcomes: [ExportOutcome] = []
            for (i, (still, url)) in items.enumerated() {
                guard let file = still.file, let ms = still.sourceInMs,
                    let job = await StillSource.fromFile(file, atMs: ms, exact: true)
                else {
                    outcomes.append(ExportOutcome(result: .failed("A picture could not be read from the file"), url: url))
                    break
                }
                let saved = await PictureSaver.save(job, to: url, defaultFormat: format, renderer: PictureRenderer())
                switch saved {
                case .success(let s): outcomes.append(ExportOutcome(result: .done, url: s.url, width: s.width, height: s.height, bytes: Int64(s.bytes)))
                case .failure: outcomes.append(ExportOutcome(result: .failed("A picture could not be written"), url: url))
                }
                control.fraction = Double(i + 1) / Double(items.count)
            }
            return outcomes
        }
    }

    private func writeStills(_ jobs: [(PictureRender, URL)]) {
        guard !jobs.isEmpty else {
            message = "There is no picture to save yet"
            return
        }
        let format = stillFormat
        run(jobs.count == 1 ? "Saving the picture" : "Saving \(jobs.count) pictures") { control in
            var outcomes: [ExportOutcome] = []
            let renderer = PictureRenderer()
            for (i, (job, url)) in jobs.enumerated() {
                switch await PictureSaver.save(job, to: url, defaultFormat: format, renderer: renderer) {
                case .success(let s): outcomes.append(ExportOutcome(result: .done, url: s.url, width: s.width, height: s.height, bytes: Int64(s.bytes)))
                case .failure: outcomes.append(ExportOutcome(result: .failed("A picture could not be written"), url: url))
                }
                control.fraction = Double(i + 1) / Double(jobs.count)
            }
            return outcomes
        }
    }
}
