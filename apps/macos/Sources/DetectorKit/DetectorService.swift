import Foundation

public enum DetectorServiceError: Error, Equatable {
    /// The job waited in a full queue and a newer one took its place.
    case dropped
}

/// The detector's inference queue: one job runs at a time on a background
/// queue (never the main thread), at most `capacity` wait, and when the queue
/// is full the oldest waiting job is dropped, not the newest (pictures age,
/// and the lossy rule of ARCHITECTURE.md's extension points is "drop rather
/// than queue when behind"). A job whose task is canceled stops waiting at
/// once; a job already inferring cannot be interrupted, its result is
/// discarded and the next job starts when it ends.
public actor DetectorService {
    private final class Job: @unchecked Sendable {
        let id: Int
        let picture: DetectorPicture
        let crops: [NormRect]
        var continuation: CheckedContinuation<[CropDetections], Error>?

        init(id: Int, picture: DetectorPicture, crops: [NormRect], continuation: CheckedContinuation<[CropDetections], Error>) {
            self.id = id
            self.picture = picture
            self.crops = crops
            self.continuation = continuation
        }
    }

    private let detector: any CropDetecting
    private let capacity: Int
    private let queue = DispatchQueue(label: "nvrview.detector", qos: .utility)
    private var waiting: [Job] = []
    private var running: Job?
    private var nextID = 0
    /// Jobs dropped because the queue was full, since creation.
    public private(set) var droppedCount = 0

    public init(detector: any CropDetecting, capacity: Int = 2) {
        precondition(capacity > 0)
        self.detector = detector
        self.capacity = capacity
    }

    /// Jobs waiting (not counting the one inferring).
    public var waitingCount: Int { waiting.count }
    public var isRunning: Bool { running != nil }

    /// Detects `crops` of `picture`. Throws `DetectorServiceError.dropped` when
    /// a newer job displaced this one, `CancellationError` when the calling
    /// task is canceled, or the detector's own error.
    public func detect(picture: DetectorPicture, crops: [NormRect]) async throws -> [CropDetections] {
        try Task.checkCancellation()
        let id = nextID
        nextID += 1
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                if waiting.count >= capacity {
                    let oldest = waiting.removeFirst()
                    droppedCount += 1
                    oldest.continuation?.resume(throwing: DetectorServiceError.dropped)
                    oldest.continuation = nil
                }
                waiting.append(Job(id: id, picture: picture, crops: crops, continuation: continuation))
                startNext()
            }
        } onCancel: {
            // The actor is busy running the body above until it suspends, so
            // this always lands after the job is queued.
            Task { await self.cancel(id) }
        }
    }

    /// Cancels every waiting job (the running one finishes unobserved).
    public func cancelAll() {
        for job in waiting { job.continuation?.resume(throwing: CancellationError()) }
        waiting.removeAll()
        running?.continuation?.resume(throwing: CancellationError())
        running?.continuation = nil
    }

    private func cancel(_ id: Int) {
        if let i = waiting.firstIndex(where: { $0.id == id }) {
            let job = waiting.remove(at: i)
            job.continuation?.resume(throwing: CancellationError())
            job.continuation = nil
        } else if let job = running, job.id == id {
            job.continuation?.resume(throwing: CancellationError())
            job.continuation = nil
        }
    }

    private func startNext() {
        guard running == nil, !waiting.isEmpty else { return }
        let job = waiting.removeFirst()
        running = job
        let detector = detector
        queue.async {
            let result = Result { try detector.detect(picture: job.picture, crops: job.crops) }
            Task { await self.finish(job, result) }
        }
    }

    private func finish(_ job: Job, _ result: Result<[CropDetections], Error>) {
        running = nil
        job.continuation?.resume(with: result)
        job.continuation = nil
        startNext()
    }
}
