import AppKit
import NVRCore

/// The app's camera motion (docs/UX.md "Pan, tilt, and zoom", docs/CAPTURE.md):
/// what each camera can do (`nv_ptz_info`, read off the main thread), the
/// held move (renewed while a control is held, stopped on release, window
/// close, tile close and focus loss), preset recall, and lens positions.
/// These are the user's own controls and need no enable; the core still
/// rate-limits and logs every command, and stops the camera by itself a
/// second after the last renewal.
@MainActor
final class PTZModel {
    private unowned let model: AppModel
    static let speedKey = "ptzSpeed"

    /// The pad's speed (1 to 64), kept between runs.
    var speed: Int {
        didSet {
            speed = min(max(speed, 1), 64)
            UserDefaults.standard.set(speed, forKey: Self.speedKey)
        }
    }

    /// What each camera can do, as the core last read it.
    private(set) var infos: [CameraRef: PTZInfo] = [:]
    /// The latest line to show for a camera (a refusal or a failure).
    private(set) var problems: [CameraRef: String] = [:]
    /// Reads of `nv_ptz_info` that finished (self-test).
    private(set) var infoReads = 0

    private var loading: Set<CameraRef> = []
    private var waiting: [CameraRef: [(PTZInfo?) -> Void]] = [:]
    private var hold = PTZHold()
    /// The camera being moved, while a control is held.
    private(set) var held: CameraRef?
    /// The arrow key that holds the move, if a key does.
    private(set) var keyHeld: PTZDirection?
    private var lensPositions: [CameraRef: [PTZLensAxis: Int]] = [:]
    private var throttles: [CameraRef: [PTZLensAxis: PTZLensThrottle]] = [:]
    /// The camera's positions are read again at this time (about 6 s after a
    /// zoom or a preset recall, when the camera has settled).
    private var rereadAt: [CameraRef: Double] = [:]
    static let rereadDelay: Double = 6
    private var timer: Timer?

    /// What observers hear.
    enum Change {
        case state
        case problem(CameraRef, String)
    }
    private var observers: [Int: (Change) -> Void] = [:]
    private var nextObserver = 0

    /// Listens for changes (popovers refresh; stages show a line over the
    /// tile). Returns the token to stop with.
    func addObserver(_ f: @escaping (Change) -> Void) -> Int {
        nextObserver += 1
        observers[nextObserver] = f
        return nextObserver
    }

    func removeObserver(_ token: Int) { observers[token] = nil }

    private func onChange() { for f in observers.values { f(.state) } }
    private func onProblem(_ camera: CameraRef, _ text: String) { for f in observers.values { f(.problem(camera, text)) } }

