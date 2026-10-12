import FakeCore
import Foundation
import NVRCore
import NVRViewCore
import Testing
import VideoKit

@Suite struct EventParsingTests {
    private func parse(_ s: String) -> CoreEvent? { CoreEvent.parse(Data(s.utf8)) }

    @Test func everyDocumentedEvent() {
        #expect(parse(#"{"type":"device_state","device":1,"state":"connecting"}"#) == .deviceState(device: 1, state: .connecting))
        #expect(parse(#"{"type":"device_state","device":1,"state":"connected"}"#) == .deviceState(device: 1, state: .connected))
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"backoff","attempt":3,"retry_ms":8000}"#)
                == .deviceState(device: 1, state: .backoff(attempt: 3, retryMilliseconds: 8000)))
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"login_failed","status":401,"remain_times":2}"#)
                == .deviceState(device: 1, state: .loginFailed(status: 401, remainTimes: 2)))
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"login_failed","status":407,"reason":"account locked","remain_times":0,"unlock_s":300}"#)
                == .deviceState(
                    device: 1, state: .loginFailed(status: 407, remainTimes: 0, reason: "account locked", unlockSeconds: 300)))
        #expect(parse(#"{"type":"device_state","device":1,"state":"closed"}"#) == .deviceState(device: 1, state: .closed))
        #expect(
            parse(#"{"type":"device_info","device":1,"name":"NVR","model":"EXAMPLE-MODEL"}"#)
                == .deviceInfo(device: 1, name: "NVR", model: "EXAMPLE-MODEL"))
        #expect(
            parse(#"{"type":"channels","device":1,"channels":[{"channel":0,"connected":true,"tiers":["main","sub"],"name":"Channel 1","model":"EXAMPLE-CAM"}]}"#)
                == .channels(
                    device: 1,
                    channels: [ChannelInfo(channel: 0, connected: true, tiers: ["main", "sub"], name: "Channel 1", model: "EXAMPLE-CAM")]))
        #expect(
            parse(#"{"type":"tile_stats","tile":7,"received":120,"decoded":118,"gated":0,"dropped":2,"failures":0,"resets":0}"#)
                == .tileStats(tile: 7, stats: TileStats(received: 120, decoded: 118, gated: 0, dropped: 2, failures: 0, resets: 0)))
        #expect(
            parse(#"{"type":"tile_closed","tile":7,"reason":"device_removed"}"#)
                == .tileClosed(tile: 7, reason: "device_removed"))
    }

    @Test func unknownTypesAndFieldsAreIgnored() {
        #expect(parse(#"{"type":"something_new","device":1}"#) == nil)
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"connected","extra":{"a":[1,2]},"more":null}"#)
                == .deviceState(device: 1, state: .connected))
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"hibernating"}"#)
                == .deviceState(device: 1, state: .other("hibernating")))
        // Unknown fields inside nested channel objects too, and missing optional ones.
        #expect(
            parse(#"{"type":"channels","device":2,"channels":[{"channel":3,"future":true},{"channel":4,"name":"x"}]}"#)
                == .channels(device: 2, channels: [ChannelInfo(channel: 3), ChannelInfo(channel: 4, name: "x")]))
        // Login failure without the optional counters.
        #expect(
            parse(#"{"type":"device_state","device":1,"state":"login_failed"}"#)
                == .deviceState(device: 1, state: .loginFailed(status: nil, remainTimes: nil)))
    }

    @Test func malformedIsDropped() {
        #expect(parse("") == nil)
        #expect(parse("not json") == nil)
        #expect(parse(#"{"device":1}"#) == nil)
        #expect(parse(#"{"type":"device_state","state":"connected"}"#) == nil)  // no device
        #expect(parse(#"{"type":"tile_stats"}"#) == nil)
        #expect(parse(#"[1,2]"#) == nil)
    }
}

@Suite struct ContractMappingTests {
    @Test func failureKindsMatchTheHeaderOneToOne() {
        let pairs: [(DecodeFailure, CoreDecodeFailure, NvDecodeFailure)] = [
            (.invalidParameterSets, .invalidParameterSets, NV_FAIL_INVALID_PARAMETER_SETS),
            (.sessionCreateFailed(-1), .sessionCreateFailed, NV_FAIL_SESSION_CREATE_FAILED),
            (.notConfigured, .notConfigured, NV_FAIL_NOT_CONFIGURED),
            (.invalidAccessUnit, .invalidAccessUnit, NV_FAIL_INVALID_ACCESS_UNIT),
            (.submitFailed(-1), .submitFailed, NV_FAIL_SUBMIT_FAILED),
            (.decodeError(-1), .decodeError, NV_FAIL_DECODE_ERROR),
            (.noPicture, .noPicture, NV_FAIL_NO_PICTURE),
        ]
        #expect(pairs.count == CoreDecodeFailure.allCases.count)
        for (decoder, core, c) in pairs {
            #expect(CoreDecodeFailure(decoder) == core)
            #expect(NvDecodeFailure(core) == c)
            #expect(core.answersConfigure == (c == NV_FAIL_INVALID_PARAMETER_SETS || c == NV_FAIL_SESSION_CREATE_FAILED))
        }
    }

