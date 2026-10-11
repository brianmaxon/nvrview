import AVFoundation
import Foundation
import Synchronization

/// Plays the core's audio: an `AVAudioEngine` whose `AVAudioSourceNode`
/// pulls every buffer from the core's real-time render function
/// (`nv_audio_render`, through an `AudioRenderHook`). All timing decisions
/// are the core's; this only tells it the output format and when each buffer
/// will be heard.
///
/// **The render block is real-time safe.** It is built by `makeRenderBlock`
/// from plain values only (a C function pointer, a raw context pointer, and
/// integers), so running it touches no Swift object: no retain or release,
/// no allocation, no lock, no Objective-C message. It reads the buffer list
/// through `UnsafeMutableAudioBufferListPointer` (a struct view, no copy),
/// converts the timestamp's host time with integer arithmetic, has the C
/// function fill a scratch buffer allocated when the node was built, and
/// copies that into the node's channel buffers. Everything that can
/// allocate or lock (building nodes and their scratch buffers, reading
/// formats, telling the core the format) happens on other threads.
///
/// **Format.** The core renders interleaved stereo; the source node has the
/// engine's standard (deinterleaved) Float32 stereo format at the output
/// device's sample rate, so the engine does no resampling (its mixer does
/// not take interleaved input). The core is told the format with
/// `audioSetOutput`. After a device or sample-rate
/// change (`AVAudioEngineConfigurationChange`) the player rebuilds the node
/// for the new rate, tells the core, and restarts.
///
/// **Time.** `host_time_ns` is when the buffer's first frame is heard: the
/// timestamp's host time (mach ticks, converted to ns with the timebase,
/// the core's clock) plus the output's presentation latency. When the
/// timestamp has no host time (offline manual rendering in tests), it is
/// `hostBaseNanos` plus the sample time.
///
/// **Voice processing** (docs/AUDIO.md "Echo cancellation"). While a call's
/// microphone is on, `startVoiceProcessing` moves playback to an engine
/// whose input node has the system's voice processing (echo cancellation
/// and noise suppression) enabled, which enables it on the output too: the
/// canceller then knows exactly what the speakers play (the doorbell) and
/// removes it from the microphone, which is tapped on that engine.
/// `stopVoiceProcessing` moves playback back to a plain engine, since voice
/// processing lowers output quality, and an engine that had it cannot be
/// turned back (measured on macOS 26: after disabling it the output node
/// reports 0 channels and the engine does not start).
///
/// Measured on one Mac (macOS 26; the longest pause between render calls,
/// in `VoiceProcessingTapTests`): enabling voice processing takes about 1 s
/// and pauses the engine that is playing for 0.3 to 0.5 s; switching from a
/// prepared plain engine to a prepared voice-processing one pauses playback
/// for 0.07 to 0.11 s; switching back for 0.85 to 1 s (the device leaving
/// voice processing). So the slow work runs on a queue of its own while the
/// current engine keeps playing, and the switches happen when they hurt
/// least: `prepareVoiceProcessing` builds the voice-processing engine when a
/// call opens (a built engine that was not started does not run the input
/// device: no microphone in use, measured); Talk only switches to it; after
/// Talk the engine lingers (`stopVoiceProcessing(linger:)`, 2 minutes), so
/// pressing Talk again is instant, and playback goes back to a plain engine
/// when the call ends (`discardVoiceProcessing`) or the linger runs out.
/// While it lingers the input device keeps running with nothing taken from
/// it, and the system shows the microphone in use. Never two render blocks
/// on the core at once: each switch stops one engine and starts the other.
///
/// Voice processing reports a multichannel input (9 channels measured, all
/// carrying the same processed signal there); only the first is converted. When the devices change while
/// it runs, the engine is rebuilt with voice processing; if that fails (or
/// keeps failing), the player goes back to plain playback and calls
/// `onLost`, and the microphone falls back to an engine of its own (and the
/// caption gate).
///
/// Stop the player before the core is freed (`CoreBridge.shutdown` stops
/// every player made with `CoreBridge.makeAudioPlayer`).
public final class AudioPlayer: @unchecked Sendable {
    public enum Mode: Sendable {
        /// The default output device.
        case device
        /// Offline manual rendering (tests): nothing reaches a speaker; the
        /// caller pulls buffers with `renderOffline(frames:)`.
        case offline(sampleRate: Double, maximumFrames: AVAudioFrameCount)
    }

