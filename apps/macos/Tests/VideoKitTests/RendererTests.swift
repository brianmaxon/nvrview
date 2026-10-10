import Foundation
import Testing
@testable import VideoKit

@Suite struct QueueTests {
    private func frame(_ key: Bool, _ n: Int = 0) -> VideoFrame {
        VideoFrame(codec: .h264, isKeyframe: key, ptsMicros: Int64(n), annexB: Data())
    }

    @Test func overflowDropsToNextKeyframe() {
        var q = BoundedFrameQueue(capacity: 3)
        #expect(q.push(frame(true, 0)) == 0)
        #expect(q.push(frame(false, 1)) == 0)
        #expect(q.push(frame(false, 2)) == 0)
        // Full: the 4th (a P-frame) drops the 3 queued plus itself.
        #expect(q.push(frame(false, 3)) == 4)
        #expect(q.isEmpty)
        // Further P-frames are dropped one by one.
        #expect(q.push(frame(false, 4)) == 1)
        #expect(q.isEmpty)
        // A keyframe resumes.
        #expect(q.push(frame(true, 5)) == 0)
        #expect(q.push(frame(false, 6)) == 0)
        guard case .frame(let f)? = q.pop() else { Issue.record("expected frame"); return }
        #expect(f.ptsMicros == 5)
    }

    @Test func keyframeOnOverflowReplacesQueue() {
        var q = BoundedFrameQueue(capacity: 2)
        _ = q.push(frame(true, 0))
        _ = q.push(frame(false, 1))
        #expect(q.push(frame(true, 2)) == 2)
        guard case .frame(let f)? = q.pop() else { Issue.record("expected frame"); return }
        #expect(f.ptsMicros == 2)
        #expect(q.isEmpty)
    }

    @Test func controlItemsSurviveAndClearPending() {
        var q = BoundedFrameQueue(capacity: 4)
        _ = q.push(frame(true, 0))
        _ = q.push(frame(false, 1))
        #expect(q.pushControl(.flush) == 2)
        guard case .flush? = q.pop() else { Issue.record("expected flush"); return }
        #expect(q.isEmpty)
    }
}

@Suite(.serialized) struct RendererTests {
    private nonisolated func waitUntil(_ timeout: Double = 10, _ condition: @Sendable () -> Bool) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline { try? await Task.sleep(for: .milliseconds(20)) }
    }

    @MainActor
    private func feed(_ name: String, capacity: Int = 1000) async throws -> RendererStats {
        let clip = try Fixtures.load(name)
        let renderer = ChannelRenderer(label: "test", queueCapacity: capacity)
        let frames = clip.frames()
        // Frames arrive on a background thread, as they will from the core.
        let sender = Task.detached {
            for frame in frames {
                renderer.submit(frame)
                try? await Task.sleep(for: .milliseconds(5))
            }
        }
        await sender.value
        await waitUntil {
            let s = renderer.stats
            return s.framesEnqueued + s.droppedWaitingForKeyframe + s.droppedDisplayBusy >= frames.count
        }
        return renderer.stats
    }

    @Test @MainActor func leadingPFramesNeverReachTheDisplay() async throws {
        let s = try await feed("h264-leading-p.h264")
        #expect(s.framesReceived == 59)
        #expect(s.droppedWaitingForKeyframe == 19)
        #expect(s.droppedQueueOverflow == 0)
        #expect(s.framesEnqueued + s.droppedDisplayBusy == 40)
        #expect(s.decodeFailures == 0)
    }

    @Test @MainActor func resolutionChangeFlushesOnce() async throws {
        let s = try await feed("h265-resize.h265")
        #expect(s.framesReceived == 80)
        #expect(s.formatChanges == 1)
        #expect(s.decodeFailures == 0)
        #expect(s.droppedWaitingForKeyframe == 0)
    }

    @Test @MainActor func overflowIsCountedNotQueued() async throws {
        let clip = try Fixtures.load("h264-basic.h264")
        let renderer = ChannelRenderer(label: "burst", queueCapacity: 4)
        for frame in clip.frames() { renderer.submit(frame) }  // no pacing: a burst
        await waitUntil {
            let s = renderer.stats
            return s.framesEnqueued + s.droppedWaitingForKeyframe + s.droppedDisplayBusy + s.droppedQueueOverflow >= 60
        }
        let s = renderer.stats
        #expect(s.framesReceived == 60)
        #expect(s.droppedQueueOverflow > 0)
    }
}
