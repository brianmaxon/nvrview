import AppKit
import Foundation
import NVRCore

/// Live recordings and call recordings (docs/CAPTURE.md "Live recording",
/// "Recording calls"; docs/UX.md "Recording"): installs the AAC encoder,
/// configures the core from the settings (at startup and on every change),
/// starts and stops recordings, and keeps the board of running ones from
/// the core's `recording` events. The core writes the files; this only
/// asks and shows. Main actor only.
@MainActor
final class CaptureController {
    private unowned let model: AppModel
    /// The running recordings and the latest ended ones.
    private(set) var board = RecordingBoard()
    /// The encoder handed to the core (kept by the core until it is freed).
    private(set) var encoder: AACCaptureEncoder?
    /// Whether the core took the last configuration.
    private(set) var configured = false
    /// The settings last sent to the core.
    private(set) var settings: CaptureSettings
    /// The self-test's override of the free space a recording needs.
    var minFreeOverride: UInt64? {
        didSet { configure() }
    }
    /// Each open call's reserved stem (its recording and transcript share
    /// it), by the doorbell's first lens.
    private var callStems: [CameraRef: String] = [:]
    /// Shows a message about a recording in the windows showing `camera`
    /// (else the active one), with an optional button (set by the app).
    var notice: (_ text: String, _ camera: CameraRef?, _ action: (title: String, handler: () -> Void)?) -> Void = {
        _, _, _ in
    }

    init(model: AppModel) {
        self.model = model
        settings = CaptureSettings(liveFolder: AppModel.defaultRecordingsFolder.path)
    }

    // MARK: Setup

    /// Set by `start`: configuring waits for it, so settings read at
    /// launch (or the self-test's) are sent once, not as each is set.
    private var started = false

    /// Installs the encoder and configures the core (at startup).
    func start() {
        started = true
        if encoder == nil, let core = try? model.bridge.capture() {
            let e = AACCaptureEncoder { recording, track, unit in core.recordAudioEncoded(recording: recording, track: track, unit: unit) }
            do {
                try core.captureSetEncoder(e)
                encoder = e
            } catch {
                NSLog("capture: the audio encoder was not installed (%@)", "\(error)")
            }
        }
        configure()
    }

    /// The settings as the model has them now.
    var currentSettings: CaptureSettings {
        var s = CaptureSettings(liveFolder: model.recordingsFolder.path)
        s.quality = model.recordFrom
        s.shareSafe = model.shareSafeNames
        s.maxMinutes = model.maxRecordingMinutes
        s.maxBytes = UInt64(max(model.maxRecordingGB, 0) * 1e9)
        if let minFreeOverride { s.minFreeBytes = minFreeOverride }
        return s.clamped
    }

    /// Sends the settings to the core (for recordings started afterwards).
    func configure() {
        guard started else { return }
        let s = currentSettings
        do {
            try model.bridge.capture().captureConfigure(json: s.json)
            settings = s
            configured = true
        } catch {
            configured = false
            NSLog("capture: configure failed (%@)", "\(error)")
        }
    }

    // MARK: Live recordings

    /// Starts recording `camera` (its tile's stream at `quality`, else the
    /// "Record from" setting). Shows why when it cannot.
    func startLive(_ camera: CameraRef, quality: RecordQuality? = nil) {
        guard let tile = model.leases.tile(for: camera) else {
            return refused(RecordStartError(.unknownID), camera: camera, call: false)
        }
        guard let core = try? model.bridge.capture() else {
            return refused(RecordStartError(.closed), camera: camera, call: false)
        }
        do throws(RecordStartError) {
            let started = try core.recordStart(
                .live(
                    tile: tile.id, quality: quality?.rawValue, name: model.name(of: camera),
                    utcOffsetSeconds: RecordRequest.localUTCOffsetSeconds))
            board.begin(
                started.recording, kind: "live", camera: camera, file: started.file,
                tier: (quality ?? settings.quality).rawValue)
            model.changed()
        } catch {
            refused(error, camera: camera, call: false)
        }
    }

    /// Record or Stop on a grid tile: stops every recording of the camera,
    /// or starts a live one.
    func toggleLive(_ camera: CameraRef) {
        let mine = board.recordings(of: camera)
        if mine.isEmpty { startLive(camera) } else { for r in mine { stop(r.id) } }
    }

    func stop(_ id: UInt64) {
        do {
            try model.bridge.capture().recordStop(id)
        } catch {
            NSLog("capture: stop %llu failed (%@)", id, "\(error)")
        }
    }

