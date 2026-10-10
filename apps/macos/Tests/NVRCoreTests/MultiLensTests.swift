import FakeCore
import Foundation
import NVRCore
import Testing

/// The second lens of a multi-lens camera as its own camera: in layouts,
/// leases, tiers, and detection routing.
@Suite struct MultiLensTests {
    private let lens0 = CameraRef(device: "dev-a", channel: 3)
    private let lens1 = CameraRef(device: "dev-a", channel: 3, lens: 1)

    // MARK: Layouts

    @Test func aLeafStoresTheLensOnlyWhenItIsTheSecond() throws {
        let tree = LayoutNode.split(.row, ratio: 0.5, a: .leaf(lens0), b: .leaf(lens1))
        let json = SavedLayout(id: "l1", name: "Lenses", root: tree).json
        #expect(json.contains(#""b":{"camera":{"channel":3,"device":"dev-a","lens":1}}"#))
        #expect(json.contains(#""a":{"camera":{"channel":3,"device":"dev-a"}}"#))
        let back = try #require(SavedLayout.list(fromJSON: "[\(json)]").first)
        #expect(back.root == tree)
        #expect(back.root.cameras == [lens0, lens1])
    }

    @Test func oldLayoutsReadAsTheFirstLensAndBadLensesAreRefused() {
        let old = #"[{"id":"o","name":"Old","root":{"camera":{"device":"dev-a","channel":3}}}]"#
        #expect(SavedLayout.list(fromJSON: old).first?.root == .leaf(lens0))
        for bad in ["2", "-1", "1.5", "true", "\"1\""] {
            let json = #"[{"id":"b","name":"Bad","root":{"camera":{"device":"dev-a","channel":3,"lens":\#(bad)}}}]"#
            #expect(SavedLayout.list(fromJSON: json).isEmpty, "lens \(bad)")
        }
    }

    @Test func theFakeStoreValidatesTheLensLikeTheCore() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        func layout(_ lens: String?) -> String {
            let field = lens.map { #","lens":\#($0)"# } ?? ""
            return #"{"id":"l","name":"L","root":{"camera":{"device":"dev-a","channel":3\#(field)}}}"#
        }
        try rig.bridge.putLayout(json: layout(nil))
        try rig.bridge.putLayout(json: layout("0"))
        try rig.bridge.putLayout(json: layout("1"))
        #expect(SavedLayout.list(fromJSON: try rig.bridge.layoutsJSON()).first?.root == .leaf(lens1))
        for bad in ["2", "-1", "1.5", "true", "null", "\"1\""] {
            #expect(throws: CoreStatus.invalidArgument, "lens \(bad)") { try rig.bridge.putLayout(json: layout(bad)) }
        }
    }

    @Test func groupsAndDragAndRekeyingKeepTheLens() throws {
        let data = try JSONEncoder().encode([lens0, lens1])
        #expect(try JSONDecoder().decode([CameraRef].self, from: data) == [lens0, lens1])
        #expect(lens1.rekeyed(to: "dev-z") == CameraRef(device: "dev-z", channel: 3, lens: 1))
        #expect(LayoutNode.leaf(lens1).rekeying(device: "dev-a", to: "dev-z").camera?.lens == 1)
        #expect(lens1.firstLens == lens0)
    }

    // MARK: Channels and detections

