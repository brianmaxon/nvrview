import Foundation

/// Work items handled in order by a renderer's serial queue.
enum QueueItem: Sendable {
    case frame(VideoFrame)
    /// Drop all decoder state and wait for a keyframe.
    case flush
    /// The stream format changed: forget parameter sets, optionally take new
    /// ones (NAL units without start codes), and wait for a keyframe.
    case formatChanged(codec: VideoCodec?, parameterSets: [Data])
}

/// A bounded queue of frames that never grows past `capacity`. When a frame
/// arrives and the queue is full, every queued frame is dropped and incoming
/// frames are dropped until the next keyframe, so the consumer resumes at a
/// clean random-access point rather than decoding a stream with holes.
struct BoundedFrameQueue: Sendable {
    let capacity: Int
    private(set) var items: [QueueItem] = []
    private var frameCount = 0
    private(set) var droppingUntilKeyframe = false

    init(capacity: Int) {
        precondition(capacity > 0)
        self.capacity = capacity
    }

    var isEmpty: Bool { items.isEmpty }

    /// Returns the number of frames dropped by this call.
    mutating func push(_ frame: VideoFrame) -> Int {
        if droppingUntilKeyframe {
            if !frame.isKeyframe { return 1 }
            droppingUntilKeyframe = false
        }
        var dropped = 0
        if frameCount >= capacity {
            dropped = dropQueuedFrames()
            if !frame.isKeyframe {
                droppingUntilKeyframe = true
                return dropped + 1
            }
        }
        items.append(.frame(frame))
        frameCount += 1
        return dropped
    }

    /// Control items are never dropped. Pending frames are discarded, since
    /// they predate the control event. Returns the number of frames discarded.
    mutating func pushControl(_ item: QueueItem) -> Int {
        let dropped = dropQueuedFrames()
        droppingUntilKeyframe = false
        items.append(item)
        return dropped
    }

    mutating func pop() -> QueueItem? {
        guard !items.isEmpty else { return nil }
        let item = items.removeFirst()
        if case .frame = item { frameCount -= 1 }
        return item
    }

    private mutating func dropQueuedFrames() -> Int {
        let dropped = frameCount
        items.removeAll { if case .frame = $0 { true } else { false } }
        frameCount = 0
        return dropped
    }
}
