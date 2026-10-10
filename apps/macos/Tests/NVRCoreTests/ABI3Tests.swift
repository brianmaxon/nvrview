import FakeCore
import Foundation
import NVRCore
import Testing
import VideoKit

// ABI 3 (docs/FFI-ABI3.md) through CoreBridge and FakeCore: event parsing,
// tier switches with generations, audio level, layouts store, detections,
// timings, several devices.

@Suite struct ABI3EventParsingTests {
    private func parse(_ s: String) -> CoreEvent? { CoreEvent.parse(Data(s.utf8)) }

    @Test func tierEvents() {
        #expect(
            parse(#"{"type":"tile_tier","tile":7,"tier":"main","state":"starting","generation":2}"#)
                == .tileTier(tile: 7, tier: .main, state: .starting, generation: 2))
        #expect(
            parse(#"{"type":"tile_tier","tile":7,"tier":"sub","state":"active","generation":3}"#)
                == .tileTier(tile: 7, tier: .sub, state: .active, generation: 3))
        #expect(
            parse(#"{"type":"tile_tier","tile":7,"tier":"main","state":"failed","generation":4,"reason":"x"}"#)
                == .tileTier(tile: 7, tier: .main, state: .failed, generation: 4))
        #expect(
            parse(#"{"type":"tile_tier","tile":7,"tier":"future","state":"paused","generation":1}"#)
                == .tileTier(tile: 7, tier: nil, state: .other("paused"), generation: 1))
        #expect(parse(#"{"type":"tile_tier","tile":7,"state":"active"}"#) == nil)
    }

    @Test func detectionAndMotion() {
        #expect(
            parse(#"{"type":"detection","device":1,"channel":3,"kind":"people","state":"start","source":"device","at_ns":123}"#)
                == .detection(DetectionEvent(device: 1, channel: 3, kind: "people", active: true, source: .device, atNanos: 123)))
        #expect(
            parse(#"{"type":"detection","device":1,"channel":3,"kind":"people","state":"stop","source":"device","at_ns":456}"#)
                == .detection(DetectionEvent(device: 1, channel: 3, kind: "people", active: false, source: .device, atNanos: 456)))
        #expect(
            parse(#"{"type":"detection","device":2,"channel":0,"kind":"package","state":"start","source":"local"}"#)
                == .detection(DetectionEvent(device: 2, channel: 0, kind: "package", active: true, source: .local)))
        #expect(
            parse(#"{"type":"motion","device":1,"channel":5,"state":"start","at_ns":789}"#)
                == .detection(DetectionEvent(device: 1, channel: 5, kind: "motion", active: true, source: .device, atNanos: 789)))
        #expect(parse(#"{"type":"detection","device":1,"kind":"people","state":"start"}"#) == nil)
    }

    @Test func layoutsTimingAndChannelCapabilities() {
        #expect(parse(#"{"type":"layouts_changed"}"#) == .layoutsChanged)
        #expect(
            parse(#"{"type":"timing","what":"first_picture","tile":7,"ns":123456789}"#)
                == .timing(what: "first_picture", tile: 7, nanos: 123_456_789))
        #expect(
            parse(#"{"type":"timing","what":"tier_switch","tile":7,"ns":2345678}"#)
                == .timing(what: "tier_switch", tile: 7, nanos: 2_345_678))
        #expect(
            parse(#"{"type":"channels","device":1,"channels":[{"channel":0,"detects":["people","vehicle","dog_cat"]},{"channel":1}]}"#)
                == .channels(
                    device: 1,
                    channels: [
                        ChannelInfo(channel: 0, detects: ["people", "vehicle", "dog_cat"]), ChannelInfo(channel: 1),
                    ]))
    }
}

@Suite struct TierSwitchTests {
    private func rig(_ configure: (inout FakeCore.Options) -> Void = { _ in }) throws -> Rig {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        configure(&o)
        return try Rig(o)
    }

    private func tierEvents(_ rig: Rig) -> [CoreEvent] {
        rig.events.get().filter { if case .tileTier = $0 { true } else { false } }
    }