    public static let channels: UInt32 = 2

    private let hook: AudioRenderHook
    private let configure: @Sendable (_ sampleRate: UInt32, _ channels: UInt32) -> CoreStatus
    private let mode: Mode
    // State below is touched only under `lock`, never by the render block.
    private let lock = NSRecursiveLock()
    /// The engine that plays (replaced when voice processing starts or stops).
    private var current = Graph()
    /// A voice-processing engine built ahead of Talk, not started.
    private var warm: Graph?
    /// Keep a voice-processing engine built (a call is open).
    private var keepWarm = false
    private var observer: NSObjectProtocol?
    private var running = false
    private var hostBaseNanos: UInt64 = 0
    private var offlineBuffer: AVAudioPCMBuffer?
    private var volume: Float = 1
    /// The microphone on this engine, while voice processing runs.
    private var capture: Capture?
    /// Bumped by every request to start or stop voice processing, so the
    /// slow work for an earlier one is dropped.
    private var wanted = 0
    /// When voice-processing engines were rebuilt after device changes.
    private var rebuilds: [Double] = []
    /// The slow work (building voice-processing engines), in order.
    private let queue = DispatchQueue(label: "nvrview.audio.voice-processing")
    static let scratchFrames = 4096
    private let reconfigurations = Atomic<Int>(0)

    /// `configure` tells the core the output format (`nv_audio_set_output`).
    public init(
        hook: AudioRenderHook, mode: Mode = .device,
        configure: @escaping @Sendable (_ sampleRate: UInt32, _ channels: UInt32) -> CoreStatus
    ) {
        self.hook = hook
        self.mode = mode
        self.configure = configure
    }

    deinit { stop() }

    /// The sample rate the source node renders at, once started.
    public private(set) var sampleRate: Double = 0
    /// How often the output configuration changed and was rebuilt.
    public var reconfigurationCount: Int { reconfigurations.load(ordering: .relaxed) }
    /// The engine's main mixer volume (0 silences the device without
    /// touching the core's per-tile volumes).
    public var outputVolume: Float {
        get { lock.withLock { volume } }
        set {
            lock.withLock {
                volume = newValue
                current.engine.mainMixerNode.outputVolume = newValue
            }
        }
    }

    /// Whether the microphone runs on this player's engine with voice processing.
    public var isVoiceProcessing: Bool { lock.withLock { capture != nil } }
    /// Whether playback runs on a voice-processing engine (with the
    /// microphone, or lingering after it).
    public var playsThroughVoiceProcessing: Bool { lock.withLock { running && current.voice } }
    /// Whether a voice-processing engine is built and waiting for Talk.
    public var isVoiceProcessingPrepared: Bool { lock.withLock { warm != nil } }

    /// Builds the graph, tells the core the format, and starts the engine.
    /// `hostBaseNanos` is the host time of sample 0 when the render
    /// timestamps carry no host time (offline mode).
    public func start(hostBaseNanos: UInt64 = 0) throws {
        try lock.withLock {
            guard !running else { return }
            self.hostBaseNanos = hostBaseNanos
            if case .offline(let rate, let maxFrames) = mode {
                let format = Self.format(sampleRate: rate)
                try current.engine.enableManualRenderingMode(.offline, format: format, maximumFrameCount: maxFrames)
                offlineBuffer = AVAudioPCMBuffer(
                    pcmFormat: current.engine.manualRenderingFormat, frameCapacity: maxFrames)
            } else {
                observe()
            }
            try current.addSource(hook: hook, mode: mode, hostBaseNanos: hostBaseNanos)
            try tellCore(current)
            try current.engine.start()
            running = true
        }
    }

    /// Stops the engine: no render call is in progress or will start after
    /// this returns. Idempotent.
    public func stop() {
        lock.withLock {
            if let observer { NotificationCenter.default.removeObserver(observer) }
            observer = nil
            keepWarm = false
            wanted += 1
            capture = nil
            warm?.tearDown()
            warm = nil
            guard running else { return }
            running = false
            current.tearDown()
        }
    }