    @Test func enumsMatchTheHeader() {
        #expect(NvStreamTier(.main) == NV_TIER_MAIN && NvStreamTier(.sub) == NV_TIER_SUB)
        #expect(NvStreamTier(.mobile) == NV_TIER_MOBILE && NvStreamTier(.extern) == NV_TIER_EXTERN)
        #expect(ColorMatrix.bt601.rawValue == NV_MATRIX_BT601.rawValue)
        #expect(ColorMatrix.bt709.rawValue == NV_MATRIX_BT709.rawValue)
        #expect(ColorMatrix.bt2020.rawValue == NV_MATRIX_BT2020.rawValue)
        #expect(TileStatus.undecodable.rawValue == NV_TILE_UNDECODABLE.rawValue)
        #expect(CoreStatus.ok.code == NV_OK.rawValue)
        #expect(CoreStatus.invalidArgument.code == NV_ERR_INVALID_ARGUMENT.rawValue)
        #expect(CoreStatus.unknownID.code == NV_ERR_UNKNOWN_ID.rawValue)
        #expect(CoreStatus.closed.code == NV_ERR_CLOSED.rawValue)
        #expect(CoreStatus.limit.code == NV_ERR_LIMIT.rawValue)
        #expect(CoreStatus.internal.code == NV_ERR_INTERNAL.rawValue)
        #expect(NV_ABI_VERSION == 3)
        #expect(NV_ABI_MINOR == 6)
    }

    /// The package's copy of the header is the contract's, byte for byte.
    @Test func headerIsTheContract() throws {
        let contract = try Data(contentsOf: Fixtures.root.appendingPathComponent("docs/nvrview.h"))
        let copy = try Data(
            contentsOf: Fixtures.root.appendingPathComponent("apps/macos/Sources/NVRViewCore/include/nvrview.h"))
        #expect(contract == copy)
    }

    @Test func failuresReachTheCoreThroughTheDecoder() async throws {
        let clip = try Fixtures.clip("h264-basic.h264")
        let rig = try Rig(clip)
        let t = try rig.open()
        // Decode before any Configure.
        rig.fake.inject(tile: t.id, .decode(Fixtures.accessUnit(clip, 0), keyframe: true, ptsMicros: 0))
        // Configure with parameter sets that describe nothing.
        rig.fake.inject(
            tile: t.id,
            .configure(codec: .h264, parameterSets: [Data([0x67, 0x00])], color: .bt601VideoRange))
        // A good Configure, then an access unit with no slice data.
        rig.fake.inject(
            tile: t.id,
            .configure(codec: .h264, parameterSets: Fixtures.parameterSets(clip), color: .bt601VideoRange))
        rig.fake.inject(tile: t.id, .decode(Data([0, 0, 0, 1, 0x09, 0x10]), keyframe: false, ptsMicros: 50_000))
        #expect(await rig.fake.waitUntilAnswered())
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
        let answers = try #require(rig.fake.answers(tile: t.id))
        #expect(answers.failures == [.notConfigured, .invalidParameterSets, .invalidAccessUnit])
        rig.bridge.shutdown()
    }
}

