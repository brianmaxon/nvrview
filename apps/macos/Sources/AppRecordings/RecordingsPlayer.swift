import AppKit
import Foundation
import NVRCore
import Observation
import AppBase
import AppCapture

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
package final class RecordingsPlayer {
    /// One camera of the playback.
    package struct Cell: Identifiable, Equatable {
        package var camera: RecordingCamera
        package let tile: CoreTile
        /// The latest `playback_camera` state: `buffering`, `playing`,
        /// `late`, `no_recording`, `ended`, `failed`.
        package var state: String = ""
        package var reason: String?
        /// With `start_rejected`: the status the device refused the play with.
        package var status: Int?
        package var id: ObjectIdentifier { ObjectIdentifier(tile) }

        package static func == (a: Cell, b: Cell) -> Bool {
            a.camera == b.camera && a.tile === b.tile && a.state == b.state && a.reason == b.reason
                && a.status == b.status
        }

        package init(camera: RecordingCamera, tile: CoreTile, state: String = "", reason: String? = nil, status: Int? = nil) {
            self.camera = camera
            self.tile = tile
            self.state = state
            self.reason = reason
            self.status = status
        }
    }

    @ObservationIgnored private let model: RecordingsModel
    @ObservationIgnored private var timer: Timer?

    /// The open playback, if any.
    package private(set) var playback: UInt64?
    package private(set) var cells: [Cell] = []
    /// The playback's range and where it is (device-local milliseconds).
    package private(set) var startMs: Int64 = 0
    package private(set) var endMs: Int64 = 0
    package private(set) var positionMs: Int64 = 0
    package private(set) var state: PlaybackState?
    package private(set) var rate: Double = 1
    /// The day the playback covers.
    package private(set) var day: Int64?
    /// The incident the playback was last positioned for.
    package private(set) var incidentKey: String?
    /// How the cameras share the player.
    package var mode: PlayerGridMode = .grid {
        didSet { if mode != oldValue { updateAudio(); refreshStreams() } }
    }
    package var muted = false { didSet { if muted != oldValue { updateAudio() } } }
    /// 0...1.
    package var volume: Float = 1 { didSet { if volume != oldValue { updateAudio() } } }
    /// Why the player shows nothing (a refused open), if it does not.
    package private(set) var message: String?
    /// Each camera's digital zoom, Fill choice and fisheye view (docs/UX.md
    /// "Recordings"). It belongs to the player's tile: play, pause, seeks,
    /// speed changes, another incident of the same cameras and the grid,
    /// large and single views keep it; a camera that leaves the player
    /// forgets it. Nothing downloaded, exported or edited reads it.
    package var tileViews = PlayerTileViews() { didSet { if tileViews != oldValue { refreshStreams() } } }
    /// Each camera's stream choice (Auto, High, Low) and what plays now
    /// (docs/UX.md "Recordings", "Quality"). A change restarts only that
    /// camera's play at the position; the zoom above is kept.
    package private(set) var streams = PlayerStreams()
    /// The view that draws the cells (it takes the pointer's zoom and pan).
    @ObservationIgnored package weak var stage: PlayerStageView?
    /// The cell under the pointer, if any (the keys act on it).
    package var hovered: Int?
    /// A short message over the player (a saved picture), with what it offers.
    package var toast: PlayerToast?
    /// Pictures saved and the last one (self-test).
    @ObservationIgnored package var pictureSaves = 0
    @ObservationIgnored package var lastPictureSave: PictureSaveRecord?
    /// Counts saves, so a cell can flash (the shutter).
    package var flashCount = 0
    package var flashCamera: RecordingCamera?
    /// How many playbacks this player opened and seeks it sent (self-test).
    @ObservationIgnored package private(set) var opens = 0
    @ObservationIgnored package private(set) var seeks = 0
    /// How many stream changes this player asked the core for (self-test).
    @ObservationIgnored package private(set) var streamChanges = 0

    package init(model: RecordingsModel) {
        self.model = model
    }

    package var app: (any CaptureHost)? { model.app }

    package var transport: TransportState {
        TransportState(startMs: startMs, endMs: endMs, positionMs: positionMs, state: state, rate: rate)
    }

    /// The cameras in the player, in order.
    package var cameras: [RecordingCamera] { cells.map(\.camera) }

    // MARK: Showing

    /// Plays an incident: its first four cameras over `day`, from its start
    /// less its margin. With the same cameras and day already open, only
    /// seeks.
    package func show(_ incident: Incident, day: Int64) {
        let cams = PlayerGrid.cameras(incident.cameras.map(\.camera))
        let at = (incident.start - incident.margin) * 1000
        show(cameras: cams, day: day, at: at)
        incidentKey = incident.key
        // Not retried by itself (every index event would ask again); a new
        // selection does.
        failedFor = playback == nil ? incident.key : nil
    }

    /// The incident whose playback could not be opened.
    package var failedFor: String?

    /// The selected incident changed shape (a camera added or removed by an
    /// edit): the player follows its cameras without moving the playhead.
    package func follow(_ incident: Incident, day: Int64) {
        let want = PlayerGrid.cameras(incident.cameras.map(\.camera))
        guard playback != nil, self.day == day else { return show(incident, day: day) }
        for c in cameras where !want.contains(c) { remove(c) }
        for c in want where !cameras.contains(c) { add(c) }
        incidentKey = incident.key
    }

    /// The incident the player plays got a new key (a split kept the part
    /// at the playhead).
    package func renamed(to key: String) { incidentKey = key }

    /// Plays these cameras (at most four) over `day` from `at`.
    package func show(cameras: [RecordingCamera], day: Int64, at: Int64) {
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
        streams.retain(cameras)
        let views = tileViews
        let tiers = streams.opening(
            cameras, mode: PlayerGrid.normalized(mode, count: cameras.count), rate: rate,
            zoom: { views.zoom($0).scale })
        let request = PlaybackRequest.range(
            startMs: start, endMs: end, positionMs: position, rate: rate,
            cameras: zip(cameras, tiers).map { PlaybackCamera(camera: $0, tier: $1 == .main ? "main" : "sub") })
        do throws(CoreStatus) {
            let opened = try model.openPlayback(json: request, cameras: cameras, tiers: tiers, player: self)
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
    package func close() {
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

    package func play() {
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackPlay(p) }
    }

    package func pause() {
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackPause(p) }
    }

    package func togglePlay() { transport.isPlaying ? pause() : play() }

    /// Seeks: `exact` shows the picture at the time (a click or a release);
    /// otherwise the keyframe before it (dragging).
    package func seek(to ms: Int64, exact: Bool = true) {
        guard let p = playback else { return }
        let target = transport.clamp(ms)
        positionMs = target
        seeks += 1
        model.playbackCall { core throws(CoreStatus) in try core.playbackSeek(p, positionMs: target, mode: exact ? .exact : .keyframe) }
    }

    package func skip(by delta: Int64) { seek(to: transport.skipped(by: delta)) }

    package func setRate(_ new: Double) {
        guard PlaybackSpeed.all.contains(new), new != rate else { return }
        rate = new
        updateAudio()
        refreshStreams()
        guard let p = playback else { return }
        model.playbackCall { core throws(CoreStatus) in try core.playbackSetRate(p, rate: new) }
    }

    /// Takes a camera out of the player (Remove from Incident does it too).
    package func remove(_ camera: RecordingCamera) {
        guard let p = playback, let i = cells.firstIndex(where: { $0.camera == camera }) else { return }
        let tile = cells[i].tile
        cells.remove(at: i)
        tileViews.retain(cameras)
        streams.retain(cameras)
        mode = PlayerGrid.normalized(mode, count: cells.count)
        model.removePlaybackCamera(p, tile: tile)
        if cells.isEmpty { close() } else {
            updateAudio()
            // The camera left may be the only one now: Auto asks for its main stream.
            refreshStreams()
        }
    }

    /// Adds a camera to the player (at most four), joining at the position.
    package func add(_ camera: RecordingCamera) {
        guard let p = playback, cells.count < PlayerGrid.maxCameras, !cameras.contains(camera) else { return }
        var joining = streams
        let tier = joining.wanted(
            camera, cameras: cameras + [camera], mode: PlayerGrid.normalized(mode, count: cells.count + 1), rate: rate,
            zoomScale: tileViews.zoom(camera).scale)
        if let tile = model.addPlaybackCamera(p, camera: camera, tier: tier) {
            joining.joined(camera, tier: tier)
            streams = joining
            cells.append(Cell(camera: camera, tile: tile))
            updateAudio()
            // The cameras already there are no longer alone: Auto goes back to their sub-stream.
            refreshStreams()
        }
    }

    // MARK: Quality

    /// The camera's choice: Auto, High (main stream) or Low (sub-stream).
    package func setQuality(_ quality: PlayerQuality, for camera: RecordingCamera) {
        guard streams.quality(camera) != quality else { return }
        streams.setQuality(quality, for: camera)
        refreshStreams()
    }

    /// Asks the core for the stream of every camera whose stream is not the
    /// one wanted now (the layout, the speed, the zoom and the choice
    /// decide: `PlayerStreams`). The core restarts that camera's play at the
    /// position, in the group-start rules; the picture and zoom stay.
    package func refreshStreams() {
        guard playback != nil else { return }
        let views = tileViews
        let wanted = streams.changes(
            cameras, mode: PlayerGrid.normalized(mode, count: cells.count), rate: rate,
            zoom: { views.zoom($0).scale })
        streamChanges += wanted.count
        for change in wanted {
            guard let cell = cells.first(where: { $0.camera == change.camera }) else { continue }
            model.setPlaybackTier(cell.tile, change.tier)
        }
    }

    package func click(_ i: Int) { mode = PlayerGrid.click(mode, on: i, count: cells.count) }
    package func doubleClick(_ i: Int) { mode = PlayerGrid.doubleClick(mode, on: i, count: cells.count) }

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
    package var audibleCamera: RecordingCamera? {
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
    package func tick() {
        guard let p = playback, let now = model.playbackPosition(p) else { return }
        if now.positionMs != positionMs { positionMs = now.positionMs }
        if now.state != state { state = now.state }
    }

    // MARK: Events

    package func apply(_ e: PlaybackEvent) {
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
