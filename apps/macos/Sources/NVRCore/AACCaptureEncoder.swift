import AVFoundation
import Foundation
import Synchronization

/// The app's AAC-LC encoder for recordings (docs/CAPTURE.md "Encoding";
/// docs/FFI.md "Capture"): the core hands it 1024-sample frames of mono
/// float PCM for each track that needs encoding, and it answers each AAC
/// access unit, in order, through `answer` (`CoreAPI.recordAudioEncoded`).
///
/// Built on `AVAudioConverter` (AudioToolbox's AudioConverter underneath):
/// one converter per open track. `open` reports the AudioSpecificConfig
/// (written from the format, two bytes for AAC-LC) and the encoder's delay
/// (`kAudioConverterPrimeInfo`'s leading frames, about 2112), which the core
/// writes as the edit list.
///
/// Threads: the core calls it on its recorders' threads, never at once for
/// one recording; different recordings may call at once. Each frame is
/// encoded on the calling thread (a frame takes well under a millisecond),
/// so the units come back in order with no queue of their own; the core
/// bounds what waits for the encoder (10 s per track). The units are
/// answered after the lock is released. At most `maxTracks` tracks are open
/// at a time; more are refused.
public final class AACCaptureEncoder: CaptureEncoder, @unchecked Sendable {
    /// Answers one encoded unit; returns the core's status.
    public typealias Answer = @Sendable (_ recording: UInt64, _ track: UInt32, _ unit: UnsafeRawBufferPointer) -> CoreStatus

    public static let maxTracks = 32
    public static let frameSamples = 1024

    private struct Key: Hashable {
        var recording: UInt64
        var track: UInt32
    }

    /// One track's converter and its formats. Used under the lock only.
    private final class Track {
        let converter: AVAudioConverter
        let input: AVAudioFormat
        let output: AVAudioFormat
        let maxPacket: Int

        init(converter: AVAudioConverter, input: AVAudioFormat, output: AVAudioFormat) {
            self.converter = converter
            self.input = input
            self.output = output
            maxPacket = max(converter.maximumOutputPacketSize, 1536)
        }
    }

    private let answer: Answer
    /// The open tracks, under `lock`.
    private let lock = NSLock()
    private var open: [Key: Track] = [:]
    /// Units the core refused (anything but `.ok`), for diagnostics.
    private let refusedCount = Atomic<Int>(0)
    /// Units answered in all.
    private let answeredCount = Atomic<Int>(0)

    public init(answer: @escaping Answer) {
        self.answer = answer
    }

    public var refused: Int { refusedCount.load(ordering: .relaxed) }
    public var answered: Int { answeredCount.load(ordering: .relaxed) }
    public var openTracks: Int { lock.withLock { open.count } }

    // MARK: CaptureEncoder

    public func open(recording: UInt64, track: UInt32, sampleRate: UInt32, channels: UInt32, bitRate: UInt32)
        -> EncoderOpened?
    {
        guard channels == 1, let asc = Self.audioSpecificConfig(sampleRate: sampleRate, channels: channels),
            let input = AVAudioFormat(
                commonFormat: .pcmFormatFloat32, sampleRate: Double(sampleRate), channels: 1, interleaved: false)
        else { return nil }
        var description = AudioStreamBasicDescription(
            mSampleRate: Double(sampleRate), mFormatID: kAudioFormatMPEG4AAC,
            mFormatFlags: AudioFormatFlags(MPEG4ObjectID.AAC_LC.rawValue), mBytesPerPacket: 0,
            mFramesPerPacket: UInt32(Self.frameSamples), mBytesPerFrame: 0, mChannelsPerFrame: 1, mBitsPerChannel: 0,
            mReserved: 0)
        guard let output = AVAudioFormat(streamDescription: &description),
            let converter = AVAudioConverter(from: input, to: output)
        else { return nil }
        if bitRate > 0 {
            // The nearest rate the encoder supports.
            let allowed = converter.applicableEncodeBitRates?.map(\.intValue) ?? []
            converter.bitRate = allowed.min { abs($0 - Int(bitRate)) < abs($1 - Int(bitRate)) } ?? Int(bitRate)
        }
        let priming = UInt32(max(converter.primeInfo.leadingFrames, 0))
        let added = lock.withLock { () -> Bool in
            let key = Key(recording: recording, track: track)
            guard open[key] != nil || open.count < Self.maxTracks else { return false }
            open[key] = Track(converter: converter, input: input, output: output)
            return true
        }
        return added ? EncoderOpened(audioSpecificConfig: asc, primingFrames: priming) : nil
    }