@Suite struct DeviceEventTests {
    @Test func connectionEventsReachTheApp() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        let events = rig.events.get()
        #expect(events.first == .deviceState(device: rig.device, state: .connecting))
        #expect(events.contains(.deviceState(device: rig.device, state: .connected)))
        #expect(events.contains(.deviceInfo(device: rig.device, name: "NVR", model: "EXAMPLE-MODEL")))
        let channelCount = events.compactMap { e -> Int? in
            if case .channels(_, let c) = e { c.count } else { nil }
        }
        #expect(channelCount == [4])
        rig.bridge.shutdown()
    }

    @Test func failedLoginIsTerminalAndSilent() async throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.loginFailureRemaining = 2
        let rig = try Rig(options)
        #expect(
            rig.events.get().contains(
                .deviceState(device: rig.device, state: .loginFailed(status: 401, remainTimes: 2))))
        let tile = try rig.open()
        #expect(rig.fake.pump(tile: tile.id) == 0)
        #expect(tile.decoder.stats.submitted == 0)
        rig.bridge.shutdown()
    }

    @Test func removingADeviceEndsItsTiles() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        let tile = try rig.open()
        rig.play(tile, 3)
        try rig.bridge.removeDevice(rig.device)
        #expect(rig.bridge.tiles.isEmpty)
        let events = rig.events.get()
        #expect(events.contains(.tileClosed(tile: tile.id, reason: "device_removed")))
        #expect(events.last == .deviceState(device: rig.device, state: .closed))
        #expect(rig.fake.pump(tile: tile.id) == 0)
        #expect(rig.fake.violations.isEmpty)
        rig.bridge.shutdown()
    }

    @Test func errorsAreThrown() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        #expect(throws: CoreStatus.unknownID) { try rig.bridge.removeDevice(999) }
        #expect(throws: CoreStatus.unknownID) { _ = try rig.bridge.openTile(device: 999, channel: 0, tier: .main) }
        #expect(throws: CoreStatus.invalidArgument) {
            _ = try rig.bridge.addDevice(host: "", port: 9000, user: "u", password: "p")
        }
        rig.bridge.shutdown()
        #expect(throws: CoreStatus.closed) { _ = try rig.open() }
    }
}

/// Polls `condition` every few milliseconds until it holds or `timeout`
/// seconds pass (real-time pacing runs on the fake's own threads).
private func waitUntil(_ timeout: Double = 5, _ condition: () -> Bool) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(2)) }
}

@Suite struct TeardownTests {
    /// How long "nothing more happens" is watched for: at 200 pictures per
    /// second, ten pictures' worth.
    private static let quiet = Duration.milliseconds(50)

    /// Tiles running at speed, then shutdown: nothing is called into the core
    /// afterwards and nothing is called back.
    @Test func noCallbacksAfterShutdown() async throws {
        var options = FakeCore.Options(clips: [
            try Fixtures.clip("h264-basic.h264"), try Fixtures.clip("h265-basic.h265"),
        ])
        options.pacing = .realtime(fps: 200)
        let rig = try Rig(options)
        let tiles = try (0..<4).map { try rig.open(channel: UInt8($0)) }
        await waitUntil { tiles.allSatisfy { $0.decoder.stats.submitted > 0 } }
        #expect(tiles.allSatisfy { $0.decoder.stats.submitted > 0 })
        rig.bridge.shutdown()
        let submitted = tiles.map { $0.decoder.stats.submitted }
        try await Task.sleep(for: Self.quiet)
        #expect(tiles.map { $0.decoder.stats.submitted } == submitted)
        #expect(rig.fake.answersAfterFree == 0)
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
        #expect(rig.bridge.tiles.isEmpty)
        rig.bridge.shutdown()  // idempotent
        #expect(rig.bridge.gridLayout(viewport: PixelSize(width: 10, height: 10), gap: 0, pictureSizes: [.zero]).isEmpty)
    }

    @Test func closeTileStopsItsActions() async throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.pacing = .realtime(fps: 200)
        let rig = try Rig(options)
        let a = try rig.open(channel: 0)
        let b = try rig.open(channel: 1)
        await waitUntil { a.decoder.stats.submitted > 0 && b.decoder.stats.submitted > 0 }
        rig.bridge.closeTile(a)
        let frozen = a.decoder.stats.submitted
        let bBefore = b.decoder.stats.submitted
        // The other tile keeps going while the closed one stays still.
        await waitUntil { b.decoder.stats.submitted > bBefore + 5 }
        try await Task.sleep(for: Self.quiet)
        #expect(a.decoder.stats.submitted == frozen)
        #expect(b.decoder.stats.submitted > bBefore)  // the other tile keeps going
        #expect(rig.bridge.tiles.count == 1)
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
        rig.bridge.shutdown()
    }

    /// An action that arrives before `openTile` returns (the tile ID is not
    /// known yet) waits for the registration instead of being lost.
    @Test func actionsBeforeOpenReturnsAreNotLost() async throws {
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.pacing = .realtime(fps: 500)
        for _ in 0..<5 {
            let rig = try Rig(options)
            let tile = try rig.open()
            await waitUntil { tile.snapshot.configures >= 1 && tile.decoder.stats.submitted > 0 }
            // The first keyframe's Configure must have been executed.
            #expect(tile.snapshot.configures >= 1)
            #expect(tile.decoder.stats.submitted > 0)
            rig.bridge.shutdown()
            #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
        }
    }
}