    /// Pumps one access unit per pipeline and waits until it has decoded.
    private func step(_ rig: Rig, _ tile: CoreTile) {
        rig.fake.pump(tile: tile.id, count: 1)
        tile.waitForPendingFrames()
    }

    @Test func switchKeepsShowingUntilTheNewTierDecodes() async throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<12 { step(rig, tile) }
        #expect(tile.latestPicture() != nil)
        let sequenceBefore = tile.pictureSequence
        let versionBefore = tile.snapshot.version

        try rig.bridge.setTileTier(tile, .main)
        #expect(tile.tierState.requested == .main)
        #expect(tile.tierState.shown == .sub)
        #expect(tierEvents(rig) == [.tileTier(tile: tile.id, tier: .main, state: .starting, generation: 1)])

        // Both pipelines run until the new one's first picture: the tile
        // never goes without a picture meanwhile.
        var sawTwoDecoders = false
        var steps = 0
        while rig.fake.generations(tile: tile.id)?.shown != 1, steps < 400 {
            step(rig, tile)
            steps += 1
            if tile.decoders.count == 2 { sawTwoDecoders = true }
            #expect(tile.latestPicture() != nil)
        }
        #expect(rig.fake.generations(tile: tile.id)?.shown == 1)
        #expect(sawTwoDecoders)
        // The switch is announced at the core's next turn.
        step(rig, tile)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .active, generation: 1))
        #expect(tile.tierState.shown == .main)
        #expect(tile.tierState.shownGeneration == 1)
        #expect(tile.tierState.requested == .main)
        #expect(tile.tier == .main)
        #expect(tile.decoders.count == 1)
        #expect(tile.latestPicture() != nil)
        #expect(tile.pictureSequence != sequenceBefore)
        #expect(tile.snapshot.version != versionBefore)
        let timings = rig.events.get().compactMap { e -> String? in
            if case .timing(let what, let t, _) = e, t == tile.id { return what }
            return nil
        }
        #expect(timings.contains("tier_switch"))

        // Answers carried both generations, and nothing was left unanswered
        // or answered wrongly.
        let generations = Set(rig.fake.answers(tile: tile.id)?.generations ?? [])
        #expect(generations == [0, 1])
        #expect(await rig.fake.waitUntilAnswered())
        #expect(rig.fake.violations.isEmpty)

        // And back to the sub-stream for the grid.
        try rig.bridge.setTileTier(tile, .sub)
        steps = 0
        while tile.tierState.shown != .sub, steps < 400 {
            step(rig, tile)
            steps += 1
            #expect(tile.latestPicture() != nil)
        }
        #expect(tile.tierState.shown == .sub)
        #expect(tile.tierState.shownGeneration == 2)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func aFailedSwitchKeepsTheOldTier() throws {
        let rig = try rig { $0.tierSwitchFails = true }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<5 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        step(rig, tile)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .failed, generation: 1))
        #expect(tile.tierState.shown == .sub)
        #expect(tile.tierState.requested == .sub)
        #expect(tile.tierState.lastState == .failed)
        #expect(tile.decoders.count == 1)
        for _ in 0..<5 { step(rig, tile) }
        #expect(tile.latestPicture() != nil)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func aPendingSwitchCanBeCalledOff() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<3 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        try rig.bridge.setTileTier(tile, .sub)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .failed, generation: 1))
        #expect(rig.fake.generations(tile: tile.id)?.running == [0])
        #expect(tile.tierState.shown == .sub)
        for _ in 0..<5 { step(rig, tile) }
        #expect(rig.fake.violations.isEmpty)
        // Asking for the tier already shown does nothing.
        let count = tierEvents(rig).count
        try rig.bridge.setTileTier(tile, .sub)
        #expect(tierEvents(rig).count == count)
    }

    @Test func gridRendersTheShownGeneration() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<5 { step(rig, tile) }
        let tiles = CoreGrid.gridTiles(
            bridge: rig.bridge, tiles: [tile], snapshots: [tile.snapshot],
            pictures: [PixelSize(width: 320, height: 240)], viewport: PixelSize(width: 640, height: 480), gap: 0,
            fill: false)
        #expect(tiles.count == 1)
        // The renderer draws the tile itself, not one decoder, so a switch
        // needs no new GridTile.
        #expect(tiles.first?.picture === tile)
    }
}

