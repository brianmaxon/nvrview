import Foundation
import NVRCore
import Observation

/// What one recordings window shows (docs/UX.md "Recordings"): a day of the
/// enabled cameras, its incidents with the filters, the timeline lanes, and
/// the month's calendar. Asks the core for the day it shows
/// (`nv_recordings_want`) and holds today while today is shown, under its
/// own hold number; releases the hold when it closes or moves to another
/// day. Every query answers from the core's memory; device work arrives as
/// events, after which the model calls `reload`.
@MainActor
@Observable
final class RecordingsBrowser {
    enum Tab: String, CaseIterable, Identifiable {
        case incidents = "Incidents"
        case timeline = "Timeline"
        case downloads = "Downloads"
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
    @ObservationIgnored private var heldCameras: [RecordingCamera] = []

    /// The shown day (device-local, its first second).
    private(set) var day: Int64
    /// The calendar's month (any second in it).
    private(set) var month: Int64
    var tab = Tab.incidents
    var kindFilters: Set<KindFilter> = [] { didSet { applyFilters() } }
    var scope = Scope.all { didSet { reloadIncidents() } }
    var lockedOnly = false { didSet { applyFilters() } }
    /// Selected incidents, by key.
    var selection: Set<String> = []
    /// Timeline zoom: 1 shows the whole day in the window's width.
    var zoom: Double = 1
    var showingGroups = false

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

    init(model: RecordingsModel) {
        self.model = model
        holdID = model.newHold()
        let today = model.today
        day = today
        month = today
        token = model.subscribe { [weak self] in self?.reload() }
        show(day: today)
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
        }
        day = d
        month = d
        asked = nil
        reload()
    }

    func showMonth(adding months: Int) {
        month = DeviceTime.month(month, adding: months)
        reloadCalendar()
    }

    func previousDay() { show(day: day - DeviceTime.day) }
    func nextDay() { show(day: day + DeviceTime.day) }

    /// Searches the shown day again on the device (pull to refresh).
    func refresh() {
        message = model.want(cameras, days: [day], force: true)
        asked = (day, Set(cameras))
    }

    /// The window closes: its hold ends, its thumbnail requests are dropped.
    func close() {
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
        updateHold()
        reloadCalendar()
        reloadLanes()
        reloadIncidents()
    }

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
            KindFilter.passes(i.kinds, kindFilters) && (!lockedOnly || i.locked)
        }
        // The selection is left alone here: it is the list's binding, and
        // changing it while the list updates its rows makes AppKit's table
        // re-enter its delegate. Keys that went away select nothing.
    }

    /// The selected keys that name an incident of the day.
    var selectedKeys: [String] { dayIncidents.map(\.key).filter { selection.contains($0) } }

    // MARK: Names

    func cameraName(_ c: RecordingCamera) -> String {
        if let override = model.groups.cameraNames.first(where: { $0.camera == c }) { return override.name }
        return model.app?.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens))
            ?? "Channel \(c.channel + 1)"
    }

    var dayTitle: String { (isToday ? "Today, " : "") + DeviceTime.longDay(day) }

    // MARK: Selection and edits

    /// The one selected incident, if exactly one is selected.
    var selected: Incident? {
        let keys = selectedKeys
        guard keys.count == 1, let key = keys.first else { return nil }
        return dayIncidents.first { $0.key == key }
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

    /// Selects the incident that holds an event (a click on the timeline).
    func select(event e: RecordedEvent) {
        if let i = dayIncidents.first(where: { i in
            i.involves(e.camera) && i.events.contains { $0.camera == e.camera && $0.start <= e.end && e.start <= $0.end }
        }) {
            selection = [i.key]
        }
    }

    func merge() {
        let keys = selectedKeys
        guard keys.count >= 2, let merged = model.edit(.merge(keys)) else { return }
        selection = Set(merged.prefix(1))
    }

    func split(_ key: String, at: Int64) {
        guard let keys = model.edit(.split(key: key, at: at)) else { return }
        selection = Set(keys.prefix(1))
    }

    func rename(_ key: String, _ name: String) { model.edit(.rename(key: key, name: name)) }
    func revert(_ key: String) { model.edit(.revert(key: key)) }
    func add(_ camera: RecordingCamera, to key: String) { model.edit(.addCamera(key: key, camera: camera)) }
    func remove(_ camera: RecordingCamera, from key: String) { model.edit(.removeCamera(key: key, camera: camera)) }
    func setLocked(_ key: String, _ locked: Bool) { model.lock(key, locked) }
    func download(_ key: String) { model.download(incident: key) }

    /// Cameras that could be added to an incident.
    func addable(to i: Incident) -> [RecordingCamera] { cameras.filter { !i.involves($0) } }

    /// The scope choices: all, each group, each camera.
    var scopes: [(Scope, String)] {
        [(Scope.all, "All Cameras")] + model.groups.groups.map { (Scope.group($0.name), $0.name) }
            + cameras.map { (Scope.camera($0), cameraName($0)) }
    }
}