    @Test func channelsSayHowManyLenses() {
        let e = CoreEvent.parse(Data(
            #"{"type":"channels","device":1,"channels":[{"channel":0,"tiers":["main","sub","extern"]},{"channel":3,"tiers":["main","sub"],"lenses":2}]}"#.utf8))
        guard case .channels(_, let list)? = e else { Issue.record("not channels"); return }
        // A third tier alone does not make a second lens.
        #expect(list[0].lenses == 1)
        #expect(!list[0].isMultiLens)
        #expect(list[1].isMultiLens)
    }

    @Test func detectionsGoToTheirLens() {
        let first = DetectionEvent(device: 1, channel: 3, kind: "people", active: true)
        let second = DetectionEvent(device: 1, channel: 3, kind: "people", active: true, lens: 1)
        #expect(first.camera(deviceKey: "dev-a") == lens0)
        #expect(second.camera(deviceKey: "dev-a") == lens1)
        var tracker = DetectionTracker()
        tracker.apply(second, camera: second.camera(deviceKey: "dev-a"))
        #expect(tracker.active(for: lens1).map(\.kind) == ["people"])
        #expect(tracker.active(for: lens0).isEmpty)
    }

    // MARK: Leases and tiers

    private func rig(_ configure: (inout FakeCore.Options) -> Void = { _ in }) throws -> (Rig, TileLeases) {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        configure(&o)
        let rig = try Rig(o)
        let leases = TileLeases(bridge: rig.bridge)
        let device = rig.device
        leases.resolve = { $0 == "dev-a" ? device : nil }
        return (rig, leases)
    }

    private func settle(_ rig: Rig, _ tile: CoreTile, until done: () -> Bool) {
        var steps = 0
        while !done(), steps < 400 {
            rig.fake.pump(tile: tile.id, count: 1)
            tile.waitForPendingFrames()
            steps += 1
        }
    }

    @Test func eachLensHasItsOwnTile() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        let a = try #require(leases.acquire(lens0))
        let b = try #require(leases.acquire(lens1))
        #expect(a !== b)
        #expect(a.channel == 3 && b.channel == 3)
        #expect(rig.bridge.tiles.count == 2)
        #expect(leases.tile(for: lens1) === b)
        leases.release(lens0)
        #expect(leases.tile(for: lens1) === b)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func theSecondLensUsesMobileInTheGridAndExternOnFocus() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        #expect(leases.tiers(for: lens0) == (.sub, .main))
        #expect(leases.tiers(for: lens1) == (.mobile, .extern))
        let tile = try #require(leases.acquire(lens1))
        // Opens on extern, then asks for mobile for the grid.
        #expect(tile.openedTier == .extern)
        #expect(tile.tierState.requested == .mobile)
        settle(rig, tile) { tile.tierState.shown == .mobile }
        #expect(tile.tierState.shown == .mobile)
        leases.setFocused(lens1, true)
        #expect(tile.tierState.requested == .extern)
        settle(rig, tile) { tile.tierState.shown == .extern }
        #expect(tile.tierState.shown == .extern)
        leases.setFocused(lens1, false)
        #expect(tile.tierState.requested == .mobile)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func withoutMobileTheSecondLensStaysOnExtern() throws {
        let (rig, leases) = try rig { $0.tierSwitchFails = true }
        defer { rig.bridge.shutdown() }
        let tile = try #require(leases.acquire(lens1))
        #expect(tile.tierState.requested == .mobile)
        // The fake fails the switch (start_rejected) at its next turn; the
        // app forwards the event to the leases.
        rig.fake.pump(tile: tile.id, count: 1)
        tile.waitForPendingFrames()
        let failed = rig.events.get().contains {
            if case .tileTier(tile.id, .mobile, .failed, _, _) = $0 { return true }
            return false
        }
        #expect(failed)
        leases.tierFailed(tile: tile.id, tier: .mobile, reason: "start_rejected")
        #expect(leases.tiers(for: lens1) == (.extern, .extern))
        #expect(tile.tierState.shown == .extern)
        // Focus no longer switches anything; the first lens is unaffected.
        leases.setFocused(lens1, true)
        #expect(tile.tierState.shown == .extern)
        #expect(leases.tiers(for: lens0) == (.sub, .main))
        // A superseded switch is not a failure of mobile.
        let (rig2, leases2) = try self.rig()
        defer { rig2.bridge.shutdown() }
        let t2 = try #require(leases2.acquire(lens1))
        leases2.tierFailed(tile: t2.id, tier: .mobile, reason: "superseded")
        #expect(leases2.tiers(for: lens1) == (.mobile, .extern))
    }
}
