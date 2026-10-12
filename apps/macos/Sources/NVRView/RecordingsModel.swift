import AppKit
import Foundation
import NVRCore
import Observation
import VideoKit

/// Which cameras of a device the user lets the recordings screens search
/// (`nv_recordings_enable`), saved between launches. The core does not keep
/// this list: the app enables every listed device again at each launch.
struct RecordingsEnablement: Codable, Equatable, Sendable {
    var device: String
    var cameras: [RecordingCamera]
}

/// The recordings side of the app (docs/UX.md "Recordings"), shared by every
/// recordings window: configuration, which cameras are searched, the core's
/// recordings events, cover thumbnails (decoded and shrunk here, then put
/// back into the core's cache), and downloads.
///
/// Main actor only. Every call it makes answers from the core's memory or
/// queues device work that reports by event (docs/FFI-RECORDINGS.md section
/// 9), so nothing here waits on a device; decoding a thumbnail runs on a
/// background queue.
@MainActor
@Observable
final class RecordingsModel {
    @ObservationIgnored private let bridge: CoreBridge
    @ObservationIgnored private let enablementFile: JSONFileStore<RecordingsEnablement>
    /// The app, for device and camera names.
    @ObservationIgnored weak var app: AppModel?

    /// False until the core accepted the configuration (a core without
    /// recordings, minor < 5, never does; the screens then stay hidden).
    private(set) var available = false
    /// Where downloads go (`downloads_dir`).
    private(set) var downloadsFolder: URL?
    /// The searched cameras, by device key.
    private(set) var enabled: [String: [RecordingCamera]] = [:]
    /// Each enabled device's search lane: `idle`, `busy`, `waiting_device`, `ended`.
    private(set) var indexStates: [String: (state: String, pendingDays: Int)] = [:]
    /// Grows with every `recordings_changed` (and enablement change).
    private(set) var indexRevision = 0
    /// Grows with every `incidents_changed`.
    private(set) var incidentsRevision = 0
    /// Camera-days the device could not search (not retried by the core).
    private(set) var failedDays: [RecordingCamera: Set<Int64>] = [:]
    /// The camera groups for incidents.
    private(set) var groups = IncidentGroups()
    /// Every known download, in the order they appeared.
    private(set) var downloads: [DownloadProgress] = []
    /// Cover thumbnails the screens show.
    private(set) var thumbnails = ThumbnailBoard()
    /// How many keyframes were decoded into small images and put back.
    private(set) var thumbnailsPut = 0
    /// A message for the user (shown at the bottom of a recordings window).
    var notice: String?

    @ObservationIgnored private var board = DownloadBoard()
    @ObservationIgnored private var publishScheduled = false
    @ObservationIgnored private var listeners: [UUID: () -> Void] = [:]
    @ObservationIgnored private var changeScheduled = false
    @ObservationIgnored private var nextHold: UInt64 = 1
    @ObservationIgnored private var closed = false
    @ObservationIgnored private let decodeQueue = DispatchQueue(label: "nvrview.thumbnails", qos: .utility)
    /// Moves a download's folder to the Trash (the system's recycle; the
    /// self-test records it instead).
    @ObservationIgnored var trasher: (URL) -> Void = { url in
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        NSWorkspace.shared.recycle([url]) { _, error in
            if let error { NSLog("could not move a download to the Trash: %@", error.localizedDescription) }
        }
    }
    /// Shows files in the Finder (the self-test records it instead).
    @ObservationIgnored var revealer: ([URL]) -> Void = { NSWorkspace.shared.activateFileViewerSelecting($0) }
    /// The computer's time (fixed by tests).
    @ObservationIgnored var now: () -> Date = { Date() }

    init(bridge: CoreBridge, enablementFile: JSONFileStore<RecordingsEnablement>) {
        self.bridge = bridge
        self.enablementFile = enablementFile
    }

    /// The device's offset from UTC: the computer's (FFI-RECORDINGS.md,
    /// Decision 6, until the core reads the device's own zone).
    var utcOffset: Int { TimeZone.current.secondsFromGMT(for: now()) }

