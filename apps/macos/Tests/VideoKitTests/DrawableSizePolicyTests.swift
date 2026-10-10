import CoreGraphics
import Foundation
import Testing
import VideoKit

@Suite struct DrawableSizePolicyTests {
    private func size(_ w: Int) -> CGSize { CGSize(width: w, height: w / 2) }

    @Test func aSingleResizeAppliesAtOnce() {
        var p = DrawableSizePolicy()
        #expect(p.want(size(1000), now: 0) == size(1000))
        #expect(p.want(size(1000), now: 0.01) == nil)
        #expect(p.want(size(1200), now: 5) == size(1200))
        #expect(p.tick(now: 5.1) == nil)
    }

    @Test func ignoresEmptySizes() {
        var p = DrawableSizePolicy()
        #expect(p.want(.zero, now: 0) == nil)
        #expect(p.applied == nil)
    }

    /// A 0.5 s animation at 120 Hz changes the size every refresh: the
    /// drawable follows at most every 100 ms, and exactly once it settles.
    @Test func anAnimationIsCoalesced() {
        var p = DrawableSizePolicy(minInterval: 0.1, settle: 0.05)
        var applied: [(TimeInterval, CGSize)] = []
        let frame = 1.0 / 120
        if let s = p.want(size(1000), now: -10) { applied.append((-10, s)) }
        var t = 0.0
        for step in 0...60 {
            t = Double(step) * frame
            if let s = p.want(size(1000 + step * 10), now: t) { applied.append((t, s)) }
            if let s = p.tick(now: t) { applied.append((t, s)) }
        }
        // The animation ends at 1600; refreshes continue without changes.
        for step in 1...20 {
            let now = t + Double(step) * frame
            if let s = p.tick(now: now) { applied.append((now, s)) }
        }
        #expect(applied.first?.1 == size(1000))
        #expect(applied.last?.1 == size(1600))
        #expect(applied.count <= 8, "\(applied.count) reallocations for 61 size changes")
        for (a, b) in zip(applied, applied.dropFirst()) {
            #expect(b.0 - a.0 >= 0.1 - 1e-9 || b.1 == size(1600))
        }
        // Settled within the settle time plus a refresh after the last change.
        let settledAt = applied.last!.0
        #expect(settledAt - t <= 0.05 + frame + 1e-9)
        #expect(p.pending == nil)
    }

    @Test func returningToTheAppliedSizeCancelsThePendingOne() {
        var p = DrawableSizePolicy()
        _ = p.want(size(1000), now: 0)
        #expect(p.want(size(1100), now: 0.01) == nil)
        #expect(p.want(size(1000), now: 0.02) == nil)
        #expect(p.pending == nil)
        #expect(p.tick(now: 1) == nil)
    }
}
