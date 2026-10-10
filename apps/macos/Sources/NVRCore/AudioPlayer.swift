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
    private let engine = AVAudioEngine()
    private var source: AVAudioSourceNode?
    private var observer: NSObjectProtocol?
    private var running = false
    private var hostBaseNanos: UInt64 = 0
    private var offlineBuffer: AVAudioPCMBuffer?
    /// Interleaved scratch for the render block, freed once its node is
    /// detached from a stopped engine.
    private var scratch: UnsafeMutablePointer<Float>?
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
        get { lock.withLock { engine.mainMixerNode.outputVolume } }
        set { lock.withLock { engine.mainMixerNode.outputVolume = newValue } }
    }

    /// Builds the graph, tells the core the format, and starts the engine.
    /// `hostBaseNanos` is the host time of sample 0 when the render
    /// timestamps carry no host time (offline mode).
    public func start(hostBaseNanos: UInt64 = 0) throws {
        try lock.withLock {
            guard !running else { return }
            self.hostBaseNanos = hostBaseNanos
            if case .offline(let rate, let maxFrames) = mode {
                let format = Self.format(sampleRate: rate)
                try engine.enableManualRenderingMode(.offline, format: format, maximumFrameCount: maxFrames)
                offlineBuffer = AVAudioPCMBuffer(pcmFormat: engine.manualRenderingFormat, frameCapacity: maxFrames)
            } else {
                observer = NotificationCenter.default.addObserver(
                    forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil
                ) { [weak self] _ in self?.reconfigure() }
            }
            try build()
            try engine.start()
            running = true
        }
    }

    /// Stops the engine: no render call is in progress or will start after
    /// this returns. Idempotent.
    public func stop() {
        lock.withLock {
            if let observer { NotificationCenter.default.removeObserver(observer) }
            observer = nil
            guard running else { return }
            running = false
            engine.stop()
            detachSource()
        }
    }

    /// With the engine stopped: no render block runs any more.
    private func detachSource() {
        if let source {
            engine.detach(source)
            self.source = nil
        }
        scratch?.deallocate()
        scratch = nil
    }

    /// Offline mode: renders `frames` frames through the engine (pulling the
    /// core) and returns the interleaved samples.
    public func renderOffline(frames: AVAudioFrameCount) throws -> [Float] {
        try lock.withLock {
            guard running, let buffer = offlineBuffer else { throw CocoaError(.featureUnsupported) }
            let status = try engine.renderOffline(frames, to: buffer)
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

    /// Called with `lock` held: a source node for the current output rate,
    /// connected to the main mixer, and the core told the format.
    private func build() throws {
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
        let status = configure(UInt32(rate.rounded()), Self.channels)
        guard status.isOK else { throw status }
        let buffer = UnsafeMutablePointer<Float>.allocate(capacity: Self.scratchFrames * Int(Self.channels))
        scratch = buffer
        let block = Self.makeRenderBlock(
            hook: hook, scratch: buffer, sampleRate: rate, latencyNanos: latencyNanos,
            hostBaseNanos: hostBaseNanos, timebase: HostClock.timebase)
        let node = AVAudioSourceNode(format: Self.format(sampleRate: rate), renderBlock: block)
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: Self.format(sampleRate: rate))
        source = node
        sampleRate = rate
    }

    /// The device or its format changed; the engine has stopped.
    private func reconfigure() {
        lock.withLock {
            guard running else { return }
            engine.stop()
            detachSource()
            do {
                try build()
                try engine.start()
                reconfigurations.add(1, ordering: .relaxed)
            } catch {
                // No output device now; the next change notification retries.
            }
        }
    }

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
