import AVFoundation
import CoreVideo
import Foundation

/// Cuts one downloaded camera file to a trim plan's output
/// (FFI-RECORDINGS.md section 7a (a)).
///
/// - **Keyframe copy** (`copy_possible` and not `exact`): the compressed
///   samples are remuxed untouched, so it is instant and loses nothing; the
///   output starts at the keyframe the core chose, at or before the in point.
/// - **Exact**: the whole output is decoded and encoded again with
///   VideoToolbox (H.264 or HEVC like the source), so it starts at the
///   picture asked for. Only the first group of pictures needs re-encoding
///   in principle; the whole clip is simpler and a trim is short, and the
///   cost is one generation of quality loss at a bit rate generous enough
///   for it (`ExportCodec.bitrate`). Audio is copied (AAC) or converted to AAC.
///
/// The work runs on its own queues, a sample at a time (memory stays
/// bounded), and `control` cancels it; a canceled or failed export leaves
/// no file.
public enum TrimExporter {
    public static func export(
        _ output: TrimPlan.Output, exact: Bool, to url: URL, shareSafe: Bool, control: ExportControl
    ) async -> ExportOutcome {
        let partial = ExportFile.partial(for: url)
        func fail(_ why: String) -> ExportOutcome {
            ExportFile.discard(partial)
            return ExportOutcome(result: .failed(why), url: url)
        }
        let asset = AVURLAsset(url: URL(fileURLWithPath: output.file))
        guard let video = try? await asset.loadTracks(withMediaType: .video).first,
            let (size, formats) = try? await video.load(.naturalSize, .formatDescriptions)
        else { return fail("The downloaded file has no video") }
        let audio = (try? await asset.loadTracks(withMediaType: .audio))?.first
        let width = Int(size.width.rounded()), height = Int(size.height.rounded())
        let start = CMTime(value: output.sourceInMs, timescale: 1000), end = CMTime(value: output.sourceOutMs, timescale: 1000)
        let copy = !exact && output.copyPossible
        do {
            let reader = try AVAssetReader(asset: asset)
            reader.timeRange = CMTimeRange(start: start, end: end)
            let writer = try AVAssetWriter(outputURL: partial, fileType: .mp4)
            writer.shouldOptimizeForNetworkUse = false
            var pairs: [(AVAssetReaderOutput, AVAssetWriterInput, isVideo: Bool)] = []
            let source = formats.first
            if copy {
                let o = AVAssetReaderTrackOutput(track: video, outputSettings: nil)
                let i = AVAssetWriterInput(mediaType: .video, outputSettings: nil, sourceFormatHint: source)
                pairs.append((o, i, true))
            } else {
                let o = AVAssetReaderTrackOutput(
                    track: video,
                    outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange])
                let codec: ExportCodec = source.map { CMFormatDescriptionGetMediaSubType($0) == kCMVideoCodecType_HEVC } == true ? .hevc : .h264
                let fps = max(1, Int((try? await video.load(.nominalFrameRate))?.rounded() ?? 30))
                let i = AVAssetWriterInput(mediaType: .video, outputSettings: codec.settings(width: width, height: height, fps: fps))
                pairs.append((o, i, true))
            }
            if let audio, let a = await ExportAudio.pair(for: audio, reader: reader, writer: writer) {
                pairs.append((a.output, a.input, false))
            }
            for (o, i, _) in pairs {
                guard reader.canAdd(o), writer.canAdd(i) else { return fail("The cut could not be set up") }
                reader.add(o)
                writer.add(i)
            }
            guard writer.startWriting() else { return fail("The file could not be written: \(writer.error?.localizedDescription ?? "?")") }
            guard reader.startReading() else {
                writer.cancelWriting()
                return fail("The downloaded file could not be read: \(reader.error?.localizedDescription ?? "?")")
            }
            writer.startSession(atSourceTime: start)
            let counted = Counter()
            let group = DispatchGroup()
            let span = max(Double(output.sourceOutMs - output.sourceInMs), 1)
            for (o, i, isVideo) in pairs {
                group.enter()
                let queue = DispatchQueue(label: "nvrview.trim.\(isVideo ? "video" : "audio")", qos: .userInitiated)
                let sampleSeen: (@Sendable (CMSampleBuffer) -> Void)?
                if isVideo {
                    let first = output.sourceInMs
                    sampleSeen = { sample in
                        counted.add()
                        let ms = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample)) * 1000
                        control.fraction = (ms - Double(first)) / span
                    }
                } else {
                    sampleSeen = nil
                }
                SampleCopier(output: o, input: i, control: control, queue: queue, onSample: sampleSeen).run { group.leave() }
            }
            await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in group.notify(queue: .global()) { c.resume() } }
            if control.isCanceled {
                reader.cancelReading()
                writer.cancelWriting()
                ExportFile.discard(partial)
                return ExportOutcome(result: .canceled, url: url)
            }
            if reader.status == .failed || writer.status == .failed {
                let why = (reader.error ?? writer.error)?.localizedDescription ?? "unknown"
                writer.cancelWriting()
                return fail("The cut failed: \(why)")
            }
            writer.endSession(atSourceTime: end)
            await writer.finishWriting()
            guard writer.status == .completed else { return fail("The file could not be finished: \(writer.error?.localizedDescription ?? "?")") }
            control.fraction = 1
            let bytes = try ExportFile.commit(partial, to: url, shareSafe: shareSafe)
            return ExportOutcome(result: .done, url: url, width: width, height: height, frames: counted.value, bytes: bytes)
        } catch {
            return fail("The cut failed: \(error.localizedDescription)")
        }
    }

    /// A count touched from one queue and read after it is done.
    private final class Counter: @unchecked Sendable {
        private let lock = NSLock()
        private var n = 0
        func add() { lock.withLock { n += 1 } }
        var value: Int { lock.withLock { n } }
    }
}
