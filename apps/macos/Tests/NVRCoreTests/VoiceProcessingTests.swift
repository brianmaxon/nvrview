import AVFoundation
import Foundation
import NVRCore
import Synchronization
import Testing

/// The microphone tap of voice processing takes the first channel only.
@Suite struct VoiceProcessingTapTests {
    @Test func theFirstChannelOfAMultichannelBufferIsKept() throws {
        let layout = try #require(AVAudioChannelLayout(layoutTag: kAudioChannelLayoutTag_DiscreteInOrder | 9))
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, interleaved: false, channelLayout: layout)
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 480))
        buffer.frameLength = 480
        let data = try #require(buffer.floatChannelData)
        for c in 0..<9 {
            for i in 0..<480 { data[c][i] = c == 0 ? 0.25 : 0.9 }
        }
        let mono = try #require(TalkAudioConverter.firstChannel(of: buffer))
        #expect(mono.format.channelCount == 1 && mono.format.sampleRate == 48_000 && mono.frameLength == 480)
        #expect(mono.floatChannelData?[0][479] == 0.25)
        // A mono buffer is used as it is.
        let one = try #require(AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1, interleaved: false))
        #expect(TalkAudioConverter.firstChannel(of: try #require(AVAudioPCMBuffer(pcmFormat: one, frameCapacity: 16))) == nil)
    }

    /// On this Mac's real devices (it opens the microphone and plays
    /// silence): `NVRVIEW_AUDIO_DEVICE_TESTS=1 swift test --filter VoiceProcessing`.
    /// A voice-processing engine is prepared ahead without running the
    /// microphone, Talk switches to it with a short gap in playback, the
    /// microphone delivers 16 kHz blocks, and playback goes back to a plain
    /// engine after.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["NVRVIEW_AUDIO_DEVICE_TESTS"] == "1"))
    func voiceProcessingRunsOnTheOutputEngine() async throws {
        let rates = Mutex<[UInt32]>([])
        let gaps = RenderGaps()
        let player = AudioPlayer(hook: gaps.hook) { rate, _ in
            rates.withLock { $0.append(rate) }
            return .ok
        }
        try player.start()
        player.prepareVoiceProcessing()
        let clock = ContinuousClock()
        let prepareStart = clock.now
        while !player.isVoiceProcessingPrepared && clock.now - prepareStart < .seconds(5) {
            try await Task.sleep(for: .milliseconds(20))
        }
        let prepared = clock.now - prepareStart
        #expect(player.isVoiceProcessingPrepared)
        try await Task.sleep(for: .milliseconds(300))
        let beforeGap = gaps.reset()
        let samples = Atomic<Int>(0)
        func talk() async -> (String??, Duration) {
            let started = AsyncStream<String?>.makeStream()
            let t0 = clock.now
            player.startVoiceProcessing(
                onSamples: { samples.add($0.count, ordering: .relaxed) }, onLost: { _ in },
                done: { started.continuation.yield($0?.description) })
            var outcome: String??
            for await e in started.stream {
                outcome = e
                break
            }
            return (outcome, clock.now - t0)
        }
        let (outcome, switched) = await talk()
        #expect(outcome == .some(nil))
        #expect(player.isVoiceProcessing && player.playsThroughVoiceProcessing)
        try await Task.sleep(for: .seconds(1))
        let heard = samples.load(ordering: .relaxed)
        let onGap = gaps.reset()
        // Released: the microphone stops at once, the engine lingers.
        player.stopVoiceProcessing()
        #expect(!player.isVoiceProcessing && player.playsThroughVoiceProcessing)
        try await Task.sleep(for: .milliseconds(300))
        let (again, retapped) = await talk()
        #expect(again == .some(nil))
        try await Task.sleep(for: .milliseconds(300))
        let lingerGap = gaps.reset()
        // The call ended: back to a plain engine at once.
        player.stopVoiceProcessing()
        player.discardVoiceProcessing()
        while player.playsThroughVoiceProcessing { try await Task.sleep(for: .milliseconds(10)) }
        try await Task.sleep(for: .milliseconds(300))
        let offGap = gaps.reset()
        let plainRate = player.sampleRate
        let configured = rates.withLock { $0 }
        player.stop()
        print(
            "voice processing: prepared in \(prepared), switched in \(switched), talk again while lingering \(retapped); "
                + "\(heard) samples at 16 kHz in 1 s; longest pause in rendering: \(beforeGap) ms while preparing, "
                + "\(onGap) ms switching on, \(lingerGap) ms talking again, \(offGap) ms switching off; "
                + "output rates \(configured), back to \(plainRate) Hz")
        #expect(heard > 12_000)
        #expect(onGap < 400 && lingerGap < 50 && offGap < 2_000)
        #expect(plainRate > 0 && configured.count == 3)
    }
}

/// A render hook that records the longest pause between render calls.
final class RenderGaps: @unchecked Sendable {
    /// [last call, longest pause] in host ticks; written by the render thread.
    private let state = UnsafeMutablePointer<UInt64>.allocate(capacity: 2)

    init() { state.initialize(repeating: 0, count: 2) }
    deinit { state.deallocate() }

    var hook: AudioRenderHook {
        AudioRenderHook(
            function: { context, out, frames, channels, _ in
                out?.update(repeating: 0, count: Int(frames) * Int(channels))
                guard let s = context?.assumingMemoryBound(to: UInt64.self) else { return 0 }
                let now = mach_absolute_time()
                if s[0] != 0 { s[1] = max(s[1], now - s[0]) }
                s[0] = now
                return 0
            }, context: UnsafeMutableRawPointer(state))
    }

    /// The longest pause since the last reset, in ms.
    func reset() -> Int {
        let ticks = state[1]
        state[1] = 0
        var tb = mach_timebase_info_data_t()
        mach_timebase_info(&tb)
        return Int(ticks * UInt64(tb.numer) / UInt64(tb.denom) / 1_000_000)
    }
}