@Suite struct AudioLevelTests {
    @Test func levelFollowsListening() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        let tile = try rig.open()
        #expect(rig.bridge.audioLevel(tile) == .silent)
        try rig.bridge.setTileAudio(tile, audible: true)
        let level = try #require(rig.bridge.audioLevel(tile))
        #expect(level.peak > 0 && level.peak <= 1)
        #expect(level.rms <= level.peak)
        rig.fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.5, rms: 0.25))
        #expect(rig.bridge.audioLevel(tile) == AudioLevel(peak: 0.5, rms: 0.25))
        try rig.bridge.setTileAudio(tile, audible: false)
        #expect(rig.bridge.audioLevel(tile) == .silent)
    }
}

@Suite struct LayoutsStoreTests {
    private static let valid =
        #"{"id":"l1","name":"Layout 1","version":7,"root":{"split":"row","ratio":0.62,"a":{"camera":{"device":"dev-a","channel":3}},"b":{"split":"col","ratio":0.34,"a":{"camera":null},"b":{"camera":{"device":"dev-a","channel":9}}}},"future":{"x":1}}"#

    private func layouts(_ rig: Rig) throws -> [[String: Any]] {
        let json = try rig.bridge.layoutsJSON()
        let object = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        return try #require(object["layouts"] as? [[String: Any]])
    }

    @Test func putVersionsAndKeepsUnknownFields() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        #expect(try layouts(rig).isEmpty)
        try rig.bridge.putLayout(json: Self.valid)
        var list = try layouts(rig)
        #expect(list.count == 1)
        #expect(list[0]["version"] as? Int == 1)
        #expect(list[0]["future"] != nil)
        try rig.bridge.putLayout(json: Self.valid)
        list = try layouts(rig)
        #expect(list.count == 1)
        #expect(list[0]["version"] as? Int == 2)
        let changes = rig.events.get().filter { $0 == .layoutsChanged }.count
        #expect(changes == 2)
        try rig.bridge.deleteLayout(id: "l1")
        #expect(try layouts(rig).isEmpty)
        #expect(throws: CoreStatus.unknownID) { try rig.bridge.deleteLayout(id: "l1") }
    }

    @Test(arguments: [
        #"{"name":"x","root":{"camera":null}}"#,
        #"{"id":"","name":"x","root":{"camera":null}}"#,
        #"{"id":"a","root":{"camera":null}}"#,
        #"{"id":"a","name":"x","root":{"split":"row","ratio":1.0,"a":{"camera":null},"b":{"camera":null}}}"#,
        #"{"id":"a","name":"x","root":{"split":"diagonal","ratio":0.5,"a":{"camera":null},"b":{"camera":null}}}"#,
        #"{"id":"a","name":"x","root":{"split":"row","ratio":0.5,"a":{"camera":null}}}"#,
        #"{"id":"a","name":"x","root":{"camera":{"device":"d","channel":"3"}}}"#,
        #"{"id":"a","name":"x","root":{"camera":{"device":"d","channel":300}}}"#,
        #"{"id":"a","name":"x","root":{"camera":{"device":"","channel":1}}}"#,
        #"{"id":"a","name":"x","root":{}}"#,
        "not json",
    ])
    func invalidLayoutsAreRefused(json: String) throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        #expect(throws: CoreStatus.invalidArgument) { try rig.bridge.putLayout(json: json) }
        #expect(try layouts(rig).isEmpty)
    }

    @Test func limitAndFile() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-layouts-\(UUID())")
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("layouts.json")
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.maxLayouts = 2
        o.layoutsFile = file
        let rig = try Rig(o)
        for i in 0..<2 {
            try rig.bridge.putLayout(json: #"{"id":"l\#(i)","name":"Layout","root":{"camera":null}}"#)
        }
        #expect(throws: CoreStatus.limit) {
            try rig.bridge.putLayout(json: #"{"id":"l9","name":"Layout","root":{"camera":null}}"#)
        }
        // Replacing an existing one is still allowed at the limit.
        try rig.bridge.putLayout(json: #"{"id":"l0","name":"Renamed","root":{"camera":null}}"#)
        rig.bridge.shutdown()

        // A new core reads them back from the file.
        let again = try Rig(o)
        defer { again.bridge.shutdown() }
        let list = try layouts(again)
        #expect(list.map { $0["id"] as? String } == ["l0", "l1"])
        #expect(list[0]["name"] as? String == "Renamed")
        #expect(list[0]["version"] as? Int == 2)
    }
}