    /// Today on the devices.
    var today: Int64 { DeviceTime.today(now: now(), utcOffset: utcOffset) }

    private func core() -> (any CoreAPI)? {
        guard !closed else { return nil }
        return try? bridge.recordings()
    }

    // MARK: Start and stop

    /// Configures recordings with a downloads folder, enables the saved
    /// cameras, and reads the groups and downloads. Without recordings in the
    /// core, nothing is shown.
    func start(downloads: URL) {
        guard let core = core() else { return }
        try? FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
        let config = #"{"downloads_dir":\#(Self.quoted(downloads.path))}"#
        do throws(CoreStatus) {
            try core.recordingsConfigure(json: config)
        } catch {
            if error != .unsupported { NSLog("recordings configuration refused: %@", String(describing: error)) }
            return
        }
        available = true
        downloadsFolder = downloads
        for e in enablementFile.load() where !e.cameras.isEmpty {
            enabled[e.device] = e.cameras
            applyEnablement(e.device)
        }
        reloadGroups()
        if let list = (try? core.downloadsGet()).flatMap(DownloadList.parse) {
            board.load(list)
            self.downloads = board.downloads
        }
    }

    /// The app quits: every open playback closes first (its plays stop on
    /// the device), then nothing more is asked of the core.
    func shutdown() {
        for player in players.values.compactMap(\.value) { player.close() }
        players.removeAll()
        closed = true
    }

