import CoreMedia
import Foundation
import Testing
import VideoKit

/// End-to-end: fixture -> access units -> FramePipeline -> CMSampleBuffer ->
/// a real VTDecompressionSession. Nothing here is mocked.
@Suite struct DecodeTests {
    private func decode(_ name: String) throws -> (Fixtures.Clip, DecodeReport) {
        let clip = try Fixtures.load(name)
        let report = HeadlessDecoder().run(clip.frames(), codec: clip.codec)
        return (clip, report)
    }

    private func expectAllDecoded(_ report: DecodeReport) {
        #expect(report.submitErrors.isEmpty)
        #expect(report.decoded.allSatisfy { $0.status == noErr })
        #expect(report.decoded.allSatisfy { $0.width > 0 && $0.height > 0 })
    }

    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func basic(name: String) throws {
        let (_, report) = try decode(name)
        expectAllDecoded(report)
        #expect(report.droppedWaitingForKeyframe == 0)
        #expect(report.formatChanges == 0)
        #expect(report.samplesSubmitted == 60)
        #expect(report.decoded.count == 60)
        #expect(report.decoded.map(\.pts).sorted() == (0..<60).map { Int64($0) * 50_000 })
        #expect(report.decoded.allSatisfy { $0.width == 320 && $0.height == 240 })
    }

    @Test(arguments: ["h264-leading-p.h264", "h265-leading-p.h265"])
    func leadingPFramesAreGated(name: String) throws {
        let (_, report) = try decode(name)
        expectAllDecoded(report)
        #expect(report.droppedWaitingForKeyframe == 19)
        #expect(report.samplesSubmitted == 40)
        #expect(report.decoded.count == 40)
        let first: Int64? = report.decoded.map(\.pts).min()
        #expect(first == Int64(950_000))
    }

    @Test(arguments: ["h264-resize.h264", "h265-resize.h265"])
    func resolutionChange(name: String) throws {
        let (_, report) = try decode(name)
        expectAllDecoded(report)
        #expect(report.droppedWaitingForKeyframe == 0)
        #expect(report.formatChanges == 1)
        #expect(report.decoded.count == 80)
        let byPTS = report.decoded.sorted { $0.pts < $1.pts }
        #expect(byPTS.prefix(40).allSatisfy { $0.width == 320 && $0.height == 240 })
        #expect(byPTS.suffix(40).allSatisfy { $0.width == 480 && $0.height == 270 })
    }

    @Test func gateReopensAfterFormatChangeEvent() throws {
        // An explicit format change closes the gate: P-frames after it are
        // dropped until the next keyframe, which carries its own parameter sets.
        let clip = try Fixtures.load("h264-basic.h264")
        let frames = clip.frames()
        var pipeline = FramePipeline(codec: .h264)
        for frame in frames[0..<5] { _ = pipeline.process(frame) }
        pipeline.closeGate(forgetParameterSets: true)
        var gated = 0
        var firstEmitted: Int?
        for (i, frame) in frames.enumerated().dropFirst(5) {
            switch pipeline.process(frame) {
            case .dropped(.waitingForKeyframe): gated += 1
            case .sample: if firstEmitted == nil { firstEmitted = i }
            default: break
            }
        }
        #expect(gated == 15)
        #expect(firstEmitted == 20)
    }

    @Test func sampleAttachments() throws {
        let clip = try Fixtures.load("h264-basic.h264")
        var pipeline = FramePipeline(codec: .h264)
        var samples: [CMSampleBuffer] = []
        for frame in clip.frames().prefix(3) {
            if case .sample(let s, _) = pipeline.process(frame) { samples.append(s) }
        }
        #expect(samples.count == 3)
        func notSync(_ s: CMSampleBuffer) -> Bool {
            let array = CMSampleBufferGetSampleAttachmentsArray(s, createIfNecessary: false) as? [[CFString: Any]]
            return (array?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false
        }
        #expect(!notSync(samples[0]))
        #expect(notSync(samples[1]))
        #expect(CMSampleBufferGetPresentationTimeStamp(samples[1]) == CMTime(value: 50_000, timescale: 1_000_000))
        // Length-prefixed: first 4 bytes are the big-endian NAL length.
        let block = CMSampleBufferGetDataBuffer(samples[1])!
        var length: UInt32 = 0
        CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: 4, destination: &length)
        #expect(Int(UInt32(bigEndian: length)) + 4 == CMBlockBufferGetDataLength(block))
    }
}
