import FakeCore
import Foundation
import NVRCore
import Testing

/// Per-tile stream quality (docs/UX.md "Stream quality per tile"): the
/// mapping to tiers per camera kind, the highest tier across tiles, Auto's
/// zoom hysteresis, and the choice in the layout JSON.
@Suite struct TileQualityTests {
    private let single = CameraRef(device: "dev-a", channel: 1)
    private let plain = CameraRef(device: "dev-a", channel: 2)
    private let lens0 = CameraRef(device: "dev-a", channel: 3)
    private let lens1 = CameraRef(device: "dev-a", channel: 3, lens: 1)

    private func rig() throws -> (Rig, TileLeases) {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        let rig = try Rig(o)
        let leases = TileLeases(bridge: rig.bridge)
        let device = rig.device
        leases.resolve = { $0 == "dev-a" ? device : nil }
        // Channel 1 lists extern (single lens); channel 3 is multi-lens.
        let single = single
        leases.hasMiddleTier = { $0 == single || $0.channel == 3 }
        return (rig, leases)
    }

    private func demand(
        _ l: TileLeases, _ owner: String, _ camera: CameraRef, _ q: TileQuality, focused: Bool = false,
        zoom: Double = 1
    ) {
        l.setDemand(owner: owner, camera: camera, quality: q, focused: focused, zoomScale: zoom)
    }

    // MARK: Mapping

