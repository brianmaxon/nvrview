import Foundation
import NVRCore
import Observation

/// What one recordings window shows (docs/UX.md "Recordings"): a day of the
/// enabled cameras, its incidents with the filters, the timeline lanes, the
/// calendar's month, and the player. Asks the core for the day it shows
/// (`nv_recordings_want`) and for a month the calendar pages to (`months`,
/// minor 7), and holds today while today is shown, under its own hold
/// number; releases the hold when it closes or moves to another day. Every
/// query answers from the core's memory; device work arrives as events,
/// after which the model calls `reload`. Selecting one incident plays it
/// (`RecordingsPlayer`).
@MainActor
@Observable
final class RecordingsBrowser {
    /// What the middle of the window shows.
    enum Center: String, CaseIterable, Identifiable {
        case player = "Player"
        case downloads = "Downloads"
        var id: String { rawValue }
    }

    /// The scrubber's span: the selected incident (with its margins) or the
    /// whole day.
    enum ScrubScope: String, CaseIterable, Identifiable {
        case incident = "Incident"
        case day = "Day"
        var id: String { rawValue }
    }

    /// Which incidents the list shows: all, a camera group's, or a camera's.
    enum Scope: Hashable {
        case all
        case group(String)
        case camera(RecordingCamera)
    }

    /// One timeline lane: a camera's recording spans and events for the day.
    struct Lane: Identifiable, Equatable {
        var camera: RecordingCamera
        var name: String
        var spans: [RecordingSpan]
        var events: [RecordedEvent]
        /// The device could not search this camera's day.
        var failed: Bool
        var id: RecordingCamera { camera }
    }

    @ObservationIgnored let model: RecordingsModel
    /// This window's `nv_recordings_hold_today` number.
    @ObservationIgnored let holdID: UInt64
    @ObservationIgnored private var token: UUID?
    /// The cameras the shown day was last asked for.
    @ObservationIgnored private var asked: (day: Int64, cameras: Set<RecordingCamera>)?
    /// The cameras each month was asked for alone (calendar paging).
    @ObservationIgnored private var askedMonths: [Int64: Set<RecordingCamera>] = [:]
    @ObservationIgnored private var heldCameras: [RecordingCamera] = []

    /// The player of this window.
    let player: RecordingsPlayer

    /// The shown day (device-local, its first second).
    private(set) var day: Int64
    /// The calendar's month (its first second).
    private(set) var month: Int64
    var center = Center.player
    var scrubScope = ScrubScope.incident
    /// The side panes the user wants (a narrow window may still hide them;
    /// `RecordingsPanes.shown`).
    var showSidebar = true
    var showInspector = true
    /// What the window last laid out: its width and the panes it showed.
    @ObservationIgnored var shownPanes: (width: CGFloat, sidebar: Bool, inspector: Bool)?
    var kindFilters: Set<KindFilter> = [] { didSet { applyFilters() } }
    var scope = Scope.all { didSet { reloadIncidents() } }
    var lockedOnly = false { didSet { applyFilters() } }
    /// Selected incidents, by key. One selected incident plays.
    var selection: Set<String> = [] {
        didSet {
            guard selection != oldValue else { return }
            player.failedFor = nil
            syncPlayer()
        }
    }
    /// The part of the day the timeline lanes show (zoom 1: the whole day
    /// in the lanes' width).
    var timeline = TimelineViewport() { didSet { if timeline.zoom != oldValue.zoom { reloadLoudness() } } }
    var showingGroups = false
    /// Each lane's loudness for the shown day, at `loudnessBucket`.
    private(set) var loudness: [RecordingCamera: LoudnessLane] = [:]
    /// The lanes' width in points (the view reports it), for the loudness
    /// bucket.
    var lanesWidth: Double = 600 { didSet { if abs(lanesWidth - oldValue) >= 1 { reloadLoudness() } } }
    /// About one loudness value per two points at the zoom shown.
    var loudnessBucket: Int64 {
        LoudnessScale.bucket(visibleSeconds: Double(DeviceTime.day) * timeline.visible, width: lanesWidth)
    }
    @ObservationIgnored private var loudnessAsked: (day: Int64, bucket: Int64, cameras: [RecordingCamera], revision: Int)?
    /// Re-reads today's loudness now and then: live seconds arrive with no
    /// event (FFI-RECORDINGS.md section 2a, "a pull API").
    @ObservationIgnored private var loudnessTimer: Timer?
    /// How often the loudness lanes were read (the self-test).
    @ObservationIgnored private(set) var loudnessReads = 0

