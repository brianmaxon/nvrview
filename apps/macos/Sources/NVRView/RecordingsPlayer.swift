import AppKit
import Foundation
import NVRCore
import Observation

/// The Recordings window's player (docs/UX.md "Recordings"): one playback
/// (FFI-RECORDINGS.md section 6) of up to four cameras of the shown day,
/// on the core's shared clock, drawn by the app's tile pipeline (each
/// camera is a `CoreTile` whose pictures the window's Metal grid draws, as
/// live tiles are). Showing an incident opens a playback of its first four
/// cameras over its day, positioned at the incident's start less the
/// margin, and plays it; another incident of the same cameras and day only
/// seeks. Live view keeps running (review mode, PLAYBACK.md).
///
/// The playback is closed on every exit path: another day or another set of
/// cameras, the window closing (`close`), the app quitting
/// (`RecordingsModel.shutdown`), and the core closing it
/// (`playback_closed`).
@MainActor
@Observable
final class RecordingsPlayer {
    /// One camera of the playback.
    struct Cell: Identifiable, Equatable {
        var camera: RecordingCamera
        let tile: CoreTile
        /// The latest `playback_camera` state: `buffering`, `playing`,
        /// `late`, `no_recording`, `ended`, `failed`.
        var state: String = ""
        var reason: String?
        /// With `start_rejected`: the status the device refused the play with.
        var status: Int?
        var id: ObjectIdentifier { ObjectIdentifier(tile) }

        static func == (a: Cell, b: Cell) -> Bool {
            a.camera == b.camera && a.tile === b.tile && a.state == b.state && a.reason == b.reason
                && a.status == b.status
        }
    }

    @ObservationIgnored private let model: RecordingsModel
    @ObservationIgnored private var timer: Timer?

    /// The open playback, if any.
    private(set) var playback: UInt64?
    private(set) var cells: [Cell] = []
    /// The playback's range and where it is (device-local milliseconds).
    private(set) var startMs: Int64 = 0
    private(set) var endMs: Int64 = 0
    private(set) var positionMs: Int64 = 0
    private(set) var state: PlaybackState?
    private(set) var rate: Double = 1
    /// The day the playback covers.
    private(set) var day: Int64?
    /// The incident the playback was last positioned for.
    private(set) var incidentKey: String?
    /// How the cameras share the player.
    var mode: PlayerGridMode = .grid { didSet { if mode != oldValue { updateAudio() } } }
    var muted = false { didSet { if muted != oldValue { updateAudio() } } }
    /// 0...1.
    var volume: Float = 1 { didSet { if volume != oldValue { updateAudio() } } }
    /// Why the player shows nothing (a refused open), if it does not.
    private(set) var message: String?
    /// Each camera's digital zoom, Fill choice and fisheye view (docs/UX.md
    /// "Recordings"). It belongs to the player's tile: play, pause, seeks,
    /// speed changes, another incident of the same cameras and the grid,
    /// large and single views keep it; a camera that leaves the player
    /// forgets it. Nothing downloaded, exported or edited reads it.
    var tileViews = PlayerTileViews()
    /// The view that draws the cells (it takes the pointer's zoom and pan).
    @ObservationIgnored weak var stage: PlayerStageView?
    /// The cell under the pointer, if any (the keys act on it).
    var hovered: Int?
    /// A short message over the player (a saved picture), with what it offers.
    var toast: PlayerToast?
    /// Pictures saved and the last one (self-test).
    @ObservationIgnored var pictureSaves = 0
    @ObservationIgnored var lastPictureSave: PictureSaveRecord?
    /// Counts saves, so a cell can flash (the shutter).
    var flashCount = 0
    var flashCamera: RecordingCamera?
    /// How many playbacks this player opened and seeks it sent (self-test).
    @ObservationIgnored private(set) var opens = 0
    @ObservationIgnored private(set) var seeks = 0

    init(model: RecordingsModel) {
        self.model = model
    }

    var app: AppModel? { model.app }

    var transport: TransportState {
        TransportState(startMs: startMs, endMs: endMs, positionMs: positionMs, state: state, rate: rate)
    }

    /// The cameras in the player, in order.
    var cameras: [RecordingCamera] { cells.map(\.camera) }

    // MARK: Showing

    /// Plays an incident: its first four cameras over `day`, from its start
    /// less its margin. With the same cameras and day already open, only
    /// seeks.
    func show(_ incident: Incident, day: Int64) {
        let cams = PlayerGrid.cameras(incident.cameras.map(\.camera))
        let at = (incident.start - incident.margin) * 1000
        show(cameras: cams, day: day, at: at)
        incidentKey = incident.key
        // Not retried by itself (every index event would ask again); a new
        // selection does.
        failedFor = playback == nil ? incident.key : nil
    }

    /// The incident whose playback could not be opened.
    var failedFor: String?

    /// The selected incident changed shape (a camera added or removed by an
    /// edit): the player follows its cameras without moving the playhead.
    func follow(_ incident: Incident, day: Int64) {
        let want = PlayerGrid.cameras(incident.cameras.map(\.camera))
        guard playback != nil, self.day == day else { return show(incident, day: day) }
        for c in cameras where !want.contains(c) { remove(c) }
        for c in want where !cameras.contains(c) { add(c) }
        incidentKey = incident.key
    }

    /// The incident the player plays got a new key (a split kept the part
    /// at the playhead).
    func renamed(to key: String) { incidentKey = key }

