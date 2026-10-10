import AVFoundation
import Foundation
import Synchronization

/// Counters exposed instead of per-frame logging.
public struct RendererStats: Sendable, Equatable {
    public var framesReceived = 0
    public var framesEnqueued = 0
    /// Dropped by the keyframe gate (start, flush, format change, after a drop).
    public var droppedWaitingForKeyframe = 0
    /// Dropped because the input queue was full (consumer too slow).
    public var droppedQueueOverflow = 0
    /// Dropped because the display path was not ready for more data.
    public var droppedDisplayBusy = 0
    /// Decode failures observed on the display path (each one flushes).
    public var decodeFailures = 0
    public var formatChanges = 0
    public var flushes = 0
}

/// Owns one display path for one channel: frames arrive on any thread through
/// `submit`, are copied into a bounded queue, and are converted and enqueued
/// on a private serial queue to an `AVSampleBufferDisplayLayer`.
///
/// Policy: keyframe gating, drop-to-next-keyframe on overflow or a busy
/// display path, and flush-and-wait-for-keyframe on a decode failure. No
/// decoder is ever rebuilt here; `AVSampleBufferDisplayLayer` owns decode.
public final class ChannelRenderer: @unchecked Sendable {
    // @unchecked Sendable: every mutable field is behind a Mutex, and the layer
    // is only touched from the main actor (creation, geometry) or via `videoRenderer` on `drainQueue`.

    /// Add this to a view. Frames are shown as soon as decoded.
    public let displayLayer: AVSampleBufferDisplayLayer

    private struct Shared {
        var queue: BoundedFrameQueue
        var stats = RendererStats()
        var drainScheduled = false
    }

    private let shared: Mutex<Shared>
    private let pipeline = Mutex<FramePipeline?>(nil)
    private let drainQueue: DispatchQueue
    /// Thread-safe enqueue/flush surface of the layer (usable off the main thread).
    private let videoRenderer: AVSampleBufferVideoRenderer

    /// Create on the main actor (the layer is main-actor isolated); frames can
    /// then be submitted from any thread.
    @MainActor
    public init(label: String = "channel", queueCapacity: Int = 8) {
        displayLayer = AVSampleBufferDisplayLayer()
        displayLayer.videoGravity = .resizeAspect
        videoRenderer = displayLayer.sampleBufferRenderer
        shared = Mutex(Shared(queue: BoundedFrameQueue(capacity: queueCapacity)))
        drainQueue = DispatchQueue(label: "nvrview.render.\(label)", qos: .userInitiated)
    }

    public var stats: RendererStats { shared.withLock { $0.stats } }

    // MARK: Input (any thread, must return quickly)

    /// Submits one frame. The bytes are copied; the buffer is not retained.
    public func submit(
        codec: VideoCodec, isKeyframe: Bool, ptsMicros: Int64, annexB: UnsafeRawBufferPointer
    ) {
        submit(VideoFrame(codec: codec, isKeyframe: isKeyframe, ptsMicros: ptsMicros, annexB: Data(annexB)))
    }

    public func submit(_ frame: VideoFrame) {
        let schedule = shared.withLock { s -> Bool in
            s.stats.framesReceived += 1
            let dropped = s.queue.push(frame)
            s.stats.droppedQueueOverflow += dropped
            return Self.needsSchedule(&s)
        }
        if schedule { scheduleDrain() }
    }

    /// The stream format changed (a "format changed" event from the core).
    /// Parameter sets are optional NAL units without start codes; frames also
    /// carry them in band. Everything is dropped until the next keyframe.
    public func formatChanged(codec: VideoCodec? = nil, parameterSets: [Data] = []) {
        control(.formatChanged(codec: codec, parameterSets: parameterSets))
    }

    /// Flushes the display path and waits for the next keyframe (for example
    /// when the channel disconnects).
    public func flush() {
        control(.flush)
    }

    private func control(_ item: QueueItem) {
        let schedule = shared.withLock { s -> Bool in
            s.stats.droppedQueueOverflow += s.queue.pushControl(item)
            return Self.needsSchedule(&s)
        }
        if schedule { scheduleDrain() }
    }

    private static func needsSchedule(_ s: inout Shared) -> Bool {
        if s.drainScheduled { return false }
        s.drainScheduled = true
        return true
    }

    // MARK: Drain (serial queue)

    private func scheduleDrain() {
        drainQueue.async { [self] in drain() }
    }

    private func drain() {
        while true {
            let item = shared.withLock { s -> QueueItem? in
                if let item = s.queue.pop() { return item }
                s.drainScheduled = false
                return nil
            }
            guard let item else { return }
            handle(item)
        }
    }

    private func handle(_ item: QueueItem) {
        let renderer = videoRenderer
        switch item {
        case .flush:
            pipeline.withLock { $0?.closeGate() }
            flushDisplay()
        case .formatChanged(let codec, let sets):
            pipeline.withLock { p in
                if let codec, codec != p?.codec { p = FramePipeline(codec: codec) }
                p?.closeGate(forgetParameterSets: true)
                if !sets.isEmpty { p?.setParameterSets(sets) }
            }
            shared.withLock { $0.stats.formatChanges += 1 }
            flushDisplay()
        case .frame(let frame):
            // A failure shows up asynchronously after earlier enqueues. Recover
            // once: flush and wait for a keyframe, never rebuild in a loop.
            if renderer.status == .failed || renderer.requiresFlushToResumeDecoding {
                shared.withLock { $0.stats.decodeFailures += 1 }
                pipeline.withLock { $0?.closeGate() }
                flushDisplay()
            }
            let output = pipeline.withLock { p -> PipelineOutput in
                if p == nil || p?.codec != frame.codec { p = FramePipeline(codec: frame.codec) }
                return p!.process(frame)
            }
            switch output {
            case .dropped(let reason):
                if reason == .waitingForKeyframe {
                    shared.withLock { $0.stats.droppedWaitingForKeyframe += 1 }
                }
            case .sample(let sample, let formatChanged):
                if formatChanged {
                    shared.withLock { $0.stats.formatChanges += 1 }
                    flushDisplay()
                }
                if renderer.isReadyForMoreMediaData {
                    renderer.enqueue(sample)
                    shared.withLock { $0.stats.framesEnqueued += 1 }
                } else {
                    // Skipping a frame breaks the reference chain: resume at a keyframe.
                    shared.withLock { $0.stats.droppedDisplayBusy += 1 }
                    pipeline.withLock { $0?.closeGate() }
                }
            }
        }
    }

    private func flushDisplay() {
        videoRenderer.flush()
        shared.withLock { $0.stats.flushes += 1 }
    }
}
