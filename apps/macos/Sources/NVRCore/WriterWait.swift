import AVFoundation
import Foundation

/// Bounded waits on an `AVAssetWriter`. The writer asks for samples through
/// `requestMediaDataWhenReady`; once it has failed or been canceled it never
/// asks again, so a plain "wait until every input finished" can wait forever
/// (a hung Mac test run was this). These waits also end when the writer stops
/// writing, and at a ceiling that only guards against a hang.
public enum WriterWait {
    public enum Outcome: Sendable { case drained, writerStopped, timedOut }

    /// How often the writer's state is looked at while waiting. It is a state
    /// check for the early exit, not a guess at when work is done.
    static let watchInterval = 0.5

    /// Resumes a continuation once, from whichever event comes first, then
    /// runs the cleanup.
    private final class Once<T: Sendable>: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<T, Never>?
        private let cleanup: @Sendable () -> Void
        init(_ c: CheckedContinuation<T, Never>, cleanup: @escaping @Sendable () -> Void = {}) {
            continuation = c
            self.cleanup = cleanup
        }
        func resume(_ value: T) {
            let c = lock.withLock { () -> CheckedContinuation<T, Never>? in
                defer { continuation = nil }
                return continuation
            }
            guard let c else { return }
            cleanup()
            c.resume(returning: value)
        }
    }

    /// Waits until `group` is empty, the writer is no longer writing, or
    /// `ceiling` seconds pass.
    public static func drain(_ group: DispatchGroup, writer: AVAssetWriter, ceiling: TimeInterval) async -> Outcome {
        await withCheckedContinuation { (c: CheckedContinuation<Outcome, Never>) in
            let timer = DispatchSource.makeTimerSource(queue: .global())
            let once = Once(c) { timer.cancel() }
            let deadline = Date().addingTimeInterval(ceiling)
            timer.schedule(deadline: .now() + watchInterval, repeating: watchInterval)
            timer.setEventHandler {
                if writer.status == .failed || writer.status == .cancelled {
                    once.resume(.writerStopped)
                } else if Date() >= deadline {
                    once.resume(.timedOut)
                }
            }
            timer.resume()
            group.notify(queue: .global()) { once.resume(.drained) }
        }
    }

    /// `finishWriting`, bounded: false when it did not return within `ceiling`
    /// seconds (the writer is then canceled).
    public static func finish(_ writer: AVAssetWriter, ceiling: TimeInterval) async -> Bool {
        await withCheckedContinuation { (c: CheckedContinuation<Bool, Never>) in
            let once = Once(c)
            writer.finishWriting { once.resume(true) }
            DispatchQueue.global().asyncAfter(deadline: .now() + ceiling) {
                if writer.status == .writing {
                    writer.cancelWriting()
                    once.resume(false)
                }
            }
        }
    }

    /// Hang guards: a test clip or an export takes seconds to minutes.
    public static let shortCeiling: TimeInterval = 120
    public static let exportCeiling: TimeInterval = 4 * 3600
}
