import AVFoundation
import Foundation

/// Turns audio buffers of any format (the microphone's, or text-to-speech
/// rendered by `AVSpeechSynthesizer.write(_:toBufferCallback:)`) into 16 kHz
/// mono float samples: what talk-back pushes (`nv_talk_push`) and what the
/// transcription engines take.
///
/// Streaming: one converter per input format keeps the resampler's state
/// between buffers, so consecutive buffers join without clicks. Not thread
/// safe; use it from one thread (or queue) at a time.
public final class TalkAudioConverter {
    public static let sampleRate = 16_000.0

    private var converter: AVAudioConverter?
    private var inputFormat: AVAudioFormat?
    private let output: AVAudioFormat

    public init() {
        output = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: Self.sampleRate, channels: 1, interleaved: false)!
    }

    /// The 16 kHz mono samples of `buffer` (channels mixed down by the
    /// converter). Empty for an empty or unconvertible buffer.
    public func convert(_ buffer: AVAudioPCMBuffer) -> [Float] {
        guard buffer.frameLength > 0 else { return [] }
        if inputFormat != buffer.format {
            inputFormat = buffer.format
            converter = AVAudioConverter(from: buffer.format, to: output)
            // Leading silence for the resampler's latency, so the output
            // has exactly the input's duration.
            converter?.primeMethod = .normal
            // Mixes every input channel into the one output channel.
            if buffer.format.channelCount > 1 { converter?.downmix = true }
        }
        var given = false
        return drain { _, status in
            if given {
                status.pointee = .noDataNow
                return nil
            }
            given = true
            status.pointee = .haveData
            return buffer
        }
    }

    /// The rest held in the resampler at the end of a stream (a typed reply
    /// rendered in one go). The next buffer starts a new stream.
    public func flush() -> [Float] {
        let rest = drain { _, status in
            status.pointee = .endOfStream
            return nil
        }
        converter?.reset()
        return rest
    }

    /// Converts until the converter wants more input (or the stream ended):
    /// a full output buffer means more is waiting.
    private func drain(_ input: @escaping AVAudioConverterInputBlock) -> [Float] {
        guard let converter, let out = AVAudioPCMBuffer(pcmFormat: output, frameCapacity: 4096) else { return [] }
        var samples: [Float] = []
        while true {
            var error: NSError?
            out.frameLength = 0
            let status = converter.convert(to: out, error: &error, withInputFrom: input)
            if status == .error { return samples }
            if let data = out.floatChannelData, out.frameLength > 0 {
                samples.append(contentsOf: UnsafeBufferPointer(start: data[0], count: Int(out.frameLength)))
            }
            // `.haveData`: the output filled up; anything else: done for now.
            if status != .haveData || out.frameLength == 0 { return samples }
        }
    }

    /// A whole recording (several buffers of one stream) at once.
    public static func convertAll(_ buffers: [AVAudioPCMBuffer]) -> [Float] {
        let c = TalkAudioConverter()
        var samples: [Float] = []
        for b in buffers { samples += c.convert(b) }
        return samples + c.flush()
    }
}

/// Collects the buffers `AVSpeechSynthesizer.write(_:toBufferCallback:)`
/// renders, from whatever thread it calls back on. The synthesizer marks the
/// end with a zero-length buffer and was seen sending that more than once, so
/// only the first end yields the samples: a continuation resumed twice is a
/// fatal error.
public final class SpeechBufferCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var buffers: [AVAudioPCMBuffer] = []
    private var finished = false

    public init() {}

    /// Takes one rendered buffer. Returns the 16 kHz mono samples of all of
    /// them at the first end (a zero-length buffer), and nil otherwise:
    /// before the end, and at any end after the first.
    public func take(_ buffer: AVAudioPCMBuffer) -> [Float]? {
        lock.withLock {
            guard !finished else { return nil }
            guard buffer.frameLength == 0 else {
                buffers.append(buffer)
                return nil
            }
            finished = true
            return TalkAudioConverter.convertAll(buffers)
        }
    }

    /// The synthesizer's buffer callback, built outside any actor (it may be
    /// called on any thread): `done` runs once, with the samples, at the
    /// first end.
    public nonisolated func callback(
        _ done: @escaping @Sendable ([Float]) -> Void
    ) -> @Sendable (AVAudioBuffer) -> Void {
        { [self] buffer in
            guard let pcm = buffer as? AVAudioPCMBuffer, let samples = take(pcm) else { return }
            done(samples)
        }
    }
}

extension TalkAudioConverter {
    /// The microphone tap's block: converts each buffer to 16 kHz mono and
    /// passes non-empty blocks to `onSamples`. AVAudioEngine calls a tap on
    /// an audio thread, so the block is built here, outside any actor: a
    /// block made inside main-actor code would be main-actor isolated, and
    /// Swift 6 stops the app when it runs on another thread.
    ///
    /// `firstChannelOnly`: only the first channel is converted (the input
    /// of voice processing reports several; see `AudioPlayer`).
    public nonisolated static func tapBlock(
        _ onSamples: @escaping @Sendable ([Float]) -> Void, firstChannelOnly: Bool = false
    ) -> @Sendable (AVAudioPCMBuffer, AVAudioTime) -> Void {
        let converter = LockedTalkConverter()
        return { buffer, _ in
            let input = firstChannelOnly ? (firstChannel(of: buffer) ?? buffer) : buffer
            let samples = converter.convert(input)
            if !samples.isEmpty { onSamples(samples) }
        }
    }

    /// The first channel of a deinterleaved float buffer with several, as a
    /// mono buffer; nil for any other buffer (used as it is).
    public nonisolated static func firstChannel(of buffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        let f = buffer.format
        guard f.channelCount > 1, f.commonFormat == .pcmFormatFloat32, !f.isInterleaved,
            let data = buffer.floatChannelData,
            let mono = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: f.sampleRate, channels: 1, interleaved: false),
            let out = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: max(buffer.frameLength, 1)),
            let dest = out.floatChannelData
        else { return nil }
        out.frameLength = buffer.frameLength
        dest[0].update(from: data[0], count: Int(buffer.frameLength))
        return out
    }
}

/// A converter used from one audio thread at a time.
final class LockedTalkConverter: @unchecked Sendable {
    private let lock = NSLock()
    private let converter = TalkAudioConverter()
    func convert(_ b: AVAudioPCMBuffer) -> [Float] { lock.withLock { converter.convert(b) } }
}