    private static func quoted(_ s: String) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: [s], options: [.withoutEscapingSlashes])) ?? Data()
        return String(decoding: data.dropFirst().dropLast(), as: UTF8.self)
    }

    // MARK: Windows

    /// Calls `body` (once per run of changes) when the index, the
    /// incidents, the groups, or the enabled cameras change. Keep the token.
    func subscribe(_ body: @escaping () -> Void) -> UUID {
        let token = UUID()
        listeners[token] = body
        return token
    }

    func unsubscribe(_ token: UUID) { listeners[token] = nil }

    private func changed() {
        guard !changeScheduled else { return }
        changeScheduled = true
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.changeScheduled = false
                for body in self.listeners.values { body() }
            }
        }
    }

    /// A hold number for a window (`nv_recordings_hold_today`).
    func newHold() -> UInt64 {
        defer { nextHold += 1 }
        return nextHold
    }

    // MARK: Enabled cameras

    /// Every camera the recordings screens search, in device and channel order.
    var enabledCameras: [RecordingCamera] {
        enabled.keys.sorted().flatMap { enabled[$0] ?? [] }
    }

    func isEnabled(_ camera: RecordingCamera) -> Bool { enabled[camera.device]?.contains(camera) == true }

    /// Turns searching one camera on or off.
    func setEnabled(_ camera: RecordingCamera, _ on: Bool) {
        var list = enabled[camera.device] ?? []
        if on, !list.contains(camera) {
            list.append(camera)
            list.sort { ($0.channel, $0.lens) < ($1.channel, $1.lens) }
        }
        if !on { list.removeAll { $0 == camera } }
        enabled[camera.device] = list.isEmpty ? nil : list
        applyEnablement(camera.device)
        saveEnablement()
        indexRevision += 1
        changed()
    }

    /// Every camera of a device on, or all off.
    func setEnabled(device: String, cameras: [RecordingCamera]) {
        enabled[device] = cameras.isEmpty ? nil : cameras
        applyEnablement(device)
        saveEnablement()
        indexRevision += 1
        changed()
    }

    private func applyEnablement(_ device: String) {
        guard available, let core = core() else { return }
        let json = enabled[device].map { RecordingsRequest.enable($0, utcOffset: utcOffset) }
        do throws(CoreStatus) {
            try core.recordingsEnable(deviceKey: device, json: json)
        } catch {
            notice = error == .unsupported
                ? "This device has no recordings to search" : "Could not search this device (status \(error.code))"
        }
    }

    private func saveEnablement() {
        enablementFile.save(enabled.keys.sorted().map { RecordingsEnablement(device: $0, cameras: enabled[$0] ?? []) })
    }

    /// The core named a device differently (first connection): its cameras
    /// follow.
    func rekey(_ old: String, to new: String) {
        guard old != new, let cams = enabled.removeValue(forKey: old) else { return }
        if available, let core = core() { try? core.recordingsEnable(deviceKey: old, json: nil) }
        enabled[new] = cams.map { RecordingCamera(device: new, channel: $0.channel, lens: $0.lens) }
        applyEnablement(new)
        saveEnablement()
        changed()
    }

    /// A device the user removed is no longer searched.
    func deviceRemoved(_ key: String) {
        guard enabled[key] != nil else { return }
        setEnabled(device: key, cameras: [])
    }

    // MARK: The index

    /// Asks for `day` of `cameras` to be searched (and refreshes it with
    /// `force`). Returns a message when the core refused.
    @discardableResult
    func want(_ cameras: [RecordingCamera], days: [Int64], force: Bool = false) -> String? {
        guard available, !cameras.isEmpty, !days.isEmpty, let core = core() else { return nil }
        for c in cameras {
            for d in days { failedDays[c]?.remove(d) }
        }
        do throws(CoreStatus) {
            try core.recordingsWant(json: RecordingsRequest.want(cameras, days: days, force: force))
            return nil
        } catch {
            return error == .limit ? "Too many days are waiting to be searched; try again shortly" : "Could not search (status \(error.code))"
        }
    }

    /// Asks for months' calendars alone (minor 7; a calendar paged to
    /// another month). Returns a message when the core refused.
    @discardableResult
    func want(_ cameras: [RecordingCamera], months: [Int64]) -> String? {
        guard available, !cameras.isEmpty, !months.isEmpty, let core = core() else { return nil }
        do throws(CoreStatus) {
            try core.recordingsWant(json: RecordingsRequest.want(cameras, months: months))
            return nil
        } catch {
            return error == .limit ? "Too many days are waiting to be searched; try again shortly" : nil
        }
    }

    /// Keeps today fresh for a window's cameras (`cameras` nil releases).
    func hold(_ hold: UInt64, cameras: [RecordingCamera]?) {
        guard available, let core = core() else { return }
        let json = cameras.flatMap { $0.isEmpty ? nil : RecordingsRequest.hold($0) }
        try? core.recordingsHoldToday(hold, json: json)
    }

    /// Whether some enabled device is searching (and how many camera-days wait).
    var searching: (busy: Bool, pendingDays: Int, waitingDevice: Bool) {
        let states = indexStates.filter { enabled[$0.key] != nil }.values
        return (
            states.contains { $0.state == "busy" }, states.reduce(0) { $0 + $1.pendingDays },
            states.contains { $0.state == "waiting_device" || $0.state == "ended" }
        )
    }

    // MARK: Queries (memory only)

    /// Runs a query, nil when it fails (not configured, not available).
    private func query<T: Decodable>(_ call: (any CoreAPI) throws(CoreStatus) -> String) -> T? {
        guard available, let core = core() else { return nil }
        guard let json = try? call(core) else { return nil }
        return T.parse(answer: json)
    }

    func calendar(start: Int64, end: Int64, cameras: [RecordingCamera]) -> CalendarAnswer? {
        guard !cameras.isEmpty else { return nil }
        let q = RecordingsRequest.query(start: start, end: end, cameras: cameras)
        return query { core throws(CoreStatus) in try core.recordingsCalendar(query: q) }
    }

    func spans(start: Int64, end: Int64, cameras: [RecordingCamera]) -> SpansAnswer? {
        guard !cameras.isEmpty else { return nil }
        let q = RecordingsRequest.query(start: start, end: end, cameras: cameras)
        return query { core throws(CoreStatus) in try core.recordingsSpans(query: q) }
    }

    func events(start: Int64, end: Int64, cameras: [RecordingCamera]) -> EventsAnswer? {
        guard !cameras.isEmpty else { return nil }
        let q = RecordingsRequest.query(start: start, end: end, cameras: cameras)
        return query { core throws(CoreStatus) in try core.recordingsEvents(query: q) }
    }

    func incidents(start: Int64, end: Int64, cameras: [RecordingCamera]?, group: String?) -> IncidentsAnswer? {
        let q = RecordingsRequest.query(start: start, end: end, cameras: cameras, group: group)
        return query { core throws(CoreStatus) in try core.incidentsGet(query: q) }
    }

    func incident(_ key: String) -> Incident? {
        let answer: IncidentAnswer? = query { core throws(CoreStatus) in try core.incidentGet(key: key) }
        return answer?.incident
    }

    // MARK: Incidents

    /// Applies an edit; the incidents reload on the `incidents_changed` that
    /// follows. Returns the resulting keys, or nil (with `notice` set).
    @discardableResult
    func edit(_ edit: IncidentEdit) -> [String]? {
        guard let core = core() else { return nil }
        do throws(CoreStatus) {
            let answer = try core.incidentEdit(json: edit.json)
            notice = nil
            return EditAnswer.parse(answer: answer)?.keys ?? []
        } catch {
            notice = switch error {
            case .invalidArgument: "That edit cannot be made (an incident would be left without events or cameras)"
            case .unknownID: "That incident changed; try again"
            case .limit: "Too many edits"
            default: "The edit failed (status \(error.code))"
            }
            return nil
        }
    }

    /// Locks or unlocks an incident; tells the user when its video is not
    /// being kept.
    @discardableResult
    func lock(_ key: String, _ locked: Bool) -> LockAnswer? {
        guard let core = core() else { return nil }
        do throws(CoreStatus) {
            let answer = LockAnswer.parse(answer: try core.incidentLock(key: key, locked: locked))
            notice = locked ? answer?.message : "Unlocked. Downloaded files stay where they are."
            return answer
        } catch {
            notice = error == .limit
                ? "Too many locked incidents (or one longer than 31 days)" : "Could not change the lock (status \(error.code))"
            return nil
        }
    }

    // MARK: Camera groups

    private func reloadGroups() {
        guard let core = core(), let json = try? core.cameraGroupsGet() else { return }
        groups = IncidentGroups.parse(answer: json) ?? IncidentGroups()
    }

    /// Replaces the camera groups. Renaming or changing a group changes the
    /// keys of its incidents (edits of locked incidents follow).
    @discardableResult
    func putGroups(_ new: IncidentGroups) -> Bool {
        guard let core = core() else { return false }
        do throws(CoreStatus) {
            try core.cameraGroupsPut(json: new.putJSON)
            reloadGroups()
            return true
        } catch {
            notice = error == .limit ? "Too many groups or cameras (64 of each)" : "Could not save the groups (status \(error.code))"
            return false
        }
    }

    // MARK: Thumbnails

    /// A view shows `key`: asks the core for it unless it is known.
    func requestThumbnail(_ key: ThumbnailKey) {
        guard available else { return }
        guard thumbnails.needsRequest(key) else {
            thumbnails.touch(key)
            return
        }
        fetchThumbnail(key)
    }

    private func fetchThumbnail(_ key: ThumbnailKey) {
        guard let core = core() else { return }
        do throws(CoreStatus) {
            switch try core.thumbnailGet(key: key.json) {
            case .pending: thumbnails.set(key, .pending)
            case .ready(let t): use(t, for: key)
            }
        } catch {
            thumbnails.set(key, .failed("status \(error.code)"))
        }
    }

    /// A cached thumbnail: the app's image as is, a keyframe decoded and
    /// shrunk on the decode queue, then put back in its place.
    private func use(_ t: Thumbnail, for key: ThumbnailKey) {
        if t.kind == .image {
            thumbnails.set(key, .image(t.data))
            return
        }
        thumbnails.set(key, .decoding)
        let codec: VideoCodec = t.kind == .h265Keyframe ? .h265 : .h264
        let data = t.data, pts = t.ptsMicros
        decodeQueue.async { [weak self] in
            let small = KeyframeImage.thumbnail(annexB: data, codec: codec)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self else { return }
                    if let small {
                        // Back on the main thread, where the core is freed,
                        // so a put never races the shutdown. It copies and
                        // returns at once.
                        let image = Thumbnail(
                            kind: .image, width: UInt32(small.width), height: UInt32(small.height), ptsMicros: pts,
                            data: small.jpeg)
                        if let core = self.core(), (try? core.thumbnailPut(key: key.json, thumbnail: image)) != nil {
                            self.thumbnailsPut += 1
                        }
                        self.thumbnails.set(key, .image(small.jpeg))
                    } else {
                        self.thumbnails.set(key, .failed("the picture could not be decoded"))
                    }
                }
            }
        }
    }

    /// The window moved on: drops the fetches still queued for it.
    func cancelThumbnails() {
        guard let core = core() else { return }
        try? core.thumbnailCancel(key: nil)
        thumbnails.forgetPending()
    }

    // MARK: Downloads

    /// Queues an incident's download (every camera, main stream).
    func download(incident key: String) {
        guard let core = core() else { return }
        do throws(CoreStatus) {
            let answer = QueuedDownload.parse(answer: try core.downloadIncident(json: RecordingsRequest.download(incident: key)))
            notice = answer?.existing == true ? "Already downloaded or downloading" : nil
        } catch {
            notice = switch error {
            case .notConfigured: "There is no downloads folder"
            case .limit: "Too many downloads; forget finished ones first"
            case .unknownID: "That incident changed; try again"
            default: "Could not download (status \(error.code))"
            }
        }
    }

    func pause(_ id: String, _ paused: Bool) {
        guard let core = core() else { return }
        do throws(CoreStatus) { try core.downloadPause(id, camera: nil, paused: paused) } catch {
            notice = "Could not \(paused ? "pause" : "resume") (status \(error.code))"
        }
    }

    func retry(_ id: String) {
        guard let core = core() else { return }
        do throws(CoreStatus) { try core.downloadRetry(id, camera: nil) } catch {
            notice = "Could not retry (status \(error.code))"
        }
    }

    /// Forgets a download (its files stay); a running one is paused first.
    func forget(_ id: String) {
        guard let core = core() else { return }
        do throws(CoreStatus) { try core.downloadForget(id, deletePartial: true) } catch {
            notice = error == .invalidArgument ? "Pause the download first" : "Could not forget (status \(error.code))"
        }
    }

    /// Moves a download's folder to the Trash, once the core has stopped
    /// writing there (its `download_removed` event).
    func trash(_ id: String) {
        guard let core = core() else { return }
        do throws(CoreStatus) {
            guard let answer = TrashAnswer.parse(answer: try core.downloadTrash(id)) else { return }
            board.trashRequested(id, folder: answer.folder)
            publishDownloads()
        } catch {
            notice = "Could not move it to the Trash (status \(error.code))"
        }
    }

    /// Shows a download's files (or its folder) in the Finder.
    func reveal(_ d: DownloadProgress) {
        let files = d.outputs.map { URL(fileURLWithPath: $0) }
        if !files.isEmpty {
            revealer(files)
        } else if let folder = d.folder {
            revealer([URL(fileURLWithPath: folder)])
        }
    }

    private func publishDownloads() {
        publishScheduled = false
        downloads = board.downloads
    }

    // MARK: Playback (minor 6)

    /// A weak reference to a window's player, for routing events.
    private final class PlayerRef {
        weak var value: RecordingsPlayer?
        init(_ p: RecordingsPlayer) { value = p }
    }

    @ObservationIgnored private var players: [UInt64: PlayerRef] = [:]

    func makePlayer() -> RecordingsPlayer { RecordingsPlayer(model: self) }

    /// Opens a playback for `player` (its events go to it).
    func openPlayback(json: String, cameras: [RecordingCamera], player: RecordingsPlayer) throws(CoreStatus)
        -> CoreBridge.OpenedTiles
    {
        guard available, !closed else { throw .unsupported }
        let opened = try bridge.openPlayback(json: json, cameras: cameras.map { (UInt8($0.channel), .sub) })
        players[opened.id] = PlayerRef(player)
        return opened
    }

    func closePlayback(_ playback: UInt64, tiles: [CoreTile]) {
        players[playback] = nil
        bridge.closePlayback(playback, tiles: tiles)
    }

    func forgetPlaybackTiles(_ tiles: [CoreTile]) { bridge.forgetTiles(tiles) }

    func removePlaybackCamera(_ playback: UInt64, tile: CoreTile) {
        bridge.removePlaybackCamera(playback, tile: tile)
    }

    func addPlaybackCamera(_ playback: UInt64, camera: RecordingCamera) -> CoreTile? {
        do throws(CoreStatus) {
            return try bridge.addPlaybackCamera(
                playback, json: PlaybackCamera(camera: camera).json, channel: UInt8(camera.channel), tier: .sub)
        } catch {
            notice = error == .limit ? "At most four cameras play at once" : "Could not add the camera (status \(error.code))"
            return nil
        }
    }

    /// A playback call that returns at once (play, pause, seek, rate).
    func playbackCall(_ body: (any CoreAPI) throws(CoreStatus) -> Void) {
        guard let core = core() else { return }
        do throws(CoreStatus) { try body(core) } catch {
            if error != .unknownID { notice = "The player could not do that (status \(error.code))" }
        }
    }

    func playbackPosition(_ playback: UInt64) -> PlaybackPosition? {
        guard !closed else { return nil }
        return bridge.playbackPosition(playback)
    }

    /// A playback tile's sound through the core's tile audio path.
    func setPlaybackAudio(_ tile: CoreTile, audible: Bool, volume: Float) {
        guard !closed else { return }
        try? bridge.setTileAudio(tile, audible: audible, volume: volume)
    }

    func applyPlayback(_ e: PlaybackEvent) {
        let id: UInt64
        switch e {
        case .state(let p, _, _, _), .camera(let p, _, _, _), .seek(let p, _, _, _), .closed(let p, _): id = p
        }
        guard let player = players[id]?.value else { return }
        if case .closed = e { players[id] = nil }
        player.apply(e)
    }

    // MARK: Events

    func apply(_ e: RecordingsEvent) {
        guard !closed else { return }
        switch e {
        case .index(let device, let state, let pending, _):
            indexStates[device] = (state, pending)
        case .dayFailed(_, let camera, let day, let reason):
            if let camera { failedDays[camera, default: []].insert(DeviceTime.dayStart(day)) }
            NSLog("a day of recordings could not be searched: %@", reason ?? "no reason given")
            changed()
        case .changed:
            indexRevision += 1
            changed()
        case .incidentsChanged:
            incidentsRevision += 1
            changed()
        case .cameraGroupsChanged:
            reloadGroups()
            changed()
        case .thumbnail(let key, let state, let reason):
            thumbnailEvent(key, state, reason)
        case .download(let d):
            // State changes show at once; progress alone at most 4 times a
            // second (the core already sends no more than that per download).
            if board.apply(d) {
                publishDownloads()
            } else if !publishScheduled {
                publishScheduled = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
                    MainActor.assumeIsolated {
                        guard let self, self.publishScheduled else { return }
                        self.publishDownloads()
                    }
                }
            }
        case .soundWatch, .soundAnalyze:
            break  // the loudness lanes (M3) read the envelope on demand
        case .downloadRemoved(let id, let trash, let folder):
            if let folder = board.removed(id, trash: trash, folder: folder) {
                trasher(URL(fileURLWithPath: folder))
            }
            publishDownloads()
        }
    }

    private func thumbnailEvent(_ key: ThumbnailKey, _ state: String, _ reason: String?) {
        // Only what a view asked for (another window's request counts too).
        guard let current = thumbnails[key], current == .pending else { return }
        switch state {
        case "ready": fetchThumbnail(key)
        case "empty": thumbnails.set(key, .empty)
        case "dropped": thumbnails.set(key, .dropped)
        default: thumbnails.set(key, .failed(reason))
        }
    }
}
