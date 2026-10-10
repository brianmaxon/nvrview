import AVFoundation
import FakeCore
import Foundation
import NVRCore
import Testing

/// Listening through the bridge, headless: an `AudioPlayer` in offline
/// manual-rendering mode (nothing reaches a speaker) pulls the real render
/// block, which calls FakeCore's render hook. FakeCore clicks whenever the
/// host time crosses a multiple of 500 ms, so the frames the clicks land on
/// show whether the block computes each buffer's host time correctly.
@Suite struct AudioTests {
    private func rig() throws -> (Rig, CoreTile) {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        return (rig, try rig.open())
    }

    /// Frame indices (of the interleaved stereo output) with a click.
    private func clicks(_ samples: [Float]) -> [(frame: Int, value: Float)] {
        stride(from: 0, to: samples.count, by: 2).compactMap { i in
            abs(samples[i]) > 0.05 ? (i / 2, samples[i]) : nil
        }
    }

    @Test func clicksArePlacedByHostTime() throws {
        let (rig, tile) = try rig()
        defer { rig.bridge.shutdown() }
        try rig.bridge.setTileAudio(tile, audible: true, volume: 0.5)
        let rate = 48_000.0
        let player = try rig.bridge.makeAudioPlayer(mode: .offline(sampleRate: rate, maximumFrames: 512))
        // Sample 0 is heard 123.4 ms before a click instant.
        let base = 10 * FakeAudio.clickPeriodNanos - 123_400_000
        try player.start(hostBaseNanos: base)
        #expect(rig.fake.audioOutput?.sampleRate == 48_000)
        #expect(rig.fake.audioOutput?.channels == 2)

        var out: [Float] = []
        for _ in 0..<(2 * 48_000 / 512) { out += try player.renderOffline(frames: 512) }
        let found = clicks(out)
        // Expected: every 500 ms from 123.4 ms on, both channels equal.
        let expected = (0..<4).map { Int(((0.1234 + Double($0) * 0.5) * rate).rounded(.up)) - 1 }
        #expect(found.count == 4, "\(found)")
        for (f, e) in zip(found, expected) {
            #expect(abs(f.frame - e) <= 1, "click at \(f.frame), expected \(e)")
            #expect(abs(f.value - FakeAudio.clickAmplitude * 0.5) < 1e-4)
            #expect(out[2 * f.frame + 1] == out[2 * f.frame])
        }
        #expect(rig.fake.audio.silentRenderCount == 0)

        // Silent tiles render silence; the engine keeps pulling.
        try rig.bridge.setTileAudio(tile, audible: false)
        var quiet: [Float] = []
        for _ in 0..<100 { quiet += try player.renderOffline(frames: 512) }
        #expect(quiet.allSatisfy { $0 == 0 })
        player.stop()
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
    }

    @Test func shutdownStopsPlayersBeforeTheCoreIsFreed() throws {
        let (rig, _) = try rig()
        let player = try rig.bridge.makeAudioPlayer(mode: .offline(sampleRate: 44_100, maximumFrames: 256))
        try player.start()
        _ = try player.renderOffline(frames: 256)
        rig.bridge.shutdown()
        // Stopped: nothing more is pulled from the (freed) core.
        let before = rig.fake.audio.renderCount
        #expect(throws: (any Error).self) { _ = try player.renderOffline(frames: 256) }
        #expect(rig.fake.audio.renderCount == before)
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
    }

    @Test func tapsReachTheHandlerWithTheirTimes() throws {
        let blocks = Box<[(UInt64, UInt64, Int64?, Bool, Int)]>([])
        let created = Box<FakeCore?>(nil)
        let bridge = try CoreBridge(
            factory: FakeCore.factory(options: FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])) {
                created.set($0)
            },
            onAudioTap: { block, tile in
                blocks.update {
                    $0.append((tile, block.firstSample, block.cameraTimeMicros, block.discontinuity, block.samples.count))
                }
            })
        defer { bridge.shutdown() }
        let fake = created.get()!
        let device = try bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "x")
        let tile = try bridge.openTile(device: device, channel: 0, tier: .main)
        #expect(fake.pumpAudio(tile: tile.id, count: 3) == 0, "not tapped yet")
        try bridge.setTileAudioTap(tile, enabled: true)
        #expect(fake.pumpAudio(tile: tile.id, count: 3) == 3)
        try bridge.setTileAudioTap(tile, enabled: false)
        #expect(fake.pumpAudio(tile: tile.id, count: 3) == 0)
        let got = blocks.get()
        #expect(got.map(\.0) == [tile.id, tile.id, tile.id])
        #expect(got.map(\.1) == [0, 1024, 2048])
        #expect(got.map(\.2) == [0, 64_000, 128_000])
        #expect(got.map(\.3) == [true, false, false])
        #expect(got.allSatisfy { $0.4 == 1024 })
        #expect(fake.violations.isEmpty, "\(fake.violations)")
    }

    @Test func audioStatsParseAndShowInTheStatusLine() throws {
        // Every field the core sends (docs/FFI.md), including "channels" as a number.
        let json = #"{"type":"audio_stats","tile":7,"records":386,"bytes":26722,"decoded":386,"concealed":0,"rejected":0,"talkback_ignored":0,"epoch":0,"audible":true,"volume":1,"tapped":false,"no_free_slot":false,"playout_delay_ms":223.76,"outliers":0,"timestamp_resets":0,"codec":"aac","sample_rate":16000,"channels":1,"rate_estimate":15999.93,"playout_delay_target_ms":100,"av_offset_ms":0.092,"buffered_ms":199.104,"correction_ppm":1.819,"resyncs":1,"starts":1,"underruns":2,"underrun_frames":0,"overflow_frames":0,"blocks_not_played":0,"tap_dropped":0,"new":[1]}"#
        let event = try #require(CoreEvent.parse(Data(json.utf8)))
        guard case .audioStats(let tile, let a) = event else {
            Issue.record("parsed as \(event)")
            return
        }
        #expect(tile == 7 && a.records == 386 && a.audible && a.codec == "aac")
        #expect(a.playoutDelayMilliseconds == 223.76 && a.avOffsetMilliseconds == 0.092)
        #expect(a.underruns == 2 && a.resyncs == 1)
        var status = ConnectionStatus()
        status.apply(.deviceState(device: 1, state: .connected))
        status.apply(event)
        #expect(status.text == "Connected, listening (audio 224 ms behind)")
    }

    @Test func hostClockIsMachAbsoluteTimeInNanoseconds() {
        let a = HostClock.nowNanos()
        let ticks = mach_absolute_time()
        let b = HostClock.nowNanos()
        let n = HostClock.nanos(hostTicks: ticks)
        #expect(a <= n && n <= b)
        // AudioToolbox's own conversion agrees.
        let viaCoreAudio = AudioConvertHostTimeToNanos(ticks)
        #expect(abs(Int64(bitPattern: viaCoreAudio &- n)) < 1_000)
    }
}