    init(model: AppModel) {
        self.model = model
        let saved = UserDefaults.standard.integer(forKey: Self.speedKey)
        speed = saved == 0 ? 20 : min(max(saved, 1), 64)
        NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in MainActor.assumeIsolated { self?.stopAll() } }
    }

    private var now: Double { model.clock.now }

    private func tile(_ camera: CameraRef) -> CoreTile? { model.leases.tile(for: camera) }

    // MARK: What a camera can do

    /// The camera's capabilities if they are known.
    func info(for camera: CameraRef) -> PTZInfo? { infos[camera] }

    /// Runs `body` with the camera's capabilities: at once when known, else
    /// after reading them (nil when the camera did not answer).
    func withInfo(_ camera: CameraRef, _ body: @escaping (PTZInfo?) -> Void) {
        if let info = infos[camera] {
            body(info)
            return
        }
        waiting[camera, default: []].append(body)
        load(camera)
    }

    /// Reads the camera's capabilities and lens positions (`nv_ptz_info`),
    /// which waits for the camera: off the main thread.
    func load(_ camera: CameraRef, force: Bool = false) {
        guard let tile = tile(camera), !loading.contains(camera), force || infos[camera] == nil else { return }
        loading.insert(camera)
        let bridge = model.bridge
        Task.detached {
            let result: Result<PTZInfo, CoreStatus>
            do throws(CoreStatus) {
                result = .success(try bridge.ptzInfo(tile))
            } catch {
                result = .failure(error)
            }
            await MainActor.run { [weak self] in self?.finishLoad(camera, result) }
        }
    }

    private func finishLoad(_ camera: CameraRef, _ result: Result<PTZInfo, CoreStatus>) {
        loading.remove(camera)
        infoReads += 1
        let callbacks = waiting.removeValue(forKey: camera) ?? []
        switch result {
        case .success(let info):
            infos[camera] = info
            problems[camera] = nil
            var positions = lensPositions[camera] ?? [:]
            for axis in PTZLensAxis.allCases {
                if let r = info.range(axis) { positions[axis] = r.cur }
            }
            lensPositions[camera] = positions
            for body in callbacks { body(info) }
        case .failure:
            let text = "The camera did not answer"
            problems[camera] = text
            onProblem(camera, text)
            for body in callbacks { body(nil) }
        }
        onChange()
    }

    /// The lens position to show for an axis: the latest sent or read.
    func lensPosition(_ camera: CameraRef, _ axis: PTZLensAxis) -> Int? {
        lensPositions[camera]?[axis] ?? infos[camera]?.range(axis)?.cur
    }

    // MARK: Moving (pad buttons and keys)

    /// A direction control goes down: the move starts, and is renewed until
    /// `release`.
    func press(_ direction: PTZDirection, camera: CameraRef) {
        guard tile(camera) != nil else { return }
        if let h = held, h != camera { release() }
        let call = hold.press(direction, speed: speed, now: now)
        held = camera
        startTimer()
        send(call, camera)
    }

    /// The control comes up (or the window closes, the tile closes, the
    /// focus is lost): the camera is stopped.
    func release() {
        keyHeld = nil
        timer?.invalidate()
        timer = nil
        guard let camera = held else {
            _ = hold.release()
            return
        }
        held = nil
        if let call = hold.release() { send(call, camera) }
        onChange()
    }

    /// Stops whatever is held, for any reason that is not a release.
    func stopAll() { release() }

    /// A key holds the move (Option with an arrow). Key repeats change nothing.
    func keyDown(_ direction: PTZDirection, camera: CameraRef) {
        if keyHeld == direction, held == camera { return }
        keyHeld = direction
        press(direction, camera: camera)
        keyHeld = direction
    }

    /// An arrow key came up: the move it held ends.
    func keyUp(_ direction: PTZDirection) {
        guard keyHeld == direction else { return }
        release()
    }

    /// Option came up while an arrow key still holds a move.
    func modifiersChanged(optionDown: Bool) {
        if !optionDown, keyHeld != nil { release() }
    }

    /// The tile of a camera closed: its move, readings and positions go.
    func cameraClosed(_ camera: CameraRef) {
        if held == camera { release() }
        infos[camera] = nil
        loading.remove(camera)
        lensPositions[camera] = nil
        throttles[camera] = nil
        rereadAt[camera] = nil
    }

    /// Time passes (the stage's tick): renews a held move, sends the lens
    /// positions a drag is waiting to send, and reads the lens again when the
    /// camera has settled.
    func tick(now: Double) {
        if let call = hold.tick(now: now), let camera = held {
            send(call, camera)
        }
        for (camera, axes) in throttles {
            for axis in axes.keys {
                if let p = throttles[camera]?[axis]?.tick(now: now) { sendLens(camera, axis, p) }
            }
        }
        for (camera, at) in rereadAt where now >= at {
            rereadAt[camera] = nil
            load(camera, force: true)
        }
    }

    private func startTimer() {
        guard timer == nil else { return }
        // The stage's tick renews too; this one keeps a held move alive when
        // the stage is not refreshing (a window behind others).
        let t = Timer(timeInterval: PTZHold.renewInterval / 2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick(now: self?.now ?? 0) }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func send(_ call: PTZHold.Call, _ camera: CameraRef) {
        guard let tile = tile(camera) else { return }
        do throws(CoreStatus) {
            switch call {
            case .move(let d, let speed): try model.bridge.ptzMove(tile, d, speed: speed)
            case .stop: try model.bridge.ptzStop(tile)
            }
        } catch {
            fail(camera, error)
        }
    }

    private func fail(_ camera: CameraRef, _ status: CoreStatus) {
        let text = "The camera was not moved (status \(status.code))"
        problems[camera] = text
        onProblem(camera, text)
        onChange()
    }

    // MARK: Presets and lens

    /// Recalls a preset of the camera's list.
    func recall(_ id: Int, camera: CameraRef) {
        guard let tile = tile(camera) else { return }
        do throws(CoreStatus) {
            try model.bridge.ptzPreset(tile, id: id)
            rereadAt[camera] = now + Self.rereadDelay
        } catch {
            fail(camera, error)
        }
    }

    /// Option with 1 to 9: the nth preset of the list (the order the camera
    /// gives). False when the camera has no such preset.
    @discardableResult
    func recallNth(_ n: Int, camera: CameraRef) -> Bool {
        guard let info = infos[camera], (1...info.presets.count).contains(n) else { return false }
        recall(info.presets[n - 1].id, camera: camera)
        return true
    }

    /// Option with = or -: one step of zoom within the camera's range.
    func zoomStep(_ direction: Int, camera: CameraRef) {
        stepLens(.zoom, direction, camera: camera)
    }

    func stepLens(_ axis: PTZLensAxis, _ direction: Int, camera: CameraRef) {
        guard let range = infos[camera]?.range(axis) else { return }
        let current = lensPosition(camera, axis) ?? range.cur
        sendLens(camera, axis, range.stepped(from: current, by: direction))
    }

    /// A slider drag reached `position`: sent at most five times a second.
    func dragLens(_ axis: PTZLensAxis, to position: Int, camera: CameraRef) {
        guard let range = infos[camera]?.range(axis) else { return }
        let p = range.clamped(position)
        lensPositions[camera, default: [:]][axis] = p
        if let send = throttles[camera, default: [:]][axis, default: PTZLensThrottle()].moved(to: p, now: now) {
            sendLens(camera, axis, send)
        }
    }

    /// The drag ended: the last position goes out.
    func endLensDrag(_ axis: PTZLensAxis, camera: CameraRef) {
        if let p = throttles[camera]?[axis]?.end() { sendLens(camera, axis, p) }
    }

    private func sendLens(_ camera: CameraRef, _ axis: PTZLensAxis, _ position: Int) {
        guard let tile = tile(camera) else { return }
        let p = infos[camera]?.range(axis)?.clamped(position) ?? position
        lensPositions[camera, default: [:]][axis] = p
        do throws(CoreStatus) {
            try model.bridge.ptzLens(tile, axis, position: p)
            if axis == .zoom { rereadAt[camera] = now + Self.rereadDelay }
        } catch {
            fail(camera, error)
        }
        onChange()
    }

    // MARK: Events

    /// A `ptz` event: refusals and failures become a line over the tile and
    /// in the popover; the core's own stops end the app's hold.
    func handle(_ event: PTZEvent) {
        guard let camera = model.leases.camera(forTile: event.tile) else { return }
        if event.action == "stop", event.state == .sent, let reason = event.reason,
            ["max_hold", "device_refused", "session_closed", "reconnected", "deadman"].contains(reason),
            held == camera
        {
            // The core stopped the camera by itself (the 30 s limit, a
            // refusal): the app's hold is over too.
            release()
            if reason == "max_hold" { announce(camera, "Stopped after 30 seconds; release and press again") }
        }
        if let text = event.problem { announce(camera, text) }
    }

    private func announce(_ camera: CameraRef, _ text: String) {
        problems[camera] = text
        onProblem(camera, text)
        onChange()
    }

    /// Clears the line for a camera (a popover opening).
    func clearProblem(_ camera: CameraRef) { problems[camera] = nil }
}
