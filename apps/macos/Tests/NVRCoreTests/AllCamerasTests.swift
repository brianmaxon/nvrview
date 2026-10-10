import FakeCore
import Foundation
import NVRCore
import Testing

/// The All Cameras selection and its refresh rule. Pure logic: no clocks.
@Suite struct AllCamerasTests {
    private func channels(_ online: [Int], offline: [Int] = [], lenses: [Int: Int] = [:]) -> [ChannelInfo] {
        (online.map { ($0, true) } + offline.map { ($0, false) })
            .sorted { $0.0 < $1.0 }
            .map { ChannelInfo(channel: $0.0, connected: $0.1, lenses: lenses[$0.0] ?? 1) }
    }

    private func device(_ key: String, connected: Bool = true, _ channels: [ChannelInfo]) -> AllCamerasDevice {
        AllCamerasDevice(key: key, connected: connected, channels: channels)
    }

    private func ref(_ d: String, _ c: Int, _ lens: Int = 0) -> CameraRef { CameraRef(device: d, channel: c, lens: lens) }

    @Test func devicesComeInOrderEachWithItsOnlineChannels() {
        let list = AllCameras.select([
            device("a", channels([0, 1])),
            device("b", channels([0, 2])),
        ])
        #expect(list == [ref("a", 0), ref("a", 1), ref("b", 0), ref("b", 2)])
    }

    @Test func aFullFirstDeviceDoesNotCrowdOutTheNext() {
        let list = AllCameras.select([
            device("a", channels(Array(0..<16))),
            device("b", channels([0, 1])),
        ])
        #expect(list.count == 18)
        #expect(list.suffix(2) == [ref("b", 0), ref("b", 1)])
    }

    @Test func offlineChannelsAreLeftOut() {
        let list = AllCameras.select([device("a", channels([0, 2], offline: [1, 3]))])
        #expect(list == [ref("a", 0), ref("a", 2)])
    }

    @Test func devicesThatAreNotConnectedAreLeftOutUntilTheyConnect() {
        // Channels not yet known (empty) and stale channels of a device that
        // dropped are both unknown, not online.
        let waiting = [device("a", channels([0])), device("b", connected: false, channels([0, 1])), device("c", [])]
        #expect(AllCameras.select(waiting) == [ref("a", 0)])
        let later = [device("a", channels([0])), device("b", channels([0, 1])), device("c", channels([0]))]
        #expect(AllCameras.select(later) == [ref("a", 0), ref("b", 0), ref("b", 1), ref("c", 0)])
    }

    @Test func everyLensOfAnOnlineChannelIsIncluded() {
        let list = AllCameras.select([device("a", channels([0, 1], lenses: [1: 2]))])
        #expect(list == [ref("a", 0), ref("a", 1), ref("a", 1, 1)])
        #expect(AllCameras.select([device("a", channels([], offline: [1], lenses: [1: 2]))]).isEmpty)
    }

    @Test func theListStopsAtTheLayoutLimit() {
        #expect(LayoutNode.maxLeaves == 64)
        let devices = (0..<5).map { device("d\($0)", channels(Array(0..<16))) }
        let list = AllCameras.select(devices)
        #expect(list.count == 64)
        #expect(list.last == ref("d3", 15))
        // A lens pair straddling the limit is cut, not skipped past.
        let lensy = AllCameras.select([device("a", channels(Array(0..<64), lenses: [63: 2]))])
        #expect(lensy.count == 64)
        #expect(AllCameras.select(devices, limit: 3).count == 3)
        #expect(LayoutNode.balanced(list).leafCount == 64)
    }

    @Test func nothingOnlineGivesAnEmptyList() {
        #expect(AllCameras.select([]).isEmpty)
        #expect(AllCameras.select([device("a", connected: false, channels([0]))]).isEmpty)
    }

    // MARK: Refresh

    private func offer(_ watch: inout AllCamerasWatch, _ cameras: [CameraRef]) -> Bool { watch.refresh(to: cameras) }

    @Test func anUneditedWindowRebuildsOnlyWhenTheListChanges() {
        var watch = AllCamerasWatch(shown: [])
        #expect(!offer(&watch, []))
        let one = [ref("a", 0)]
        #expect(offer(&watch, one))
        #expect(!offer(&watch, one))
        #expect(!offer(&watch, one))
        let two = [ref("a", 0), ref("b", 0)]
        #expect(offer(&watch, two))
        #expect(!offer(&watch, two))
        // A channel going offline, then everything going away.
        #expect(offer(&watch, [ref("b", 0)]))
        #expect(offer(&watch, []))
    }

    @Test func anEditedWindowIsNeverRefreshed() {
        var watch = AllCamerasWatch(shown: [ref("a", 0)])
        watch.markEdited()
        #expect(!offer(&watch, [ref("a", 0), ref("b", 0)]))
        #expect(!offer(&watch, []))
        #expect(watch.shown == [ref("a", 0)])
    }

    @Test func rebuildingWithTheSameCamerasKeepsTheirStreams() throws {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        let rig = try Rig(o)
        defer { rig.bridge.shutdown() }
        let leases = TileLeases(bridge: rig.bridge)
        let device = rig.device
        leases.resolve = { $0 == "dev-a" ? device : nil }
        let a = ref("dev-a", 0)
        let b = ref("dev-a", 1)

        func counts(_ list: [CameraRef]) -> [CameraRef: Int] {
            Dictionary(list.map { ($0, 1) }, uniquingKeysWith: +)
        }
        var held: [CameraRef: Int] = [:]
        var wanted = counts([a])
        leases.update(from: held, to: wanted)
        held = wanted
        let tileA = try #require(leases.tile(for: a))
        // A second camera joins: the first keeps its tile.
        wanted = counts([a, b])
        leases.update(from: held, to: wanted)
        held = wanted
        #expect(leases.tile(for: a) === tileA)
        #expect(rig.bridge.tiles.count == 2)
        // The first goes away and the second stays.
        let tileB = try #require(leases.tile(for: b))
        wanted = counts([b])
        leases.update(from: held, to: wanted)
        #expect(leases.tile(for: b) === tileB)
        #expect(leases.tile(for: a) == nil)
        #expect(rig.fake.violations.isEmpty)
    }
}