    /// Days of the calendar's month that have recordings on an enabled camera.
    private(set) var recordedDays: Set<Int64> = []
    private(set) var lanes: [Lane] = []
    /// The day's incidents in scope, newest first, before the kind and lock filters.
    private(set) var dayIncidents: [Incident] = []
    /// What the list shows.
    private(set) var incidents: [Incident] = []
    /// Whether this window holds today.
    private(set) var holding = false
    /// A message from the last request, if it was refused.
    var message: String?
    /// How often the window re-read the index (the self-test).
    @ObservationIgnored private(set) var reloads = 0
    /// Months asked for alone, in order (the self-test).
    @ObservationIgnored private(set) var monthRequests: [Int64] = []

    init(model: RecordingsModel) {
        self.model = model
        holdID = model.newHold()
        player = model.makePlayer()
        let today = model.today
        day = today
        month = DeviceTime.month(containing: today).start
        token = model.subscribe { [weak self] in self?.reload() }
        show(day: today)
        let t = Timer(timeInterval: 10, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.isToday else { return }
                self.reloadLoudness(force: true)
            }
        }
        RunLoop.main.add(t, forMode: .common)
        loudnessTimer = t
    }

    var isToday: Bool { day == model.today }

    /// The cameras this window shows (every enabled camera).
    var cameras: [RecordingCamera] { model.enabledCameras }

    // MARK: Navigation

    /// Shows a day: asks the core for it, holds today only while it is today.
    func show(day newDay: Int64) {
        let d = DeviceTime.dayStart(newDay)
        guard d <= model.today else { return }
        if d != day {
            selection = []
            model.cancelThumbnails()
            player.close()
        }
        day = d
        month = DeviceTime.month(containing: d).start
        asked = nil
        reload()
    }

    /// Pages the calendar; a month other than the shown day's is asked for
    /// alone (its calendar only), once per set of cameras.
    func showMonth(adding months: Int) {
        let next = DeviceTime.month(containing: DeviceTime.month(month, adding: months)).start
        guard next <= model.today else { return }
        month = next
        askMonth()
        reloadCalendar()
    }

    private func askMonth() {
        guard month != DeviceTime.month(containing: day).start else { return }
        let cams = Set(cameras)
        let new = cameras.filter { !(askedMonths[month]?.contains($0) ?? false) }
        guard !new.isEmpty else { return }
        if let refused = model.want(new, months: [month]) {
            message = refused
            return
        }
        askedMonths[month, default: []].formUnion(cams)
        monthRequests.append(month)
    }

    func previousDay() { show(day: day - DeviceTime.day) }
    func nextDay() { show(day: day + DeviceTime.day) }

    /// Searches the shown day again on the device (pull to refresh).
    func refresh() {
        message = model.want(cameras, days: [day], force: true)
        asked = (day, Set(cameras))
    }

    /// The window closes: its playback closes, its hold ends, its thumbnail
    /// requests are dropped.
    func close() {
        loudnessTimer?.invalidate()
        loudnessTimer = nil
        player.close()
        if holding { model.hold(holdID, cameras: nil) }
        holding = false
        heldCameras = []
        if let token { model.unsubscribe(token) }
        token = nil
        model.cancelThumbnails()
    }

    // MARK: Reading the index

    /// Re-reads everything the window shows (after an event, a navigation,
    /// or an enablement change). Asks for cameras newly enabled.
    func reload() {
        reloads += 1
        let cams = Set(cameras)
        if asked?.day != day || !(cams.isSubset(of: asked?.cameras ?? [])) {
            let new = asked?.day == day ? cameras.filter { !(asked?.cameras.contains($0) ?? false) } : cameras
            if !new.isEmpty { message = model.want(new, days: [day]) }
            asked = (day, cams)
        }
        askMonth()
        updateHold()
        reloadCalendar()
        reloadLanes()
        reloadLoudness()
        reloadIncidents()
        syncPlayer()
    }

    /// Reads the shown day's loudness lanes when the day, the cameras, the
    /// bucket, or the index changed (`force`: also when nothing did).
    func reloadLoudness(force: Bool = false) {
        let bucket = loudnessBucket, cams = cameras, revision = model.indexRevision
        if !force, let a = loudnessAsked, a.day == day, a.bucket == bucket, a.cameras == cams, a.revision == revision {
            return
        }
        loudnessAsked = (day, bucket, cams, revision)
        loudnessReads += 1
        let answer = model.loudness(
            start: day, end: day + DeviceTime.day - 1, bucket: bucket, cameras: LoudnessScale.microphones(cams))
        var out: [RecordingCamera: LoudnessLane] = [:]
        for lane in answer?.lanes ?? [] { out[lane.camera] = lane }
        if out != loudness { loudness = out }
    }

    /// The loudness a lane shows (its camera's microphone).
    func loudness(of camera: RecordingCamera) -> LoudnessLane? { loudness[LoudnessScale.microphone(of: camera)] }

    /// Holds today (for the enabled cameras) while today is shown; the core
    /// is told only when that changes.
    private func updateHold() {
        let cams = isToday ? cameras : []
        if !cams.isEmpty {
            guard heldCameras != cams else { return }
            model.hold(holdID, cameras: cams)
            heldCameras = cams
            holding = true
        } else if holding {
            model.hold(holdID, cameras: nil)
            heldCameras = []
            holding = false
        }
    }

    private func reloadCalendar() {
        let m = DeviceTime.month(containing: month)
        recordedDays = model.calendar(start: m.start, end: m.next - 1, cameras: cameras)?.allDays ?? []
    }

    private func reloadLanes() {
        let end = day + DeviceTime.day - 1
        let spans = model.spans(start: day, end: end, cameras: cameras)?.spans ?? []
        let events = model.events(start: day, end: end, cameras: cameras)?.events ?? []
        lanes = cameras.map { c in
            Lane(
                camera: c, name: cameraName(c), spans: spans.first { $0.camera == c }?.spans ?? [],
                events: events.filter { $0.camera == c },
                failed: model.failedDays[c]?.contains(day) == true)
        }
    }

    private func reloadIncidents() {
        let end = day + DeviceTime.day - 1
        var cams: [RecordingCamera]? = nil
        var group: String? = nil
        switch scope {
        case .all: cams = cameras.isEmpty ? nil : cameras
        case .group(let g): group = g
        case .camera(let c): cams = [c]
        }
        dayIncidents = model.incidents(start: day, end: end, cameras: cams, group: group)?.incidents ?? []
        applyFilters()
    }

    private func applyFilters() {
        incidents = dayIncidents.filter { i in
            KindFilter.passes(i, kindFilters) && (!lockedOnly || i.locked)
        }
    }

    /// The selected keys that name an incident of the day.
    var selectedKeys: [String] { dayIncidents.map(\.key).filter { selection.contains($0) } }

    // MARK: Names

    /// A camera's name as the camera list shows it: the user's override
    /// (kept with the camera groups), else the device's channel name with
    /// the lens for a multi-lens camera, else "Channel N".
    func cameraName(_ c: RecordingCamera) -> String {
        if let override = model.groups.cameraNames.first(where: { $0.camera == c }) { return override.name }
        return model.app?.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens))
            ?? "Channel \(c.channel + 1)"
    }

    var dayTitle: String { (isToday ? "Today, " : "") + DeviceTime.longDay(day) }

    // MARK: Selection and the player

    /// The one selected incident, if exactly one is selected.
    var selected: Incident? {
        let keys = selectedKeys
        guard keys.count == 1, let key = keys.first else { return nil }
        return dayIncidents.first { $0.key == key }
    }

    /// Plays the selected incident from its start when it is not the one
    /// the player plays; otherwise only follows its cameras (an edit added
    /// or removed one) without moving the playhead.
    private func syncPlayer() {
        guard let incident = selected, incident.key != player.failedFor else { return }
        if player.incidentKey != incident.key || player.playback == nil {
            player.show(incident, day: day)
        } else {
            player.follow(incident, day: day)
        }
    }

    /// A click on an incident row: selects it alone, or with ⌘ adds or
    /// removes it (several selected incidents can be merged).
    func click(_ key: String, extend: Bool) {
        if extend {
            if selection.contains(key) { selection.remove(key) } else { selection.insert(key) }
        } else {
            selection = [key]
        }
    }

    /// Selects the incident that holds an event.
    func select(event e: RecordedEvent) {
        if let i = dayIncidents.first(where: { i in
            i.involves(e.camera) && i.events.contains { $0.camera == e.camera && $0.start <= e.end && e.start <= $0.end }
        }) {
            selection = [i.key]
        }
    }

    /// A click on a lane at `seconds`: the player seeks there when it plays
    /// that camera; a click on an event of another camera selects (and so
    /// plays) its incident; anywhere else that camera plays alone from there.
    func laneClick(_ camera: RecordingCamera, at seconds: Int64, near event: RecordedEvent? = nil) {
        let ms = seconds * 1000
        if player.cameras.contains(camera), player.day == day, player.playback != nil {
            player.seek(to: ms)
            player.play()
        } else if let event, dayIncidents.contains(where: { $0.involves(event.camera) }) {
            select(event: event)
        } else {
            selection = []
            player.show(cameras: [camera], day: day, at: ms)
        }
    }

    /// The incident the player is positioned for, if it is still of the day.
    var playingIncident: Incident? {
        guard let key = player.incidentKey, player.playback != nil else { return nil }
        return dayIncidents.first { $0.key == key }
    }

    /// The second "Split Here" would split `incident` at: the playhead's,
    /// when the player plays that incident and the playhead is inside it.
    func splitSecond(_ incident: Incident) -> Int64? {
        guard player.incidentKey == incident.key, player.playback != nil else { return nil }
        return IncidentSplit.second(start: incident.start, end: incident.end, playheadMs: player.positionMs)
    }

    func splitHere(_ incident: Incident) {
        guard let at = splitSecond(incident) else { return }
        split(incident.key, at: at)
    }

    func merge() {
        let keys = selectedKeys
        guard keys.count >= 2, let merged = model.edit(.merge(keys)) else { return }
        selection = Set(merged.prefix(1))
    }

    func split(_ key: String, at: Int64) {
        guard let keys = model.edit(.split(key: key, at: at)) else { return }
        // The part that holds the playhead stays selected (and keeps playing).
        let parts = keys.compactMap { k in model.incident(k) }
        let keep = parts.first { $0.start <= at && at <= $0.end && player.positionMs >= $0.start * 1000 }
        if let keep {
            player.renamed(to: keep.key)
            selection = [keep.key]
        } else {
            selection = Set(keys.prefix(1))
        }
    }

    func rename(_ key: String, _ name: String) { model.edit(.rename(key: key, name: name)) }
    func revert(_ key: String) { model.edit(.revert(key: key)) }
    func add(_ camera: RecordingCamera, to key: String) { model.edit(.addCamera(key: key, camera: camera)) }

    /// Remove from Incident: the camera leaves the incident (and the player).
    func remove(_ camera: RecordingCamera, from key: String) {
        guard model.edit(.removeCamera(key: key, camera: camera)) != nil else { return }
        if player.incidentKey == key { player.remove(camera) }
    }

    func setLocked(_ key: String, _ locked: Bool) { model.lock(key, locked) }
    func download(_ key: String) { model.download(incident: key) }

    /// Cameras that could be added to an incident.
    func addable(to i: Incident) -> [RecordingCamera] { cameras.filter { !i.involves($0) } }

    /// The scope choices: all, each group, each camera.
    var scopes: [(Scope, String)] {
        [(Scope.all, "All Cameras")] + model.groups.groups.map { (Scope.group($0.name), $0.name) }
            + cameras.map { (Scope.camera($0), cameraName($0)) }
    }

    /// The scrubber's range in milliseconds: the incident with its margins,
    /// or the player's day.
    var scrubRange: (start: Int64, end: Int64)? {
        guard player.playback != nil else { return nil }
        if scrubScope == .incident, let i = playingIncident {
            return ((i.start - i.margin) * 1000, (i.end + i.margin + 1) * 1000)
        }
        return (player.startMs, player.endMs)
    }
}