    /// Plays these cameras (at most four) over `day` from `at`.
    func show(cameras: [RecordingCamera], day: Int64, at: Int64) {
        incidentKey = nil
        let cams = PlayerGrid.cameras(cameras)
        guard !cams.isEmpty else { return close() }
        if playback != nil, self.day == day, self.cameras == cams {
            seek(to: at, exact: true)
            play()
            return
        }
        close()
        open(cameras: cams, day: day, at: at)
    }

    private func open(cameras: [RecordingCamera], day: Int64, at: Int64) {
        let start = day * 1000, end = (day + DeviceTime.day) * 1000
        let position = min(max(at, start), end)
        let request = PlaybackRequest.range(
            startMs: start, endMs: end, positionMs: position, rate: rate,
            cameras: cameras.map { PlaybackCamera(camera: $0) })
        do throws(CoreStatus) {
            let opened = try model.openPlayback(json: request, cameras: cameras, player: self)
            opens += 1
            playback = opened.id
            cells = zip(cameras, opened.tiles).map { Cell(camera: $0, tile: $1) }
            tileViews.retain(cameras)
            self.day = day
            startMs = start
            endMs = end
            positionMs = position
            state = .paused
            message = nil
            mode = PlayerGrid.normalized(mode, count: cells.count)
            updateAudio()
            startTimer()
            play()
        } catch {
            message = PlaybackOpenMessage.text(error)
        }
    }

    /// Closes the playback (every tile closes with it).
    func close() {
        timer?.invalidate()
        timer = nil
        if let p = playback { model.closePlayback(p, tiles: cells.map(\.tile)) }
        playback = nil
        cells = []
        state = nil
        day = nil
    }

    /// The playback ended on the core's side (`playback_closed`).
    fileprivate func closedByCore() {
        timer?.invalidate()
        timer = nil
        model.forgetPlaybackTiles(cells.map(\.tile))
        playback = nil
        cells = []
        state = nil
        day = nil
        message = "The playback was closed"
    }

    // MARK: Transport

    func play() {
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackPlay(p) }
    }

    func pause() {
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackPause(p) }
    }

    func togglePlay() { transport.isPlaying ? pause() : play() }

    /// Seeks: `exact` shows the picture at the time (a click or a release);
    /// otherwise the keyframe before it (dragging).
    func seek(to ms: Int64, exact: Bool = true) {
        guard let p = playback else { return }
        let target = transport.clamp(ms)
        positionMs = target
        seeks += 1
        model.playbackCall { core throws(CoreStatus) in try core.playbackSeek(p, positionMs: target, mode: exact ? .exact : .keyframe) }
    }

    func skip(by delta: Int64) { seek(to: transport.skipped(by: delta)) }

    func setRate(_ new: Double) {
        guard PlaybackSpeed.all.contains(new), new != rate else { return }
        rate = new
        updateAudio()
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackSetRate(p, rate: new) }
    }

    /// Takes a camera out of the player (Remove from Incident does it too).
    func remove(_ camera: RecordingCamera) {
        guard let p = playback, let i = cells.firstIndex(where: { $0.camera == camera }) else { return }
        let tile = cells[i].tile
        cells.remove(at: i)
        tileViews.retain(cameras)
        mode = PlayerGrid.normalized(mode, count: cells.count)
        model.removePlaybackCamera(p, tile: tile)
        if cells.isEmpty { close() } else { updateAudio() }
    }

    /// Adds a camera to the player (at most four), joining at the position.
    func add(_ camera: RecordingCamera) {
        guard let p = playback, cells.count < PlayerGrid.maxCameras, !cameras.contains(camera) else { return }
        if let tile = model.addPlaybackCamera(p, camera: camera) {
            cells.append(Cell(camera: camera, tile: tile))
            updateAudio()
        }
    }

    func click(_ i: Int) { mode = PlayerGrid.click(mode, on: i, count: cells.count) }
    func doubleClick(_ i: Int) { mode = PlayerGrid.doubleClick(mode, on: i, count: cells.count) }

    // MARK: Sound

    /// The playback's sound goes through the core's tile audio path, like a
    /// live tile's: the large (or first) camera is audible unless muted; the
    /// core plays recorded sound at 1× only.
    private func updateAudio() {
        let audible = muted ? nil : PlayerGrid.audibleIndex(mode, count: cells.count)
        for (i, c) in cells.enumerated() {
            model.setPlaybackAudio(c.tile, audible: i == audible && PlaybackSpeed.hasSound(rate), volume: volume)
        }
    }

    /// The camera whose sound plays, if any.
    var audibleCamera: RecordingCamera? {
        guard !muted, PlaybackSpeed.hasSound(rate), let i = PlayerGrid.audibleIndex(mode, count: cells.count) else {
            return nil
        }
        return cells[i].camera
    }

    // MARK: The clock

    private func startTimer() {
        timer?.invalidate()
        // The scrubber's reading, ten times a second (nv_playback_position
        // is lock-free and answers from memory).
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    /// Reads the position and state now.
    func tick() {
        guard let p = playback, let now = model.playbackPosition(p) else { return }
        if now.positionMs != positionMs { positionMs = now.positionMs }
        if now.state != state { state = now.state }
    }

    // MARK: Events

    func apply(_ e: PlaybackEvent) {
        switch e {
        case .state(_, let s, let position, let r):
            state = s
            positionMs = position
            if r != rate, PlaybackSpeed.all.contains(r) { rate = r }
        case .camera(_, let tile, let s, let reason, let status):
            if let i = cells.firstIndex(where: { $0.tile.id == tile }) {
                cells[i].state = s
                cells[i].reason = reason
                cells[i].status = status
            }
        case .seek:
            break
        case .closed:
            closedByCore()
        }
    }
}
