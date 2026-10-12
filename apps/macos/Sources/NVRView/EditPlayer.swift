import AVFoundation
import Foundation
import NVRCore
import Observation
import QuartzCore

/// Why no edit list could be made.
struct EditPlanRefusal: Error, Equatable {
    let text: String
}

/// The transport the Recordings window's controls drive: the device playback
/// (`RecordingsPlayer`) or a follow cut or grid of downloaded files
/// (`EditPlayer`).
@MainActor
protocol PlayerTransportDriver: AnyObject {
    var transport: TransportState { get }
    var rate: Double { get }
    var muted: Bool { get set }
    var volume: Float { get set }
    /// Something is open to play.
    var canPlay: Bool { get }
    func togglePlay()
    func skip(by delta: Int64)
    func seek(to ms: Int64, exact: Bool)
    func setRate(_ new: Double)
}

extension RecordingsPlayer: PlayerTransportDriver {
    var canPlay: Bool { playback != nil }
}

/// The Recordings window's follow cut and 2-up or 4-up grid (docs/UX.md
/// "Recordings", "Follow and Grid"): the core's edit list over an incident's
/// downloaded files (`nv_edit_compute`), the files decoded by VideoToolbox
/// and drawn by the Metal grid renderer (`EditSession`), on the edit's own
/// clock driven by the same transport as the device playback (play and
/// pause, skip, speeds, scrubbing: keyframes while dragging and at 2× and
/// up, the exact picture otherwise). The sound is the shown camera's in the
/// follow cut and the chosen cell's (the first) in a grid, at 1× only, with
/// the player's mute and volume.
@MainActor
@Observable
final class EditPlayer: PlayerTransportDriver {
    enum Kind: Equatable, Sendable {
        case follow
        /// `2up` or `4up`.
        case grid(String)

        var request: String {
            switch self {
            case .follow: "follow"
            case .grid: "grid"
            }
        }
    }

    @ObservationIgnored private let model: RecordingsModel
    /// The device playback's player, whose mute and volume this shares.
    @ObservationIgnored private weak var cameras: RecordingsPlayer?
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var opening: Task<Void, Never>?
    @ObservationIgnored private let audio = EditAudio()

    private(set) var kind: Kind?
    private(set) var download: String?
    private(set) var plan: EditPlan?
    @ObservationIgnored private(set) var session: EditSession?
    /// Opening (computing the plan, opening the files).
    private(set) var loading = false
    /// Why nothing is shown, if so.
    private(set) var message: String?
    private(set) var positionMs: Int64 = 0
    private(set) var state: PlaybackState?
    private(set) var rate: Double = 1
    /// The grid cell whose sound plays.
    var gridSound = 0 { didSet { updateAudio() } }
    /// Dragging the scrubber: keyframes only until the release.
    @ObservationIgnored var scrubbing = false

    @ObservationIgnored private(set) var clock = EditClock(startMs: 0, endMs: 0)
    /// The host clock in seconds; the self-test sets a simulated one.
    @ObservationIgnored var now: () -> Double = { CACurrentMediaTime() }
    /// Plans opened (the self-test).
    @ObservationIgnored private(set) var opens = 0

    init(model: RecordingsModel, cameras: RecordingsPlayer) {
        self.model = model
        self.cameras = cameras
    }

    var isActive: Bool { kind != nil }
    var canPlay: Bool { session != nil }

    var muted: Bool {
        get { cameras?.muted ?? false }
        set {
            cameras?.muted = newValue
            updateAudio()
        }
    }

    var volume: Float {
        get { cameras?.volume ?? 1 }
        set {
            cameras?.volume = newValue
            updateAudio()
        }
    }

    var transport: TransportState {
        TransportState(startMs: clock.startMs, endMs: clock.endMs, positionMs: positionMs, state: state, rate: rate)
    }

    /// Where the follow cut changes camera (scrubber marks).
    var cuts: [Int64] { plan.map(EditTimeline.cuts) ?? [] }

    /// The source whose sound plays now.
    var audibleSource: Int? {
        guard let plan else { return nil }
        return EditTimeline.audibleSource(plan, at: positionMs, rate: rate, muted: muted, gridCell: gridSound)
    }

    /// Pictures decode as keyframes only (dragging, 2× and up).
    var keyframesOnly: Bool { scrubbing || PlaybackSpeed.keyframesOnly(rate) }

    /// The position to draw now (the stage asks once per display refresh).
    func framePosition() -> Int64 { clock.position(at: now()) }

    // MARK: Opening

