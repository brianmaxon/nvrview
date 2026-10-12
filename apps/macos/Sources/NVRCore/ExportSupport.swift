import AVFoundation
import Foundation

/// Cancel and progress of a running export, shared between the writer's
/// queue and the screen.
public final class ExportControl: @unchecked Sendable {
    private let lock = NSLock()
    private var canceledFlag = false
    private var fractionValue = 0.0

    public init() {}

    public func cancel() { lock.withLock { canceledFlag = true } }
    public var isCanceled: Bool { lock.withLock { canceledFlag } }

    /// 0...1.
    public var fraction: Double {
        get { lock.withLock { fractionValue } }
        set { lock.withLock { fractionValue = min(max(newValue, 0), 1) } }
    }
}

/// Where an export is written and how it is finished: into a hidden file
/// next to the destination, which replaces the destination only when the
/// export is complete (a canceled or failed export leaves nothing, and a
/// file the user agreed to replace stays until then).
enum ExportFile {
    static func partial(for url: URL) -> URL {
        url.deletingLastPathComponent().appendingPathComponent(".nvrview-export-\(UUID().uuidString).mp4")
    }

    /// Moves the finished partial file over `url`; returns its size.
    static func commit(_ partial: URL, to url: URL, shareSafe: Bool) throws -> Int64 {
        if shareSafe { try MP4ShareSafe.apply(to: partial) }
        let manager = FileManager.default
        if manager.fileExists(atPath: url.path) {
            _ = try manager.replaceItemAt(url, withItemAt: partial)
        } else {
            try manager.moveItem(at: partial, to: url)
        }
        return (try? manager.attributesOfItem(atPath: url.path)[.size] as? Int64) ?? 0
    }

    static func discard(_ partial: URL) { try? FileManager.default.removeItem(at: partial) }
}

/// The video codecs an export writes (both through VideoToolbox).
public enum ExportCodec: String, Sendable, CaseIterable {
    case h264
    case hevc

    var avCodec: AVVideoCodecType { self == .h264 ? .h264 : .hevc }

    /// About a tenth of a bit per pixel and frame (6 Mbit/s for 1080p30).
    func bitrate(width: Int, height: Int, fps: Int) -> Int {
        max(500_000, Int(Double(width * height * fps) * (self == .h264 ? 0.1 : 0.07)))
    }

    /// Compression settings for a writer input.
    func settings(width: Int, height: Int, fps: Int) -> [String: Any] {
        var compression: [String: Any] = [
            AVVideoAverageBitRateKey: bitrate(width: width, height: height, fps: fps),
            AVVideoMaxKeyFrameIntervalKey: fps * 2,
            AVVideoExpectedSourceFrameRateKey: fps,
            // No reordering: the first picture is at time zero with no edit
            // list, as in the files the core writes.
            AVVideoAllowFrameReorderingKey: false,
        ]
        if self == .h264 { compression[AVVideoProfileLevelKey] = AVVideoProfileLevelH264HighAutoLevel }
        return [
            AVVideoCodecKey: avCodec, AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: compression,
            AVVideoColorPropertiesKey: [
                AVVideoColorPrimariesKey: AVVideoColorPrimaries_ITU_R_709_2,
                AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
                AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_709_2,
            ],
        ]
    }
}

enum ExportAudio {
    /// A reader output and writer input that carry `track` into an MP4: the
    /// samples as they are when they are AAC (which the container takes),
    /// else decoded and written as AAC. Nil when the writer takes neither.
    static func pair(for track: AVAssetTrack, reader: AVAssetReader, writer: AVAssetWriter) async
        -> (output: AVAssetReaderOutput, input: AVAssetWriterInput)?
    {
        let hint = (try? await track.load(.formatDescriptions))?.first
        if let hint, CMFormatDescriptionGetMediaSubType(hint) == kAudioFormatMPEG4AAC {
            let copy = AVAssetWriterInput(mediaType: .audio, outputSettings: nil, sourceFormatHint: hint)
            let direct = AVAssetReaderTrackOutput(track: track, outputSettings: nil)
            if writer.canAdd(copy), reader.canAdd(direct) { return (direct, copy) }
        }
        let decoded = AVAssetReaderTrackOutput(track: track, outputSettings: pcm)
        let aac = AVAssetWriterInput(mediaType: .audio, outputSettings: aacSettings)
        guard writer.canAdd(aac), reader.canAdd(decoded) else { return nil }
        return (decoded, aac)
    }

    private static var stereo: Data {
        var layout = AudioChannelLayout()
        layout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo
        return Data(bytes: &layout, count: MemoryLayout<AudioChannelLayout>.size)
    }

    static var pcm: [String: Any] { [
        AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 48_000, AVNumberOfChannelsKey: 2,
        AVLinearPCMBitDepthKey: 16, AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false,
        AVLinearPCMIsNonInterleaved: false,
        AVChannelLayoutKey: stereo,
    ] }
    static var aacSettings: [String: Any] { [
        AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 48_000, AVNumberOfChannelsKey: 2, AVEncoderBitRateKey: 128_000,
        AVChannelLayoutKey: stereo,
    ] }
}

/// Copies sample buffers from a reader output to a writer input on `queue`
/// while the input wants more; `finish` runs once, when the output is
/// drained, the export is canceled, or the writer failed.
final class SampleCopier: @unchecked Sendable {
    private let output: AVAssetReaderOutput
    private let input: AVAssetWriterInput
    private let control: ExportControl
    private let queue: DispatchQueue
    private var done = false
    private let onSample: (@Sendable (CMSampleBuffer) -> Void)?

    init(
        output: AVAssetReaderOutput, input: AVAssetWriterInput, control: ExportControl, queue: DispatchQueue,
        onSample: (@Sendable (CMSampleBuffer) -> Void)? = nil
    ) {
        self.output = output
        self.input = input
        self.control = control
        self.queue = queue
        self.onSample = onSample
    }

    func run(finish: @escaping @Sendable () -> Void) {
        input.requestMediaDataWhenReady(on: queue) { [self] in
            while input.isReadyForMoreMediaData, !done {
                guard !control.isCanceled, let sample = output.copyNextSampleBuffer() else {
                    done = true
                    input.markAsFinished()
                    finish()
                    return
                }
                onSample?(sample)
                if !input.append(sample) {
                    done = true
                    input.markAsFinished()
                    finish()
                    return
                }
            }
        }
    }
}
