import AVFoundation
import NVRCore
import Testing

/// The typed reply's speech collection: one result at the first end, however
/// many ends the synthesizer sends.
@Suite struct SpeechBufferCollectorTests {
    private func buffer(frames: AVAudioFrameCount) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(standardFormatWithSampleRate: 22_050, channels: 1)!
        let b = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: max(frames, 1))!
        b.frameLength = frames
        if frames > 0, let ch = b.floatChannelData?[0] {
            for i in 0..<Int(frames) { ch[i] = sin(Float(i) * 0.05) * 0.5 }
        }
        return b
    }

    @Test func onlyTheFirstEndYieldsTheSamples() {
        let c = SpeechBufferCollector()
        #expect(c.take(buffer(frames: 2205)) == nil)
        #expect(c.take(buffer(frames: 2205)) == nil)
        let samples = c.take(buffer(frames: 0))
        // 0.2 s at 22.05 kHz is about 3200 samples at 16 kHz.
        #expect((3000...3400).contains(samples?.count ?? 0))
        // The synthesizer was seen sending the end again: nothing more.
        #expect(c.take(buffer(frames: 0)) == nil)
        #expect(c.take(buffer(frames: 2205)) == nil)
    }

    /// The system voice itself, rendered to buffers as the typed reply does:
    /// the continuation must resume exactly once.
    @Test @MainActor func theSystemVoiceResumesOnce() async {
        let synthesizer = AVSpeechSynthesizer()
        let utterance = AVSpeechUtterance(string: "Hello, one moment please")
        let collected = SpeechBufferCollector()
        let samples = await withCheckedContinuation { (done: CheckedContinuation<[Float], Never>) in
            synthesizer.write(utterance) { buffer in
                guard let pcm = buffer as? AVAudioPCMBuffer else { return }
                if let s = collected.take(pcm) { done.resume(returning: s) }
            }
        }
        #expect(samples.count > 8_000)
        // Late callbacks after the end must not resume again (a second resume traps).
        try? await Task.sleep(for: .milliseconds(300))
    }
}

/// The microphone tap's block runs on an audio thread: built outside any
/// actor, it must run there without stopping the app.
@Suite struct MicrophoneTapTests {
    @Test func theTapBlockRunsOnABackgroundThread() async {
        let got = SampleCount()
        let block = TalkAudioConverter.tapBlock { got.add($0.count) }
        let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 1)!
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            DispatchQueue.global(qos: .userInteractive).async {
                for _ in 0..<10 {
                    let b = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4_800)!
                    b.frameLength = 4_800
                    block(b, AVAudioTime(hostTime: 0))
                }
                done.resume()
            }
        }
        // 1 s at 48 kHz is about 16000 samples at 16 kHz (less the resampler's tail).
        #expect((15_000...16_100).contains(got.total))
    }
}

private final class SampleCount: @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    func add(_ k: Int) { lock.withLock { n += k } }
    var total: Int { lock.withLock { n } }
}
