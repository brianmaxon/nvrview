import Foundation
import NVRCore
import Testing

/// The reveal state machine runs on manual time: no test waits.
@Suite struct SidebarRevealTests {
    @Test func restingAtTheEdgeRevealsAfterTheDwell() {
        var r = SidebarReveal()
        r.pointer(inEdge: true, inOverlay: false, now: 10)
        #expect(r.deadline == 10.15)
        let c1 = r.tick(now: 10.1)
        #expect(!c1)
        #expect(!r.isShown)
        let c2 = r.tick(now: 10.15)
        #expect(c2)
        #expect(r.isShown)
        #expect(r.deadline == nil)
    }

    @Test func leavingBeforeTheDwellCancels() {
        var r = SidebarReveal()
        r.pointer(inEdge: true, inOverlay: false, now: 0)
        r.pointer(inEdge: false, inOverlay: false, now: 0.1)
        #expect(r.deadline == nil)
        let c3 = r.tick(now: 5)
        #expect(!c3)
        #expect(!r.isShown)
        // Re-entering starts a fresh dwell, not the old one.
        r.pointer(inEdge: true, inOverlay: false, now: 6)
        let c4 = r.tick(now: 6.1)
        #expect(!c4)
        let c5 = r.tick(now: 6.15)
        #expect(c5)
    }

    @Test func hidesAfterThePointerLeavesTheOverlay() {
        var r = shown()
        r.pointer(inEdge: false, inOverlay: false, now: 20)
        #expect(r.deadline == 20.5)
        let c6 = r.tick(now: 20.4)
        #expect(!c6)
        #expect(r.isShown)
        let c7 = r.tick(now: 20.5)
        #expect(c7)
        #expect(!r.isShown)
        #expect(r.deadline == nil)
    }

    @Test func comingBackCancelsTheHide() {
        var r = shown()
        r.pointer(inEdge: false, inOverlay: false, now: 20)
        r.pointer(inEdge: false, inOverlay: true, now: 20.3)
        #expect(r.deadline == nil)
        let c8 = r.tick(now: 30)
        #expect(!c8)
        #expect(r.isShown)
    }

    @Test func aHoldKeepsItUpAndItGoesOnceReleased() {
        for hold in [SidebarReveal.Hold.drag, .menu, .editing] {
            var r = shown()
            r.pointer(inEdge: false, inOverlay: false, now: 20)
            let c9 = r.tick(now: 20.5, holds: [hold])
            #expect(!c9)
            #expect(r.isShown)
            // Looked at again later, still held, then released.
            #expect(r.deadline == 20.75)
            let c10 = r.tick(now: 21, holds: [hold])
            #expect(!c10)
            let c11 = r.tick(now: 21.25, holds: [])
            #expect(c11)
            #expect(!r.isShown)
        }
    }

    @Test func dockingHidesAtOnceAndDisablesTheReveal() {
        var r = shown()
        r.setEnabled(false)
        #expect(!r.isShown)
        #expect(r.deadline == nil)
        r.pointer(inEdge: true, inOverlay: false, now: 30)
        #expect(r.deadline == nil)
        let c12 = r.tick(now: 31)
        #expect(!c12)
        // Collapsed again: it works again.
        r.setEnabled(true)
        r.pointer(inEdge: true, inOverlay: false, now: 40)
        let c13 = r.tick(now: 40.15)
        #expect(c13)
    }

    @Test func aPendingRevealIsDroppedWhenDocked() {
        var r = SidebarReveal()
        r.pointer(inEdge: true, inOverlay: false, now: 0)
        r.setEnabled(false)
        let c14 = r.tick(now: 1)
        #expect(!c14)
        #expect(!r.isShown)
    }

    private func shown() -> SidebarReveal {
        var r = SidebarReveal()
        r.pointer(inEdge: true, inOverlay: false, now: 0)
        r.tick(now: 1)
        r.pointer(inEdge: false, inOverlay: true, now: 1)
        return r
    }
}
