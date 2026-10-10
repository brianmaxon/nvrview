import Foundation

/// Pictures per second over a sliding window of timestamps (microseconds),
/// in a fixed ring: adding a sample never allocates.
///
/// Used twice per tile: with the decoded pictures' camera timestamps (the
/// rate the device sends, which arrival jitter does not distort) and with
/// host times of new pictures presented (the rate actually shown). A
/// timestamp that goes backwards or jumps forward by more than
/// `maxGapMicros` (a wrap, a stream restart, a long stall) starts the
/// window over, so a discontinuity never shows as an absurd rate; one that
/// steps back by less than that (a picture out of order) is ignored.
public struct RateMeter: Sendable {
    public let windowMicros: Int64
    public let maxGapMicros: Int64
    private var ring: [Int64]
    private var start = 0
    private var count = 0

    /// `capacity` bounds the samples kept: enough for the highest rate
    /// expected over the window (240 covers 120 fps over 2 s).
    public init(windowMicros: Int64 = 2_000_000, maxGapMicros: Int64 = 5_000_000, capacity: Int = 240) {
        self.windowMicros = windowMicros
        self.maxGapMicros = maxGapMicros
        ring = Array(repeating: 0, count: max(capacity, 2))
    }

    private var last: Int64? { count == 0 ? nil : ring[(start + count - 1) % ring.count] }
    private var first: Int64? { count == 0 ? nil : ring[start] }

    /// Adds one picture's timestamp.
    public mutating func add(_ micros: Int64) {
        if let last {
            if micros == last { return }
            if micros < last {
                if last - micros <= maxGapMicros { return }
                reset()
            } else if micros - last > maxGapMicros {
                reset()
            }
        }
        if count == ring.count {
            start = (start + 1) % ring.count
            count -= 1
        }
        ring[(start + count) % ring.count] = micros
        count += 1
        // Drop samples older than the window, measured from the newest.
        while count > 2, let first, micros - first > windowMicros {
            start = (start + 1) % ring.count
            count -= 1
        }
    }

    public mutating func reset() {
        start = 0
        count = 0
    }

    /// Pictures per second over the window; nil with fewer than two samples.
    /// With `now` (in the same clock as the samples), also nil once the
    /// newest sample is older than the window: the stream has stopped.
    public func rate(now: Int64? = nil) -> Double? {
        guard count >= 2, let first, let last, last > first else { return nil }
        if let now, now - last > windowMicros { return nil }
        return Double(count - 1) * 1_000_000 / Double(last - first)
    }
}
