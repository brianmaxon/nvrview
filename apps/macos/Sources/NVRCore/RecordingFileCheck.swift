import AVFoundation
import Foundation

/// What a finished recording holds, read back with AVFoundation (the
/// self-test's `capture-*-real-core` steps): its tracks, which audio tracks
/// are enabled, its duration, whether the video starts on a keyframe, and
/// how much of each audio track the system's AAC decoder decodes.
public struct RecordingFileCheck: Sendable {
    public struct Audio: Sendable {
        public var enabled: Bool
        public var codec: FourCharCode
        public var sampleRate: Double
        /// Frames decoded to PCM (0: it did not decode).
        public var decodedFrames: Int
        /// The decoded audio's peak (0 for silence).
        public var peak: Float
    }

    public var videoTracks: Int
    public var audio: [Audio]
    public var duration: Double
    /// The first video sample is a sync sample.
    public var startsOnKeyframe: Bool

    /// Reads the file at `url`.
    public static func inspect(_ url: URL) async throws -> RecordingFileCheck {
        let asset = AVURLAsset(url: url)
        let tracks = try await asset.load(.tracks)
        let duration = try await asset.load(.duration).seconds
        var videos = 0
        var audio: [Audio] = []
        var keyframe = false
        for track in tracks {
            let type = track.mediaType
            if type == .video {
                videos += 1
                if videos == 1 { keyframe = try firstSampleIsSync(asset, track) }
            } else if type == .audio {
                let enabled = try await track.load(.isEnabled)
                let format = try await track.load(.formatDescriptions).first
                let asbd = format.flatMap { CMAudioFormatDescriptionGetStreamBasicDescription($0)?.pointee }
                let (frames, peak) = try decode(asset, track)
                audio.append(
                    Audio(
                        enabled: enabled, codec: asbd?.mFormatID ?? 0, sampleRate: asbd?.mSampleRate ?? 0,
                        decodedFrames: frames, peak: peak))
            }
        }
        return RecordingFileCheck(videoTracks: videos, audio: audio, duration: duration, startsOnKeyframe: keyframe)
    }

    private static func firstSampleIsSync(_ asset: AVAsset, _ track: AVAssetTrack) throws -> Bool {
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: nil)
        reader.add(output)
        guard reader.startReading(), let sample = output.copyNextSampleBuffer() else { return false }
        reader.cancelReading()
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        return (attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true
    }

    private static func decode(_ asset: AVAsset, _ track: AVAssetTrack) throws -> (Int, Float) {
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [
                AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 32, AVLinearPCMIsFloatKey: true,
                AVLinearPCMIsNonInterleaved: false, AVLinearPCMIsBigEndianKey: false,
            ])
        reader.add(output)
        guard reader.startReading() else { return (0, 0) }
        var frames = 0
        var peak: Float = 0
        while let sample = output.copyNextSampleBuffer() {
            frames += CMSampleBufferGetNumSamples(sample)
            guard let block = CMSampleBufferGetDataBuffer(sample) else { continue }
            var length = 0
            var pointer: UnsafeMutablePointer<CChar>?
            guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer) == noErr,
                let pointer
            else { continue }
            pointer.withMemoryRebound(to: Float.self, capacity: length / 4) { p in
                for i in 0..<(length / 4) { peak = max(peak, abs(p[i])) }
            }
        }
        return (reader.status == .failed ? 0 : frames, peak)
    }
}