    @Test func aSingleLensCameraMapsQualitiesToItsTiers() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try #require(l.acquire(single))
        #expect(tile.openedTier == .sub)
        #expect(l.offersBalanced(single))
        let cases: [(TileQuality, Bool, StreamTier)] = [
            (.auto, false, .sub), (.auto, true, .main), (.clear, false, .main), (.clear, true, .main),
            (.fluent, false, .sub), (.fluent, true, .sub), (.balanced, false, .extern), (.balanced, true, .extern),
        ]
        for (q, focused, tier) in cases {
            demand(l, "w1", single, q, focused: focused)
            #expect(l.wantedTier(single) == tier, "\(q) focused \(focused)")
        }
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func balancedNeedsAnExternTierAndActsAsAutoWithout() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        l.acquire(plain)
        #expect(!l.offersBalanced(plain))
        #expect(l.effectiveQuality(.balanced, plain) == .auto)
        demand(l, "w1", plain, .balanced)
        #expect(l.wantedTier(plain) == .sub)
        demand(l, "w1", plain, .balanced, focused: true)
        #expect(l.wantedTier(plain) == .main)
        // Balanced is never offered for a second lens.
        #expect(!l.offersBalanced(lens1))
    }

    @Test func theSecondLensMapsToMobileAndExternWithFallback() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        l.acquire(lens1)
        for (q, focused, tier) in [
            (TileQuality.auto, false, StreamTier.mobile), (.auto, true, .extern), (.clear, false, .extern),
            (.fluent, false, .mobile), (.fluent, true, .mobile), (.balanced, false, .mobile),
        ] {
            demand(l, "w1", lens1, q, focused: focused)
            #expect(l.wantedTier(lens1) == tier, "\(q) focused \(focused)")
        }
    }

    @Test func withoutMobileTheSecondLensUsesExternForEveryQuality() throws {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        o.startRejectedTiers = [.mobile]
        let rig = try Rig(o)
        defer { rig.bridge.shutdown() }
        let l = TileLeases(bridge: rig.bridge)
        let device = rig.device
        l.resolve = { $0 == "dev-a" ? device : nil }
        let first = try #require(l.acquire(lens1))
        l.startFailed(tile: first.id, reason: "start_rejected")
        for q in [TileQuality.auto, .clear, .fluent] {
            demand(l, "w1", lens1, q)
            #expect(l.wantedTier(lens1) == .extern, "\(q)")
        }
    }

    // MARK: Sharing

    @Test func theHighestTierAskedForWinsAcrossTilesAndWindows() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try #require(l.acquire(single))
        l.acquire(single)
        l.acquire(single)
        demand(l, "w1", single, .fluent)
        demand(l, "w2", single, .balanced)
        #expect(tile.tierState.requested == .extern)
        demand(l, "w3", single, .clear)
        #expect(tile.tierState.requested == .main)
        // Fluent on a focused tile stays low, but another tile's Auto focus
        // (or Clear) still raises it for everyone.
        demand(l, "w3", single, .auto)
        #expect(tile.tierState.requested == .extern)
        demand(l, "w1", single, .fluent, focused: true)
        #expect(tile.tierState.requested == .extern)
        demand(l, "w2", single, .auto, focused: true)
        #expect(tile.tierState.requested == .main)
        l.clearDemands(owner: "w2")
        #expect(tile.tierState.requested == .sub)
        // Plain focus counts as high unless a tile chose Fluent.
        l.setFocused(single, true)
        #expect(tile.tierState.requested == .main)
        l.setFocused(single, false)
        #expect(tile.tierState.requested == .sub)
        // Releasing every lease forgets the demands.
        l.release(single)
        l.release(single)
        l.release(single)
        #expect(l.demand(owner: "w1") == nil)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func changingTheTierIsAnAlignedSwitchNotARestart() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try #require(l.acquire(single))
        demand(l, "w1", single, .clear)
        #expect(tile.tierState.requested == .main)
        #expect(rig.fake.generations(tile: tile.id)?.running == [0, 1])
        // Asking again changes nothing.
        demand(l, "w1", single, .clear)
        #expect(rig.fake.generations(tile: tile.id)?.running == [0, 1])
        demand(l, "w1", single, .auto)
        #expect(tile.tierState.requested == .sub)
        #expect(rig.bridge.tiles.count == 1)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func aTileOpensOnTheTierItsDemandAsksFor() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        demand(l, "w1", single, .clear)
        let tile = try #require(l.acquire(single))
        #expect(tile.openedTier == .main)
    }

    // MARK: Zoom

    @Test func hysteresisKeepsAutoFromFlapping() {
        var high = false
        var trace: [Bool] = []
        for scale in [1.0, 1.9, 2.0, 1.9, 1.7, 1.6, 1.59, 1.9, 2.1, 1.0] {
            high = ZoomHysteresis.next(high: high, scale: scale)
            trace.append(high)
        }
        #expect(trace == [false, false, true, true, true, true, false, false, true, false])
    }

    @Test func zoomDrivesAutoThroughTheLeases() throws {
        let (rig, l) = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try #require(l.acquire(single))
        for (scale, tier) in [(1.5, StreamTier.sub), (2.0, .main), (1.7, .main), (1.5, .sub), (1.9, .sub), (2.4, .main)] {
            demand(l, "w1", single, .auto, zoom: scale)
            #expect(tile.tierState.requested == tier, "zoom \(scale)")
        }
        // Fluent ignores zoom; the zoom state is the tile's own.
        demand(l, "w1", single, .fluent, zoom: 4)
        #expect(tile.tierState.requested == .sub)
        #expect(rig.fake.violations.isEmpty)
    }

    // MARK: Persistence

    @Test func theChoiceIsSavedWithTheLayoutAndKeepsUnknownMembers() throws {
        let json = #"""
            {"id":"q1","name":"Q","version":1,"root":{"split":"row","ratio":0.5,
             "a":{"camera":{"device":"dev-a","channel":1},"label":"A","quality":"clear"},
             "b":{"camera":{"device":"dev-a","channel":2},"quality":"ultra"}}}
            """#
        let layout = try #require(SavedLayout.list(fromJSON: "[\(json)]").first)
        #expect(layout.root[[.a]]?.quality == .clear)
        // A value from a newer app reads as Auto and is kept as it was.
        #expect(layout.root[[.b]]?.quality == .auto)
        let edited = try #require(layout.root.settingQuality(.fluent, at: [.a]))
        #expect(edited[[.a]]?.quality == .fluent)
        #expect(edited[[.a]]?.extras.node["label"] == .string("A"))
        #expect(edited[[.b]]?.extras.node["quality"] == .string("ultra"))
        // Auto is stored as no member.
        let auto = try #require(edited.settingQuality(.auto, at: [.a]))
        #expect(auto[[.a]]?.extras.node["quality"] == nil)
        #expect(auto[[.a]]?.extras.node["label"] == .string("A"))
        #expect(edited.settingQuality(.clear, at: []) == nil)
    }

    @Test func theChoiceRoundTripsThroughTheCoreStoreAndFollowsItsTile() throws {
        let (rig, _) = try rig()
        defer { rig.bridge.shutdown() }
        var root = LayoutNode.split(.row, ratio: 0.5, a: .leaf(single), b: .leaf(plain))
        root = try #require(root.settingQuality(.clear, at: [.a]))
        try rig.bridge.putLayout(json: SavedLayout(id: "q2", name: "Q2", root: root).json)
        let back = try #require(SavedLayout.list(fromJSON: try rig.bridge.layoutsJSON()).first)
        #expect(back.root == root)
        #expect(back.root[[.a]]?.quality == .clear)
        #expect(back.root[[.b]]?.quality == .auto)
        // The quality belongs to the tile: another camera in it, or a swap,
        // keeps each tile's own choice.
        let assigned = try #require(back.root.assigning(plain, at: [.a]))
        #expect(assigned[[.a]]?.quality == .clear)
        let swapped = try #require(back.root.swapping([.a], [.b]))
        #expect(swapped[[.a]]?.quality == .clear)
        #expect(swapped[[.b]]?.quality == .auto)
        #expect(swapped[[.a]]?.camera == plain)
        // Splitting keeps the old tile's choice in `a`; the new tile is Auto.
        let split = try #require(back.root.splitting(at: [.a], axis: .col)?.tree)
        #expect(split[[.a, .a]]?.quality == .clear)
        #expect(split[[.a, .b]]?.quality == .auto)
    }
}