    /// Offline mode: renders `frames` frames through the engine (pulling the
    /// core) and returns the interleaved samples.
    public func renderOffline(frames: AVAudioFrameCount) throws -> [Float] {
        try lock.withLock {
            guard running, let buffer = offlineBuffer else { throw CocoaError(.featureUnsupported) }
            let status = try current.engine.renderOffline(frames, to: buffer)
            guard status == .success else { throw CocoaError(.fileReadUnknown) }
            let n = Int(buffer.frameLength)
            let list = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
            let channels = Int(buffer.format.channelCount)
            var out = [Float](repeating: 0, count: n * channels)
            if buffer.format.isInterleaved {
                if let d = list[0].mData { out.withUnsafeMutableBytes { $0.copyMemory(from: UnsafeRawBufferPointer(start: d, count: $0.count)) } }
            } else {
                for c in 0..<channels {
                    guard let d = list[c].mData?.assumingMemoryBound(to: Float.self) else { continue }
                    for i in 0..<n { out[i * channels + c] = d[i] }
                }
            }
            return out
        }
    }

    // MARK: Graph

    private static func format(sampleRate: Double) -> AVAudioFormat {
        AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: AVAudioChannelCount(channels))!
    }

    /// One engine with its source node (and, with voice processing, the
    /// microphone tap). Touched under the player's lock, or by one queue job
    /// before it is handed over.
    private final class Graph: @unchecked Sendable {
        let engine = AVAudioEngine()
        let voice: Bool
        private var source: AVAudioSourceNode?
        /// Interleaved scratch for the render block, freed once its node is
        /// detached from a stopped engine.
        private var scratch: UnsafeMutablePointer<Float>?
        /// The input node has the microphone tap. (Only then is the input
        /// node touched: creating one opens the input device.)
        private var tapped = false
        private(set) var rate: Double = 0

        init() { voice = false }

        /// A voice-processing engine (slow: see the type's documentation).
        init(voice: Bool) throws {
            self.voice = voice
            guard voice else { return }
            // The main mixer (and its connection to the output) must exist
            // before voice processing is enabled: made after, the output unit
            // fails to initialize when the engine starts (measured on macOS
            // 26, error -10875).
            _ = engine.mainMixerNode
            try engine.inputNode.setVoiceProcessingEnabled(true)
            // Other apps' audio is lowered as little as the system allows.
            engine.inputNode.voiceProcessingOtherAudioDuckingConfiguration = .init(
                enableAdvancedDucking: false, duckingLevel: .min)
        }

        /// A source node for the output's rate, connected to the main mixer
        /// (the core is told the rate when the engine takes over).
        func addSource(hook: AudioRenderHook, mode: Mode, hostBaseNanos: UInt64) throws {
            let rate: Double
            let latencyNanos: UInt64
            switch mode {
            case .offline(let r, _):
                rate = r
                latencyNanos = 0
            case .device:
                rate = engine.outputNode.outputFormat(forBus: 0).sampleRate
                latencyNanos = UInt64(max(engine.outputNode.presentationLatency, 0) * 1e9)
            }
            guard rate > 0 else { throw CocoaError(.featureUnsupported) }
            let buffer = UnsafeMutablePointer<Float>.allocate(capacity: AudioPlayer.scratchFrames * Int(AudioPlayer.channels))
            scratch = buffer
            let block = AudioPlayer.makeRenderBlock(
                hook: hook, scratch: buffer, sampleRate: rate, latencyNanos: latencyNanos,
                hostBaseNanos: hostBaseNanos, timebase: HostClock.timebase)
            let node = AVAudioSourceNode(format: AudioPlayer.format(sampleRate: rate), renderBlock: block)
            engine.attach(node)
            engine.connect(node, to: engine.mainMixerNode, format: AudioPlayer.format(sampleRate: rate))
            source = node
            self.rate = rate
        }

        /// The microphone tap (voice-processing engines only).
        func tap(_ onSamples: @escaping @Sendable ([Float]) -> Void) throws {
            guard voice else { throw VoiceProcessingError("not a voice-processing engine") }
            removeTap()
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else { throw VoiceProcessingError("no microphone input") }
            input.installTap(
                onBus: 0, bufferSize: 1024, format: format,
                block: TalkAudioConverter.tapBlock(onSamples, firstChannelOnly: true))
            tapped = true
        }

        func removeTap() {
            guard tapped else { return }
            engine.inputNode.removeTap(onBus: 0)
            tapped = false
        }

        /// With the engine stopped: no render block runs any more.
        func removeSource() {
            if let source {
                engine.detach(source)
                self.source = nil
            }
            scratch?.deallocate()
            scratch = nil
        }

        func tearDown() {
            engine.stop()
            removeTap()
            removeSource()
        }
    }

    /// Called with `lock` held: follows the current engine's configuration changes.
    private func observe() {
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: current.engine, queue: nil
        ) { [weak self] _ in self?.reconfigure() }
    }

    /// Called with `lock` held: tells the core the graph's format.
    private func tellCore(_ g: Graph) throws {
        let status = configure(UInt32(g.rate.rounded()), Self.channels)
        guard status.isOK else { throw status }
        sampleRate = g.rate
    }

    /// A built engine, not started (slow for voice processing; run it
    /// without the lock).
    private func makeGraph(voice: Bool) throws -> Graph {
        let base = lock.withLock { hostBaseNanos }
        let g = try Graph(voice: voice)
        do {
            try g.addSource(hook: hook, mode: mode, hostBaseNanos: base)
            // Prepared now, while the current engine still plays, so the
            // switch only stops one engine and starts the other. A prepared
            // engine does not run the input device (measured).
            g.engine.prepare()
        } catch {
            g.tearDown()
            throw error
        }
        return g
    }

    /// Called with `lock` held: `g` takes over from the current engine,
    /// which stops first.
    private func install(_ g: Graph) throws {
        current.tearDown()
        current = g
        observe()
        g.engine.mainMixerNode.outputVolume = volume
        try tellCore(g)
        try g.engine.start()
    }

    /// Called with `lock` held: back to a plain engine (voice processing
    /// failed). Without an output device the next change notification retries.
    private func fallBackToPlain() {
        capture = nil
        if let g = try? makeGraph(voice: false) { try? install(g) }
    }

    /// The device or its format changed; the engine has stopped.
    private func reconfigure() {
        lock.withLock {
            guard running else { return }
            // A waiting voice-processing engine was built for the old devices.
            warm?.tearDown()
            warm = nil
            if current.voice {
                // A notice for an engine that still runs (voice processing
                // may post some while it settles) needs nothing.
                guard !current.engine.isRunning else { return }
                if capture != nil {
                    queue.async { [weak self] in self?.rebuildVoiceProcessing() }
                } else {
                    // Lingering after Talk: no need for voice processing now.
                    fallBackToPlain()
                }
                return
            }
            current.engine.stop()
            current.removeSource()
            do {
                try current.addSource(hook: hook, mode: mode, hostBaseNanos: hostBaseNanos)
                try tellCore(current)
                try current.engine.start()
                reconfigurations.add(1, ordering: .relaxed)
            } catch {
                // No output device now; the next change notification retries.
            }
        }
        queue.async { [weak self] in self?.warmUp() }
    }

    // MARK: Voice processing

    /// A call is open: builds a voice-processing engine now (on a queue), so
    /// the first Talk only has to switch to it. Kept until it is used,
    /// `discardVoiceProcessing`, or a device change (then built again).
    /// Building it pauses the playing engine (0.3 to 0.5 s measured), so this
    /// is done when a call opens, not during the conversation.
    public func prepareVoiceProcessing() {
        let go = lock.withLock { () -> Bool in
            guard running, case .device = mode else { return false }
            keepWarm = true
            return true
        }
        if go { queue.async { [weak self] in self?.warmUp() } }
    }

    /// No call is open: the waiting voice-processing engine goes, and a
    /// lingering one hands back to a plain engine now.
    public func discardVoiceProcessing() {
        let ticket = lock.withLock { () -> Int in
            keepWarm = false
            warm?.tearDown()
            warm = nil
            return wanted
        }
        queue.async { [weak self] in self?.leaveVoiceProcessing(ticket) }
    }

    /// On the queue: builds the waiting engine if one is wanted.
    private func warmUp() {
        let needed = { [self] in keepWarm && running && warm == nil && capture == nil }
        guard lock.withLock(needed), let g = try? makeGraph(voice: true) else { return }
        lock.withLock {
            if needed() { warm = g } else { g.tearDown() }
        }
    }

    /// Starts the microphone on this player's engine with the system's voice
    /// processing (see the type's documentation). `onSamples` gets 16 kHz
    /// mono blocks on an audio thread that is not the real-time one. `done`
    /// is called once, on a queue of the player's: nil when voice processing
    /// runs, else why not (playback is then on a plain engine). `onLost` is
    /// called (on any thread) if voice processing stops working later,
    /// after playback went back to a plain engine.
    public func startVoiceProcessing(
        onSamples: @escaping @Sendable ([Float]) -> Void, onLost: @escaping @Sendable (String) -> Void,
        done: @escaping @Sendable (VoiceProcessingError?) -> Void
    ) {
        let ticket = lock.withLock {
            wanted += 1
            return wanted
        }
        let c = Capture(onSamples: onSamples, onLost: onLost)
        queue.async { [self] in done(switchToVoiceProcessing(ticket, c)) }
    }

    /// Stops the microphone at once (nothing more is tapped after this
    /// returns). The voice-processing engine keeps playing for `linger`
    /// seconds, so Talk pressed again soon needs no switch (and no gap);
    /// then playback moves back to a plain engine. While it lingers the
    /// input device keeps running (the system shows the microphone in use)
    /// and nothing is taken from it.
    public func stopVoiceProcessing(linger: Double = AudioPlayer.voiceProcessingLinger) {
        let ticket = lock.withLock { () -> Int in
            wanted += 1
            if capture != nil {
                capture = nil
                current.removeTap()
            }
            return wanted
        }
        queue.asyncAfter(deadline: .now() + linger) { [weak self] in self?.leaveVoiceProcessing(ticket) }
    }

    /// How long a voice-processing engine keeps playing after Talk, s.
    public static let voiceProcessingLinger = 120.0

    /// On the queue: back to a plain engine unless Talk was pressed again.
    private func leaveVoiceProcessing(_ ticket: Int) {
        let idle = { [self] in ticket == wanted && running && capture == nil && current.voice }
        guard lock.withLock(idle) else { return }
        let g = try? makeGraph(voice: false)
        lock.withLock {
            guard idle(), let g else {
                g?.tearDown()
                return
            }
            // On failure the next change notification retries.
            try? install(g)
        }
        // Not built again ahead of the next Talk: building one pauses the
        // playing engine (0.3 to 0.5 s measured), and a conversation is
        // more likely to be going on now than when the call opened.
    }

    private struct Capture {
        let onSamples: @Sendable ([Float]) -> Void
        let onLost: @Sendable (String) -> Void
    }

    /// On the queue: the waiting engine (or one built now) takes over with
    /// the microphone tapped.
    private func switchToVoiceProcessing(_ ticket: Int, _ c: Capture) -> VoiceProcessingError? {
        // Still running (Talk pressed again while it lingers): only the tap
        // is put back.
        let retapped = lock.withLock { () -> VoiceProcessingError?? in
            guard ticket == wanted else { return .some(VoiceProcessingError("canceled")) }
            guard running, case .device = mode else { return .some(VoiceProcessingError("the audio output is not running")) }
            guard current.voice else { return .none }
            do {
                try current.tap(c.onSamples)
                capture = c
                return .some(nil)
            } catch {
                return .some(Self.describe(error))
            }
        }
        if let retapped { return retapped }
        var lastError = VoiceProcessingError("voice processing did not start")
        // The waiting engine first; it may be stale (a device changed in a
        // way that sent no notice), so its failure tries a new one once.
        var waiting = lock.withLock { () -> Graph? in
            defer { warm = nil }
            return warm
        }
        let attempts = waiting == nil ? 1 : 2
        for _ in 0..<attempts {
            let g: Graph
            if let w = waiting {
                g = w
                waiting = nil
            } else {
                do {
                    g = try makeGraph(voice: true)
                } catch {
                    lastError = Self.describe(error)
                    break
                }
            }
            let outcome = lock.withLock { () -> VoiceProcessingError?? in
                guard ticket == wanted, running else {
                    g.tearDown()
                    return .some(VoiceProcessingError("canceled"))
                }
                do {
                    try g.tap(c.onSamples)
                    try install(g)
                    capture = c
                    rebuilds = []
                    return .some(nil)
                } catch {
                    lastError = Self.describe(error)
                    fallBackToPlain()
                    return .none
                }
            }
            if let outcome { return outcome }
        }
        return lastError
    }

    /// On the queue: the voice-processing engine stopped after a device
    /// change; a new one takes over, or playback goes back to a plain engine
    /// and the microphone is told.
    private func rebuildVoiceProcessing() {
        let allowed = lock.withLock { () -> Bool in
            let now = ProcessInfo.processInfo.systemUptime
            rebuilds = rebuilds.filter { now - $0 < Self.rebuildWindow } + [now]
            return rebuilds.count <= Self.maximumRebuilds
        }
        guard let c = lock.withLock({ capture }) else { return }
        let g = allowed ? try? makeGraph(voice: true) : nil
        let lost = lock.withLock { () -> Bool in
            guard running, capture != nil, !current.engine.isRunning else {
                g?.tearDown()
                return false
            }
            if let g {
                do {
                    try g.tap(c.onSamples)
                    try install(g)
                    reconfigurations.add(1, ordering: .relaxed)
                    return false
                } catch {}
            }
            fallBackToPlain()
            return true
        }
        if lost {
            c.onLost(allowed ? "voice processing stopped after an audio device change" : "the audio devices keep changing")
        }
    }

    private static func describe(_ error: any Error) -> VoiceProcessingError {
        (error as? VoiceProcessingError) ?? VoiceProcessingError("voice processing did not start (\((error as NSError).code))")
    }

    /// Voice-processing rebuilds allowed within `rebuildWindow` seconds
    /// before the player gives up on it.
    static let maximumRebuilds = 5
    static let rebuildWindow = 30.0

    /// The real-time render block. It captures only trivial values (see the
    /// type's documentation); keep it that way. `scratch` holds
    /// `scratchFrames` interleaved stereo frames.
    static func makeRenderBlock(
        hook: AudioRenderHook, scratch: UnsafeMutablePointer<Float>, sampleRate: Double, latencyNanos: UInt64,
        hostBaseNanos: UInt64, timebase: (numer: UInt64, denom: UInt64)
    ) -> AVAudioSourceNodeRenderBlock {
        let function = hook.function
        let context = hook.context
        let numer = timebase.numer
        let denom = timebase.denom
        let nanosPerFrame = 1e9 / sampleRate
        let chunk = UInt32(scratchFrames)
        return { _, timestamp, frameCount, outputData in
            let buffers = UnsafeMutableAudioBufferListPointer(outputData)
            guard buffers.count > 0 else { return noErr }
            let ts = timestamp.pointee
            let host: UInt64
            if ts.mFlags.contains(.hostTimeValid) {
                let (high, low) = ts.mHostTime.multipliedFullWidth(by: numer)
                host = denom.dividingFullWidth((high, low)).quotient &+ latencyNanos
            } else {
                host = hostBaseNanos &+ UInt64(max(ts.mSampleTime, 0) * nanosPerFrame)
            }
            // Never write past what a channel buffer holds.
            var frames = frameCount
            for b in buffers {
                frames = min(frames, b.mDataByteSize / UInt32(MemoryLayout<Float>.size))
            }
            let left = buffers[0].mData?.assumingMemoryBound(to: Float.self)
            let right = buffers.count > 1 ? buffers[1].mData?.assumingMemoryBound(to: Float.self) : nil
            var done: UInt32 = 0
            while done < frames {
                let n = min(chunk, frames - done)
                _ = function(context, scratch, n, 2, host &+ UInt64(Double(done) * nanosPerFrame))
                for i in 0..<Int(n) {
                    let l = scratch[2 * i]
                    let r = scratch[2 * i + 1]
                    if let right {
                        left?[Int(done) + i] = l
                        right[Int(done) + i] = r
                    } else {
                        left?[Int(done) + i] = 0.5 * (l + r)
                    }
                }
                done += n
            }
            return noErr
        }
    }
}

/// Why voice processing could not start, in words.
public struct VoiceProcessingError: Error, CustomStringConvertible, Sendable {
    public let description: String
    public init(_ description: String) { self.description = description }
}
