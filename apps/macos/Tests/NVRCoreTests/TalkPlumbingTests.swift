import FakeCore
import Foundation
import NVRCore
import Testing

/// Talk-back through the bridge (docs/FFI.md "Talk-back"): the `talk` event
/// parses, and the fake core models the permission, the start, pushed audio
/// and every stop as the contract says.
@Suite struct TalkPlumbingTests {
    @Test func talkEventsParse() {
        let refused = CoreEvent.parse(
            Data(#"{"type":"talk","tile":7,"device":1,"channel":3,"state":"refused","reason":"rate_limited","retry_ms":41000}"#.utf8))
        #expect(
            refused
                == .talk(
                    TalkEvent(
                        tile: 7, device: 1, channel: 3, state: .refused, reason: "rate_limited", retryMilliseconds: 41000)))
        let stopped = CoreEvent.parse(
            Data(#"{"type":"talk","tile":7,"device":1,"channel":3,"state":"stopped","reason":"stopped","sent_ms":4096,"dropped_ms":0,"new":1}"#.utf8))
        #expect(
            stopped
                == .talk(
                    TalkEvent(
                        tile: 7, device: 1, channel: 3, state: .stopped, reason: "stopped", sentMilliseconds: 4096,
                        droppedMilliseconds: 0)))
        #expect(CoreEvent.parse(Data(#"{"type":"talk","state":"active"}"#.utf8)) == nil)
    }

    private func rig() throws -> Rig {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.devices["192.0.2.1"] = FakeDeviceSpec(channelCount: 4, talkChannels: [2])
        return try Rig(o)
    }

    private func talkStates(_ rig: Rig) -> [TalkEvent] {
        rig.events.get().compactMap { if case .talk(let t) = $0 { t } else { nil } }
    }

    @Test func withoutThePermissionNothingStarts() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 2)
        #expect(throws: CoreStatus.notPermitted) { try rig.bridge.startTalk(tile) }
        #expect(talkStates(rig).map(\.state) == [.refused])
        #expect(talkStates(rig).first?.reason == "not_permitted")
        #expect(rig.fake.talkStarts == 0)
    }

    @Test func aPermittedTalkStartsCarriesAudioAndStops() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 2)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: true)
        try rig.bridge.startTalk(tile)
        // Starting again while it runs changes nothing.
        try rig.bridge.startTalk(tile)
        try rig.bridge.pushTalk(tile, samples: [Float](repeating: 0.1, count: 48_000), sampleRate: 48_000)
        #expect(abs(rig.fake.talkPushedMilliseconds(tile: tile.id) - 1000) < 0.01)
        #expect(throws: CoreStatus.invalidArgument) {
            try rig.bridge.pushTalk(tile, samples: [0.1], sampleRate: 4_000)
        }
        try rig.bridge.stopTalk(tile, drain: true)
        let states = talkStates(rig)
        #expect(states.map(\.state) == [.starting, .active, .stopped])
        #expect(states.last?.reason == "finished")
        #expect(states.last?.sentMilliseconds == 1000)
        #expect(rig.fake.talkStarts == 1)
    }

    @Test func withdrawingThePermissionStopsTheTalk() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 2)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: true)
        try rig.bridge.startTalk(tile)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: false)
        #expect(talkStates(rig).last?.state == .stopped)
        #expect(talkStates(rig).last?.reason == "permission_revoked")
        #expect(!rig.fake.isTalking(tile: tile.id))
    }

    @Test func aCameraWithoutTalkBackRefuses() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 1)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 1, allowed: true)
        try rig.bridge.startTalk(tile)
        #expect(talkStates(rig).map(\.reason) == ["not_supported"])
    }

    @Test func conversationMarksFollowTheCallAndEndWithTheTileOrPermission() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 2)
        try rig.bridge.setTalkConversation(tile, open: true)
        #expect(rig.fake.inConversation(tile: tile.id))
        try rig.bridge.setTalkConversation(tile, open: false)
        #expect(!rig.fake.inConversation(tile: tile.id))
        // Withdrawing the permission clears the mark.
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: true)
        try rig.bridge.setTalkConversation(tile, open: true)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: false)
        #expect(!rig.fake.inConversation(tile: tile.id))
        // So does closing the tile; then the tile is unknown.
        try rig.bridge.setTalkConversation(tile, open: true)
        rig.bridge.closeTile(tile)
        #expect(!rig.fake.inConversation(tile: tile.id))
        #expect(throws: CoreStatus.unknownID) { try rig.bridge.setTalkConversation(tile, open: true) }
    }

    @Test func closingTheTileStopsItsTalk() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.open(channel: 2)
        try rig.bridge.setTalkPermission(device: rig.device, channel: 2, allowed: true)
        try rig.bridge.startTalk(tile)
        rig.bridge.closeTile(tile)
        #expect(talkStates(rig).last?.reason == "tile_closed")
    }
}