    /// Shows `kind` over a finished download, from `at` (kept within the
    /// plan's range; its start when nil), and plays.
    func open(_ kind: Kind, download: String, at: Int64?) {
        close()
        self.kind = kind
        self.download = download
        loading = true
        message = nil
        let canvas = #""canvas":{"width":1920,"height":1080}"#
        let id = String(decoding: (try? JSONSerialization.data(withJSONObject: [download])) ?? Data(#"[""]"#.utf8), as: UTF8.self)
            .dropFirst().dropLast()
        let json: String
        switch kind {
        case .follow: json = #"{"kind":"follow","download":\#(id)}"#
        case .grid(let layout): json = #"{"kind":"grid","download":\#(id),"layout":"\#(layout)",\#(canvas)}"#
        }
        let plan: EditPlan
        switch model.editPlan(json: json) {
        case .success(let p): plan = p
        case .failure(let refusal):
            loading = false
            message = refusal.text
            return
        }
        self.plan = plan
        opens += 1
        opening = Task { [weak self] in
            let session = await EditSession.open(plan)
            guard let self, !Task.isCancelled, self.plan == plan, self.kind == kind else {
                session.close()
                return
            }
            self.session = session
            self.loading = false
            if session.files.isEmpty { self.message = "The downloaded files could not be opened" }
            self.clock = EditClock(startMs: plan.startMs, endMs: plan.endMs, positionMs: at)
            self.clock.setRate(self.rate, at: self.now())
            self.startTimer()
            self.play()
        }
    }

    /// Closes the edit (its files and sound).
    func close() {
        opening?.cancel()
        opening = nil
        timer?.invalidate()
        timer = nil
        session?.close()
        session = nil
        audio.close()
        plan = nil
        kind = nil
        download = nil
        loading = false
        state = nil
        message = nil
    }

    // MARK: Transport

    func play() {
        guard session != nil else { return }
        clock.play(at: now())
        tick()
    }

    func pause() {
        clock.pause(at: now())
        tick()
    }

    func togglePlay() { transport.isPlaying ? pause() : play() }

    func seek(to ms: Int64, exact: Bool = true) {
        scrubbing = !exact
        clock.seek(to: ms, at: now())
        tick()
    }

    func skip(by delta: Int64) { seek(to: transport.skipped(by: delta)) }

    func setRate(_ new: Double) {
        guard PlaybackSpeed.all.contains(new), new != rate else { return }
        rate = new
        clock.setRate(new, at: now())
        tick()
    }

    // MARK: The clock

    private func startTimer() {
        timer?.invalidate()
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    /// Reads the clock (ten times a second, and after every transport call).
    func tick() {
        let t = now()
        let p = clock.position(at: t), s = clock.state(at: t)
        if p != positionMs { positionMs = p }
        if s != state { state = s }
        updateAudio()
    }

    private func updateAudio() {
        guard let plan, let session else { return audio.close() }
        let source = audibleSource
        let position: Int64? = source.flatMap { s in
            switch plan.kind {
            case .follow: EditTimeline.shot(plan, at: positionMs)?.filePositionMs
            case .grid: plan.source(s).map { EditTimeline.filePosition($0, at: positionMs) }
            }
        }
        audio.update(
            source: source, url: source.flatMap { session.files[$0]?.url }, filePositionMs: position,
            playing: state == .playing, volume: volume)
    }

    /// The source whose sound the audio engine plays (the self-test).
    var playingSoundSource: Int? { audio.active }
}

/// The edit's sound: one `AVPlayer` per source file, the audible one playing
/// at the shown position and kept within a quarter second of the picture's
/// clock, the others paused. A file without sound plays nothing.
@MainActor
final class EditAudio {
    private var players: [Int: AVPlayer] = [:]
    private(set) var active: Int?
    static let maxDriftMs: Int64 = 250

    func update(source: Int?, url: URL?, filePositionMs: Int64?, playing: Bool, volume: Float) {
        if active != source, let old = active { players[old]?.pause() }
        active = source
        guard let source, let url, let position = filePositionMs, playing else {
            if let a = active { players[a]?.pause() }
            return
        }
        let player = players[source] ?? {
            let p = AVPlayer(url: url)
            p.automaticallyWaitsToMinimizeStalling = false
            players[source] = p
            return p
        }()
        player.volume = volume
        let seconds = player.currentTime().seconds
        let at: Int64? = seconds.isFinite ? Int64((seconds * 1000).rounded()) : nil
        if player.rate == 0 || at.map({ abs($0 - position) > Self.maxDriftMs }) ?? true {
            player.seek(to: CMTime(value: position, timescale: 1000), toleranceBefore: .zero, toleranceAfter: .zero)
            player.play()
        }
    }

    func close() {
        for p in players.values { p.pause() }
        players = [:]
        active = nil
    }
}
