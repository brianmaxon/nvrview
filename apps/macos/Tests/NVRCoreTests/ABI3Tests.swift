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
            parse(#"{"type":"tile_tier","tile":7,"tier":"main","state":"failed","generation":4,"reason":"timeout"}"#)
                == .tileTier(tile: 7, tier: .main, state: .failed, generation: 4, reason: "timeout"))
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
            parse(#"{"type":"detection","device":1,"channel":2,"kind":"smoke_alarm","state":"start","source":"local_audio","at_ns":457}"#)
                == .detection(
                    DetectionEvent(device: 1, channel: 2, kind: "smoke_alarm", active: true, source: .localAudio, atNanos: 457)))
        #expect(
            parse(#"{"type":"detection","device":1,"channel":2,"kind":"co_alarm","state":"stop","source":"local_audio","at_ns":9}"#)
                == .detection(
                    DetectionEvent(device: 1, channel: 2, kind: "co_alarm", active: false, source: .localAudio, atNanos: 9)))
        #expect(
            parse(#"{"type":"motion","device":1,"channel":5,"state":"start","at_ns":789}"#)
                == .detection(DetectionEvent(device: 1, channel: 5, kind: "motion", active: true, source: .device, atNanos: 789)))
        #expect(parse(#"{"type":"detection","device":1,"kind":"people","state":"start"}"#) == nil)
        #expect(
            parse(#"{"type":"detection","device":1,"channel":3,"lens":1,"kind":"People","state":"stop","source":"device","at_ns":456}"#)
                == .detection(
                    DetectionEvent(device: 1, channel: 3, kind: "people", active: false, source: .device, atNanos: 456, lens: 1)))
        #expect(
            parse(#"{"type":"alarm","device":1,"channel":2,"kind":"visitor","state":"start","at_ns":790}"#)
                == .detection(
                    DetectionEvent(device: 1, channel: 2, kind: "visitor", active: true, source: .device, atNanos: 790, alarm: true)))
    }

    @Test func layoutsTimingAndChannelCapabilities() {
        #expect(parse(#"{"type":"layouts_changed"}"#) == .layoutsChanged(revision: nil))
        #expect(parse(#"{"type":"layouts_changed","revision":4}"#) == .layoutsChanged(revision: 4))
        #expect(
            parse(#"{"type":"timing","what":"first_picture","tile":7,"ns":123456789}"#)
                == .timing(what: "first_picture", tile: 7, nanos: 123_456_789))
        #expect(
            parse(#"{"type":"timing","what":"tier_switch","tile":7,"ns":2345678}"#)
                == .timing(what: "tier_switch", tile: 7, nanos: 2_345_678))
        #expect(
            parse(#"{"type":"timing","what":"tier_switch","tile":7,"ns":2345678,"delta_ms":40,"held_ms":0,"capped":false}"#)
                == .timing(
                    what: "tier_switch", tile: 7, nanos: 2_345_678,
                    tierSwitch: TierSwitchTiming(deltaMs: 40, heldMs: 0, capped: false)))
        #expect(
            parse(#"{"type":"timing","what":"tier_switch","tile":7,"ns":9,"delta_ms":-520,"held_ms":300,"capped":true}"#)
                == .timing(
                    what: "tier_switch", tile: 7, nanos: 9,
                    tierSwitch: TierSwitchTiming(deltaMs: -520, heldMs: 300, capped: true)))
        // Nothing of the old generation decoded: no delta_ms.
        #expect(
            parse(#"{"type":"timing","what":"tier_switch","tile":7,"ns":9,"held_ms":0,"capped":false}"#)
                == .timing(
                    what: "tier_switch", tile: 7, nanos: 9, tierSwitch: TierSwitchTiming(deltaMs: nil, heldMs: 0, capped: false)))
        // Other timings ignore the fields.
        #expect(
            parse(#"{"type":"timing","what":"first_picture","tile":7,"ns":5,"held_ms":3}"#)
                == .timing(what: "first_picture", tile: 7, nanos: 5))
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
        #expect(rig.fake.lastActions(tile: tile.id).contains { $0 == "reset@0" })
        #expect(tile.tierState.shown == .main)
        #expect(tile.tierState.shownGeneration == 1)
        #expect(tile.tierState.requested == .main)
        #expect(tile.tier == .main)
        #expect(tile.decoders.count == 1)
        #expect(tile.latestPicture() != nil)
        #expect(tile.pictureSequence != sequenceBefore)
        #expect(tile.snapshot.version != versionBefore)
        let timings = rig.events.get().compactMap { e -> String? in
            if case .timing(let what, let t, _, _) = e, t == tile.id { return what }
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

    /// The device's rate comes from the camera timestamps (the fake sends
    /// one picture per 50 ms, however fast the test pumps), and a tier
    /// switch starts it over with the new generation's own decoder.
    @Test func deviceRateFromTimestampsStartsOverOnASwitch() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        step(rig, tile)
        #expect(tile.deviceRate == nil)
        for _ in 0..<30 { step(rig, tile) }
        #expect(abs((tile.deviceRate ?? 0) - 20) < 0.5)
        let before = tile.decoder
        try rig.bridge.setTileTier(tile, .main)
        var steps = 0
        while tile.tierState.shown != .main, steps < 400 {
            step(rig, tile)
            steps += 1
        }
        #expect(tile.tierState.shown == .main)
        #expect(tile.decoder !== before)
        // Only the new generation's pictures count: few so far, but never a
        // rate made of two streams' timestamps.
        let decoded = Int(tile.decoder.stats.decoded)
        if decoded < 2 { #expect(tile.deviceRate == nil) } else { #expect(abs((tile.deviceRate ?? 0) - 20) < 0.5) }
        for _ in 0..<30 { step(rig, tile) }
        #expect(abs((tile.deviceRate ?? 0) - 20) < 0.5)
    }

    @Test func aFailedSwitchKeepsTheOldTier() throws {
        let rig = try rig { $0.tierSwitchFails = true }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<5 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        step(rig, tile)
        #expect(
            tierEvents(rig).last
                == .tileTier(tile: tile.id, tier: .main, state: .failed, generation: 1, reason: "start_rejected"))
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
        #expect(
            tierEvents(rig).suffix(2)
                == [
                    .tileTier(tile: tile.id, tier: .main, state: .failed, generation: 1, reason: "superseded"),
                    .tileTier(tile: tile.id, tier: .sub, state: .active, generation: 0),
                ])
        #expect(rig.fake.generations(tile: tile.id)?.running == [0])
        #expect(tile.tierState.shown == .sub)
        for _ in 0..<5 { step(rig, tile) }
        // The superseded generation ended with its own Reset.
        #expect(rig.fake.lastActions(tile: tile.id).contains("reset@1"))
        #expect(tile.decoders.count == 1)
        #expect(rig.fake.violations.isEmpty)
        // Asking for the tier already shown starts nothing; it is reported again.
        try rig.bridge.setTileTier(tile, .sub)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .sub, state: .active, generation: 0))
        #expect(rig.fake.generations(tile: tile.id)?.running == [0])
    }

    // MARK: Aligned switches (docs/FFI.md, "Switching a tile's stream tier")

    private func switchTiming(_ rig: Rig, _ tile: CoreTile) -> TierSwitchTiming? {
        rig.events.get().compactMap { e -> TierSwitchTiming? in
            if case .timing("tier_switch", tile.id, _, let detail) = e { return detail }
            return nil
        }.last
    }

    /// Pumps until the tile shows `tier`, checking that it always has a
    /// picture and, while the old generation is shown, that the picture is
    /// the old generation's (never a new-generation one before the final Reset).
    private func pumpToSwitch(_ rig: Rig, _ tile: CoreTile, to tier: StreamTier, limit: Int = 200) {
        var steps = 0
        while tile.tierState.shown != tier, steps < limit {
            step(rig, tile)
            steps += 1
            if tile.tierState.shownGeneration == 0, tile.decoders.count == 2 {
                #expect(tile.latestPicture()?.ptsMicros == tile.decoders[0].latestPicture()?.ptsMicros)
            }
        }
        rig.fake.flushEvents()
    }

    @Test func mainAheadOfSubSwitchesAtOnce() throws {
        let rig = try rig { $0.tierPTSOffsetMicros = [.main: 100_000] }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<6 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        pumpToSwitch(rig, tile, to: .main)
        #expect(tile.tierState.shown == .main)
        let t = try #require(switchTiming(rig, tile))
        #expect((t.deltaMs ?? 0) >= 100)
        #expect(t.heldMs == 0)
        #expect(!t.capped)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func mainBehindSubHoldsThenSwitchesAtTheCap() throws {
        let rig = try rig {
            $0.tierPTSOffsetMicros = [.main: -100_000]
            $0.tierAlignCap = .milliseconds(60)
        }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<6 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        // Held: the new generation decodes but is never presented, however
        // many pictures it has decoded.
        // (The new stream waits for its first keyframe first.)
        var steps = 0
        while (tile.decoders.count < 2 || tile.decoders[1].stats.decoded < 3), steps < 400 {
            step(rig, tile)
            steps += 1
        }
        #expect(tile.tierState.shown == .sub)
        #expect(tile.tierState.shownGeneration == 0)
        #expect(tile.decoders.count == 2)
        #expect((tile.decoders[1].stats.decoded) > 0)
        #expect(tile.latestPicture()?.ptsMicros == tile.decoders[0].latestPicture()?.ptsMicros)
        #expect(tile.latestPicture()?.ptsMicros != tile.decoders[1].latestPicture()?.ptsMicros)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .starting, generation: 1))
        #expect(switchTiming(rig, tile) == nil)

        Thread.sleep(forTimeInterval: 0.1)
        pumpToSwitch(rig, tile, to: .main, limit: 4)
        #expect(tile.tierState.shown == .main)
        let t = try #require(switchTiming(rig, tile))
        #expect(t.capped)
        #expect(t.heldMs >= 60)
        #expect((t.deltaMs ?? 0) < 0)
        #expect(rig.fake.lastActions(tile: tile.id).contains { $0 == "reset@0" })
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func mainFarBehindSubIsNotOneClock() throws {
        let rig = try rig { $0.tierPTSOffsetMicros = [.main: -6_000_000] }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<6 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        pumpToSwitch(rig, tile, to: .main, limit: 400)
        #expect(tile.tierState.shown == .main)
        let t = try #require(switchTiming(rig, tile))
        #expect(!t.capped)
        #expect(t.heldMs == 0)
        #expect((t.deltaMs ?? 0) < -5000)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func otherTierPairsSwitchAtTheFirstPicture() throws {
        let rig = try rig { $0.tierPTSOffsetMicros = [.mobile: -100_000] }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<6 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .mobile)
        pumpToSwitch(rig, tile, to: .mobile, limit: 400)
        #expect(tile.tierState.shown == .mobile)
        let t = try #require(switchTiming(rig, tile))
        #expect(!t.capped)
        #expect(t.heldMs == 0)
        #expect((t.deltaMs ?? 0) < 0)  // stepped back, and allowed to
        #expect(rig.fake.violations.isEmpty)
    }

    /// The new generation's pictures are decoded while the old one is held,
    /// and the tile presents none of them until the old generation's final
    /// Reset: the Reset is what moves the tile to the new generation.
    @Test func newPicturesAreNotPresentedBeforeTheFinalReset() throws {
        let rig = try rig { $0.tierPTSOffsetMicros = [.main: 100_000] }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<6 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        var steps = 0
        while rig.fake.generations(tile: tile.id)?.shown != 1, steps < 50 {
            step(rig, tile)
            steps += 1
        }
        // The core switched; its final Reset and event are not sent yet.
        #expect(tile.tierState.shownGeneration == 0)
        #expect(tile.decoders.count == 2)
        #expect(tile.decoders[1].latestPicture() != nil)
        #expect(tile.latestPicture()?.ptsMicros == tile.decoders[0].latestPicture()?.ptsMicros)
        rig.fake.deliverFinalResetsOnly(tile: tile.id)
        #expect(tile.tierState.shownGeneration == 1)
        #expect(tile.latestPicture()?.ptsMicros == tile.decoders[0].latestPicture()?.ptsMicros)
    }

    @Test func aSwitchThatNeverDecodesTimesOut() throws {
        let rig = try rig { $0.tierSwitchTimeout = .milliseconds(50) }
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<3 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        // Never answered: the new generation's actions are only executed, and
        // its decoder is kept from answering by pumping nothing for it.
        Thread.sleep(forTimeInterval: 0.1)
        step(rig, tile)
        #expect(
            tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .failed, generation: 1, reason: "timeout"))
        #expect(tile.tierState.shown == .sub)
        #expect(tile.decoders.count == 1)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func theFinalResetSwitchesBeforeTheEvent() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<12 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        var steps = 0
        while rig.fake.generations(tile: tile.id)?.shown != 1, steps < 400 {
            step(rig, tile)
            steps += 1
        }
        // Only the final Reset, no event yet: the tile already shows the new
        // generation (the action path decides; the event confirms).
        rig.fake.deliverFinalResetsOnly(tile: tile.id)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .starting, generation: 1))
        #expect(tile.tierState.shownGeneration == 1)
        #expect(tile.decoders.count == 1)
        #expect(tile.latestPicture() != nil)
        step(rig, tile)
        #expect(tierEvents(rig).last == .tileTier(tile: tile.id, tier: .main, state: .active, generation: 1))
        #expect(rig.fake.violations.isEmpty)
    }

    /// An `active` repeated for an earlier request can reach the app after
    /// it asked for the next tier: the next generation must still get the
    /// tier the core started it with.
    @Test func aRepeatedActiveDoesNotConfuseTheNextSwitch() throws {
        let rig = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try rig.bridge.openTile(device: rig.device, channel: 0, tier: .sub)
        for _ in 0..<12 { step(rig, tile) }
        try rig.bridge.setTileTier(tile, .main)
        var steps = 0
        while tile.tierState.shown != .main, steps < 400 { step(rig, tile); steps += 1 }
        try rig.bridge.setTileTier(tile, .sub)
        rig.fake.emit(event: #"{"type":"tile_tier","tile":\#(tile.id),"tier":"main","state":"active","generation":1}"#)
        #expect(tile.tierState.requested == .sub)
        steps = 0
        while tile.tierState.shownGeneration != 2, steps < 400 { step(rig, tile); steps += 1 }
        #expect(tile.tierState.shown == .sub)
        #expect(tile.tierState.requested == .sub)
        #expect(rig.fake.violations.isEmpty)
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
        // Whether audible or not: the level of the decoded audio.
        let muted = try #require(rig.bridge.audioLevel(tile))
        #expect(muted.peak > 0 && muted.peak <= 1)
        try rig.bridge.setTileAudio(tile, audible: true)
        let level = try #require(rig.bridge.audioLevel(tile))
        #expect(level.peak > 0 && level.peak <= 1)
        #expect(level.rms <= level.peak)
        rig.fake.setAudioLevel(tile: tile.id, AudioLevel(peak: 0.5, rms: 0.25))
        #expect(rig.bridge.audioLevel(tile) == AudioLevel(peak: 0.5, rms: 0.25))
        // Measured before the volume: a muted audible tile still shows sound arriving.
        try rig.bridge.setTileAudio(tile, audible: true, volume: 0)
        #expect(rig.bridge.audioLevel(tile) == AudioLevel(peak: 0.5, rms: 0.25))
        try rig.bridge.setTileAudio(tile, audible: false)
        #expect(rig.bridge.audioLevel(tile) == AudioLevel(peak: 0.5, rms: 0.25))
        // Unknown tiles read 0, never an error.
        #expect(try rig.fake.tileAudioLevel(999) == .silent)
    }

    @Test func aCameraWithoutAudioReadsZero() throws {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.devices = ["192.0.2.1": FakeDeviceSpec(channelCount: 2, channelsWithoutAudio: [1])]
        let rig = try Rig(o)
        defer { rig.bridge.shutdown() }
        let quiet = try rig.open(channel: 1)
        #expect(rig.bridge.audioLevel(quiet) == .silent)
        try rig.bridge.setTileAudio(quiet, audible: true)
        #expect(rig.bridge.audioLevel(quiet) == .silent)
        #expect((rig.bridge.audioLevel(try rig.open(channel: 0))?.peak ?? 0) > 0)
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
        let revisions = rig.events.get().compactMap { e -> Int? in
            if case .layoutsChanged(let r) = e { return r }
            return nil
        }
        #expect(revisions == [1, 2])
        let document = try #require(
            try JSONSerialization.jsonObject(with: Data(try rig.bridge.layoutsJSON().utf8)) as? [String: Any])
        #expect(document["revision"] as? Int == 2)
        try rig.bridge.deleteLayout(id: "l1")
        #expect(try layouts(rig).isEmpty)
        #expect(throws: CoreStatus.unknownID) { try rig.bridge.deleteLayout(id: "l1") }
    }

    @Test func limitsFromTheContract() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        func chain(_ splits: Int) -> String {
            var node = #"{"camera":null}"#
            for _ in 0..<splits { node = #"{"split":"row","ratio":0.5,"a":\#(node),"b":{"camera":null}}"# }
            return #"{"id":"deep","name":"Deep","root":\#(node)}"#
        }
        try rig.bridge.putLayout(json: chain(16))
        #expect(throws: CoreStatus.invalidArgument) { try rig.bridge.putLayout(json: chain(17)) }
        try rig.bridge.putLayout(
            json: #"{"id":"r","name":"R","root":{"split":"col","ratio":0.02,"a":{"camera":null},"b":{"camera":null}}}"#)
        #expect(throws: CoreStatus.invalidArgument) {
            try rig.bridge.putLayout(
                json: #"{"id":"r","name":"R","root":{"split":"col","ratio":0.01,"a":{"camera":null},"b":{"camera":null}}}"#)
        }
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

@Suite struct DeviceKeyTests {
    @Test func keysAreStablePerAddress() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        let key = try #require(rig.bridge.deviceKey(rig.device))
        #expect(key.hasPrefix("d-") && key.count == 18)
        #expect(key == FakeCore.key(host: "192.0.2.1", port: 9000))
        #expect(FakeCore.key(host: "NVR.example", port: 9000) == FakeCore.key(host: "nvr.example", port: 9000))
        #expect(FakeCore.key(host: "192.0.2.1", port: 9001) != key)
        let again = try rig.bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "example")
        #expect(rig.bridge.deviceKey(again) == key)
        #expect(rig.bridge.deviceKey(12345) == nil)
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

    @Test func soundDetectionsArriveWithoutALens() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        rig.fake.emitSoundDetection(device: rig.device, channel: 2, kind: "co_alarm", start: true)
        rig.fake.emitSoundDetection(device: rig.device, channel: 2, kind: "co_alarm", start: false)
        let detections = rig.events.get().compactMap { e -> DetectionEvent? in
            if case .detection(let d) = e { return d }
            return nil
        }
        #expect(detections.map(\.kind) == ["co_alarm", "co_alarm"])
        #expect(detections.map(\.active) == [true, false])
        #expect(detections.allSatisfy { $0.source == .localAudio && $0.lens == nil && $0.atNanos != nil })
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
            if case .timing(let what, let t, let ns, _) = e { return (what, t, ns) }
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
