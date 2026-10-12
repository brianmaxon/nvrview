import AVFoundation
import Foundation
import NVRCore
import Synchronization
import Testing

/// The recordings' AAC encoder (docs/CAPTURE.md "Encoding"): a synthetic
/// tone through it, its units read back by the system's AAC decoder (as an
/// ADTS file), and the decoded signal compared with the input: where the
/// tone starts (the reported priming), its level and its pitch.
@Suite struct AACCaptureEncoderTests {
    private final class Units: Sendable {
        let units = Mutex<[UInt32: [Data]]>([:])
        func add(_ track: UInt32, _ unit: UnsafeRawBufferPointer) {
            units.withLock { $0[track, default: []].append(Data(unit)) }
        }
    }

    @Test func audioSpecificConfigOfAACLC() {
        #expect(AACCaptureEncoder.audioSpecificConfig(sampleRate: 48_000, channels: 1) == [0x11, 0x88])
        #expect(AACCaptureEncoder.audioSpecificConfig(sampleRate: 44_100, channels: 2) == [0x12, 0x10])
        #expect(AACCaptureEncoder.audioSpecificConfig(sampleRate: 1234, channels: 1) == nil)
    }

    @Test func toneRoundTrip() throws {
        let sink = Units()
        let encoder = AACCaptureEncoder { _, track, unit in
            sink.add(track, unit)
            return .ok
        }
        let opened = try #require(encoder.open(recording: 1, track: 2, sampleRate: 48_000, channels: 1, bitRate: 96_000))
        #expect(opened.audioSpecificConfig == [0x11, 0x88])
        print("AAC encoder priming: \(opened.primingFrames) frames")
        #expect((1024...4096).contains(opened.primingFrames))

        // 4096 samples of silence, then a 1 kHz tone at half scale.
        let silent = 4096, frames = 60
        var pcm = [Float](repeating: 0, count: frames * 1024)
        for i in silent..<pcm.count { pcm[i] = 0.5 * sin(2 * .pi * 1000 * Float(i - silent) / 48_000) }
        for f in 0..<frames {
            pcm[(f * 1024)..<((f + 1) * 1024)].withUnsafeBufferPointer {
                encoder.encode(recording: 1, track: 2, pcm: $0)
            }
        }
        encoder.close(recording: 1, track: 2, flush: true)
        #expect(encoder.openTracks == 0 && encoder.refused == 0)
        let units = sink.units.withLock { $0[2] ?? [] }
        // Every frame comes back, and the flush returns the delayed rest.
        #expect(units.count >= frames && units.count <= frames + 4, "\(units.count) units for \(frames) frames")

        let decoded = try decode(units)
        #expect(decoded.count >= frames * 1024)
        let onset = try #require(decoded.firstIndex { abs($0) > 0.1 })
        print("AAC round trip: tone starts at \(onset), \(silent) + priming \(opened.primingFrames)")
        #expect(abs(onset - (silent + Int(opened.primingFrames))) <= 48)

        // The steady part: the level and the pitch of the input.
        let from = onset + 4800, to = min(decoded.count, onset + 4800 + 24_000)
        let steady = Array(decoded[from..<to])
        let rms = sqrt(steady.reduce(0) { $0 + $1 * $1 } / Float(steady.count))
        #expect(abs(rms - 0.5 / sqrt(2)) < 0.05, "rms \(rms)")
        var crossings = 0
        for i in 1..<steady.count where (steady[i - 1] < 0) != (steady[i] < 0) { crossings += 1 }
        let hz = Double(crossings) / 2 / (Double(steady.count) / 48_000)
        #expect(abs(hz - 1000) < 10, "\(hz) Hz")
    }

    @Test func tracksAreIndependentAndBounded() {
        let encoder = AACCaptureEncoder { _, _, _ in .ok }
        for t in 0..<UInt32(AACCaptureEncoder.maxTracks) {
            #expect(encoder.open(recording: 9, track: t, sampleRate: 48_000, channels: 1, bitRate: 96_000) != nil)
        }
        #expect(encoder.open(recording: 10, track: 0, sampleRate: 48_000, channels: 1, bitRate: 96_000) == nil)
        #expect(encoder.open(recording: 10, track: 0, sampleRate: 48_000, channels: 2, bitRate: 96_000) == nil)
        for t in 0..<UInt32(AACCaptureEncoder.maxTracks) { encoder.close(recording: 9, track: t, flush: false) }
        #expect(encoder.openTracks == 0)
        // A frame for a track that is not open is ignored.
        [Float](repeating: 0, count: 1024).withUnsafeBufferPointer { encoder.encode(recording: 9, track: 0, pcm: $0) }
        #expect(encoder.answered == 0)
    }

    /// The units as an ADTS file, read with the system's decoder.
    private func decode(_ units: [Data]) throws -> [Float] {
        var file = Data()
        for u in units {
            file.append(contentsOf: AACCaptureEncoder.adtsHeader(length: u.count, sampleRate: 48_000, channels: 1))
            file.append(u)
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-aac-\(UUID()).aac")
        try file.write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let f = try AVAudioFile(forReading: url, commonFormat: .pcmFormatFloat32, interleaved: false)
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: f.processingFormat, frameCapacity: AVAudioFrameCount(f.length + 4096)))
        try f.read(into: buffer)
        let data = try #require(buffer.floatChannelData)
        return Array(UnsafeBufferPointer(start: data[0], count: Int(buffer.frameLength)))
    }
}