    /// Stop All Recordings.
    func stopAll() { for r in board.list { stop(r.id) } }

    /// The recordings of `camera` (the tile's pill and its Record button).
    func isRecorded(_ camera: CameraRef) -> Bool { !board.recordings(of: camera).isEmpty }

    // MARK: Call recordings

    /// The stem the call's recording and transcript share, reserved the
    /// first time either needs it (nil without a call).
    func callStem(for camera: CameraRef) -> String? {
        let key = camera.firstLens
        if let s = callStems[key] { return s }
        guard let call = model.calls.call(for: key) else { return nil }
        let name = CallTranscriptFile.fileName(startedAt: call.startedAt, camera: model.name(of: key), timeZone: .current)
        let taken = Set((try? FileManager.default.contentsOfDirectory(atPath: model.transcripts.folder.path)) ?? [])
        let stem = CallTranscriptFile.sharedStem(name, taken: taken)
        callStems[key] = stem
        return stem
    }

    /// The stem reserved for a call, if its recording reserved one.
    func reservedCallStem(_ camera: CameraRef) -> String? { callStems[camera.firstLens] }

    /// The call ended: its stem is free again (after the transcript used it).
    func callEnded(_ camera: CameraRef) { callStems[camera.firstLens] = nil }

    /// Starts recording the open call of `camera` beside its transcript.
    func startCall(_ camera: CameraRef) {
        let key = camera.firstLens
        guard model.calls.call(for: key) != nil, let tile = model.leases.tile(for: key), let stem = callStem(for: key)
        else { return refused(RecordStartError(.unknownID), camera: camera, call: true) }
        let folder = model.transcripts.folder
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let file = folder.appendingPathComponent(stem + ".mp4").path
        guard let core = try? model.bridge.capture() else {
            return refused(RecordStartError(.closed), camera: camera, call: true)
        }
        do throws(RecordStartError) {
            let started = try core.recordStart(.call(tile: tile.id, file: file))
            board.begin(started.recording, kind: "call", camera: key, file: started.file, tier: settings.quality.rawValue)
            model.changed()
        } catch {
            refused(error, camera: camera, call: true)
        }
    }

    /// The call window's Record: starts or stops the call recording.
    func toggleCall(_ camera: CameraRef) {
        if let r = board.call(of: camera.firstLens) { stop(r.id) } else { startCall(camera) }
    }

    /// The running call recording of a doorbell, if any.
    func callRecording(_ camera: CameraRef) -> RecordingBoard.Entry? { board.call(of: camera.firstLens) }

    // MARK: Events

    /// A `recording` event from the core.
    func apply(_ e: RecordingEvent) {
        guard let ended = board.apply(e) else { return }
        if ended.kind == "call" { model.transcripts.pruneRecordings() }
        model.logEvent(
            "Recording \(ended.state) (\(ended.reason ?? "-")): \((ended.file as NSString).lastPathComponent), "
                + "\(RecordingText.elapsed(ms: ended.durationMs)), \(ended.bytes) bytes, audio \(ended.audio)")
        guard let text = RecordingText.ended(ended, settings: settings) else { return }
        let url = URL(fileURLWithPath: ended.file)
        let camera = model.devices.first { $0.deviceID == ended.device }.map {
            CameraRef(device: $0.key, channel: Int(ended.channel), lens: Int(ended.lens))
        }
        let exists = FileManager.default.fileExists(atPath: url.path)
        notice(text, camera, exists ? ("Show in Finder", { NSWorkspace.shared.activateFileViewerSelecting([url]) }) : nil)
    }

    func rekey(_ old: String, to new: String) { board.rekey(old, to: new) }

    // MARK: Messages

    private func refused(_ error: RecordStartError, camera: CameraRef, call: Bool) {
        let (text, offer) = RecordingText.refused(error, settings: settings, call: call)
        lastRefusal = text
        notice(text, camera, offer ? ("Record Sub-stream", { [weak self] in self?.startLive(camera, quality: .sub) }) : nil)
    }

    /// The last refusal's message (self-test).
    private(set) var lastRefusal: String?

    /// "Open Recordings Folder": creates it if needed and shows it.
    func openFolder() {
        try? FileManager.default.createDirectory(at: model.recordingsFolder, withIntermediateDirectories: true)
        NSWorkspace.shared.open(model.recordingsFolder)
    }
}