    public func encode(recording: UInt64, track: UInt32, pcm: UnsafeBufferPointer<Float>) {
        let units = lock.withLock { () -> [Data] in
            guard let tr = open[Key(recording: recording, track: track)],
                let buffer = AVAudioPCMBuffer(pcmFormat: tr.input, frameCapacity: AVAudioFrameCount(pcm.count)),
                let channel = buffer.floatChannelData?[0]
            else { return [] }
            if let base = pcm.baseAddress { channel.update(from: base, count: pcm.count) }
            buffer.frameLength = AVAudioFrameCount(pcm.count)
            var given = false
            return Self.drain(tr) { _, status in
                if given {
                    status.pointee = .noDataNow
                    return nil
                }
                given = true
                status.pointee = .haveData
                return buffer
            }
        }
        send(units, recording, track)
    }

    public func close(recording: UInt64, track: UInt32, flush: Bool) {
        let units = lock.withLock { () -> [Data] in
            guard let tr = open.removeValue(forKey: Key(recording: recording, track: track)), flush else { return [] }
            return Self.drain(tr) { _, status in
                status.pointee = .endOfStream
                return nil
            }
        }
        send(units, recording, track)
    }

    // MARK: Pieces

    private func send(_ units: [Data], _ recording: UInt64, _ track: UInt32) {
        for unit in units {
            let status = unit.withUnsafeBytes { answer(recording, track, $0) }
            answeredCount.add(1, ordering: .relaxed)
            if status != .ok { refusedCount.add(1, ordering: .relaxed) }
        }
    }

    /// Converts until the converter wants more input or the stream ended,
    /// collecting every packet it produced.
    private static func drain(_ tr: Track, _ input: @escaping AVAudioConverterInputBlock) -> [Data] {
        var units: [Data] = []
        while true {
            let out = AVAudioCompressedBuffer(format: tr.output, packetCapacity: 8, maximumPacketSize: tr.maxPacket)
            var error: NSError?
            let status = tr.converter.convert(to: out, error: &error, withInputFrom: input)
            if status == .error { return units }
            if let descriptions = out.packetDescriptions {
                for i in 0..<Int(out.packetCount) {
                    let d = descriptions[i]
                    guard d.mDataByteSize > 0 else { continue }
                    units.append(Data(bytes: out.data.advanced(by: Int(d.mStartOffset)), count: Int(d.mDataByteSize)))
                }
            }
            // `.haveData`: the buffer filled up and more may follow.
            if status != .haveData || out.packetCount == 0 { return units }
        }
    }

    /// The two-byte AudioSpecificConfig of AAC-LC (ISO/IEC 14496-3): object
    /// type 2, the sampling frequency index, the channel configuration. Nil
    /// for a rate without an index.
    public static func audioSpecificConfig(sampleRate: UInt32, channels: UInt32) -> [UInt8]? {
        let rates: [UInt32] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]
        guard let index = rates.firstIndex(of: sampleRate), (1...7).contains(channels) else { return nil }
        let objectType: UInt16 = 2
        let bits = objectType << 11 | UInt16(index) << 7 | UInt16(channels) << 3
        return [UInt8(bits >> 8), UInt8(bits & 0xFF)]
    }

    /// A 7-byte ADTS header for one AAC-LC unit of `length` bytes (for
    /// reading units back with the system's decoders in tests).
    public static func adtsHeader(length: Int, sampleRate: UInt32, channels: UInt32) -> [UInt8] {
        let rates: [UInt32] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]
        let index = UInt8(rates.firstIndex(of: sampleRate) ?? 3)
        let full = length + 7
        let profile: UInt8 = 1  // AAC-LC, object type minus one
        return [
            0xFF, 0xF1,
            profile << 6 | index << 2 | UInt8((channels >> 2) & 1),
            UInt8((channels & 3) << 6) | UInt8((full >> 11) & 0x3),
            UInt8((full >> 3) & 0xFF),
            UInt8((full & 0x7) << 5) | 0x1F,
            0xFC,
        ]
    }
}