@Suite struct DetectionAndDeviceTests {
    @Test func detectionsArriveAsEvents() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        rig.fake.emitDetection(device: rig.device, channel: 2, kind: "vehicle", start: true)
        rig.fake.emitDetection(device: rig.device, channel: 2, kind: "vehicle", start: false)
        rig.fake.emitDetection(device: rig.device, channel: 1, kind: "motion", start: true)
        let detections = rig.events.get().compactMap { e -> DetectionEvent? in
            if case .detection(let d) = e { return d }
            return nil
        }
        #expect(detections.map(\.kind) == ["vehicle", "vehicle", "motion"])
        #expect(detections.map(\.active) == [true, false, true])
        #expect(detections.allSatisfy { $0.source == .device && $0.atNanos != nil && $0.device == rig.device })
    }

    @Test func channelsCarryCapabilities() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        let channels = rig.events.get().compactMap { e -> [ChannelInfo]? in
            if case .channels(_, let c) = e { return c }
            return nil
        }.last ?? []
        #expect(channels.count == 4)
        #expect(channels[0].detects == ["people", "vehicle", "dog_cat"])
        #expect(channels[1].detects.isEmpty)
    }

    @Test func firstPictureTiming() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        let tile = try rig.open()
        rig.play(tile, 3)
        rig.fake.flushEvents()
        let timings = rig.events.get().compactMap { e -> (String, UInt64?, UInt64)? in
            if case .timing(let what, let t, let ns) = e { return (what, t, ns) }
            return nil
        }
        #expect(timings.count == 1)
        #expect(timings.first?.0 == "first_picture")
        #expect(timings.first?.1 == tile.id)
        #expect((timings.first?.2 ?? 0) > 0)
    }

    @Test func twoDevicesAtOnce() throws {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264"), try Fixtures.clip("h265-basic.h265")])
        o.devices = [
            "192.0.2.10": FakeDeviceSpec(name: "NVR 1", channelCount: 8),
            "192.0.2.11": FakeDeviceSpec(name: "NVR 2", channelCount: 2),
        ]
        let rig = try Rig(o)
        defer { rig.bridge.shutdown() }
        let a = try rig.bridge.addDevice(host: "192.0.2.10", port: 9000, user: "example-user", password: "example")
        let b = try rig.bridge.addDevice(host: "192.0.2.11", port: 9000, user: "example-user", password: "example")
        let names = rig.events.get().compactMap { e -> String? in
            if case .deviceInfo(let d, let name, _) = e, d == a || d == b { return name }
            return nil
        }
        #expect(names == ["NVR 1", "NVR 2"])
        let counts = rig.events.get().compactMap { e -> Int? in
            if case .channels(let d, let c) = e, d == a || d == b { return c.count }
            return nil
        }
        #expect(counts == [8, 2])
        let ta = try rig.bridge.openTile(device: a, channel: 0, tier: .sub)
        let tb = try rig.bridge.openTile(device: b, channel: 1, tier: .sub)
        rig.play(ta, 4)
        rig.play(tb, 4)
        #expect(ta.latestPicture() != nil)
        #expect(tb.latestPicture() != nil)
        #expect(ta.snapshot.codec == .h264)
        #expect(tb.snapshot.codec == .h265)
        #expect(rig.fake.violations.isEmpty)
    }
}
