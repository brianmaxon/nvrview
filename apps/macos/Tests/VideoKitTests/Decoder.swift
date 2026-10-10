import CoreMedia
import Foundation
import Synchronization
import VideoKit
import VideoToolbox

struct DecodedFrame: Sendable {
    let status: OSStatus
    let pts: Int64  // microseconds
    let width: Int
    let height: Int
}

struct DecodeReport {
    var decoded: [DecodedFrame] = []
    var droppedWaitingForKeyframe = 0
    var formatChanges = 0
    var samplesSubmitted = 0
    var submitErrors: [OSStatus] = []
}

/// Pushes frames through the real `FramePipeline` into a real hardware-capable
/// `VTDecompressionSession`, rebuilding the session only when the pipeline
/// reports a format change.
final class ResultBox: Sendable {
    private let frames = Mutex<[DecodedFrame]>([])
    func append(_ frame: DecodedFrame) { frames.withLock { $0.append(frame) } }
    var all: [DecodedFrame] { frames.withLock { $0 } }
}

final class HeadlessDecoder {
    private let results = ResultBox()
    private var session: VTDecompressionSession?
    private(set) var report = DecodeReport()

    func run(_ frames: [VideoFrame], codec: VideoCodec) -> DecodeReport {
        var pipeline = FramePipeline(codec: codec)
        for frame in frames {
            switch pipeline.process(frame) {
            case .dropped(let reason):
                if reason == .waitingForKeyframe { report.droppedWaitingForKeyframe += 1 }
            case .sample(let sample, let changed):
                if changed { report.formatChanges += 1 }
                decode(sample, newFormat: changed)
            }
        }
        finish()
        report.decoded = results.all
        return report
    }

    private func decode(_ sample: CMSampleBuffer, newFormat: Bool) {
        let format = CMSampleBufferGetFormatDescription(sample)!
        if newFormat { finish() }
        if session == nil {
            var s: VTDecompressionSession?
            let status = VTDecompressionSessionCreate(
                allocator: kCFAllocatorDefault, formatDescription: format,
                decoderSpecification: nil, imageBufferAttributes: nil,
                outputCallback: nil, decompressionSessionOut: &s)
            guard status == noErr, let s else {
                report.submitErrors.append(status)
                return
            }
            session = s
        }
        report.samplesSubmitted += 1
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        let micros = Int64(CMTimeConvertScale(pts, timescale: 1_000_000, method: .default).value)
        let status = VTDecompressionSessionDecodeFrame(
            session!, sampleBuffer: sample, flags: [._EnableAsynchronousDecompression],
            infoFlagsOut: nil
        ) { [results] status, _, image, _, _ in
            results.append(DecodedFrame(
                status: status, pts: micros,
                width: image.map(CVPixelBufferGetWidth) ?? 0,
                height: image.map(CVPixelBufferGetHeight) ?? 0))
        }
        if status != noErr { report.submitErrors.append(status) }
    }

    private func finish() {
        guard let session else { return }
        VTDecompressionSessionFinishDelayedFrames(session)
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        VTDecompressionSessionInvalidate(session)
        self.session = nil
    }
}
