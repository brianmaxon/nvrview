import CoreMedia
import Foundation
import Testing
import VideoKit

@Suite struct FormatDescriptionTests {
    /// Feeds every parameter set of every frame; returns the distinct sizes
    /// seen and how many times the format description changed.
    private func formats(_ clip: Fixtures.Clip) -> (sizes: [CMVideoDimensions], changes: Int, subtype: FourCharCode?) {
        var tracker = ParameterSetTracker(codec: clip.codec)
        var sizes: [CMVideoDimensions] = []
        var changes = 0
        var subtype: FourCharCode?
        for unit in clip.units {
            let bytes = clip.data.subdata(in: unit.range)
            var dirty = false
            for nal in AnnexB.split(bytes, codec: clip.codec)
            where NALType.isParameterSet(nal.type, codec: clip.codec) {
                if tracker.record(type: nal.type, nal: bytes.subdata(in: nal.range)) { dirty = true }
            }
            if tracker.rebuildIfNeeded(dirty: dirty), let fd = tracker.formatDescription {
                changes += 1
                sizes.append(CMVideoFormatDescriptionGetDimensions(fd))
                subtype = CMFormatDescriptionGetMediaSubType(fd)
            }
        }
        return (sizes, changes, subtype)
    }

    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func singleFormat(name: String) throws {
        let clip = try Fixtures.load(name)
        let result = formats(clip)
        #expect(result.changes == 1)  // repeated identical sets are not changes
        #expect(result.sizes.first?.width == 320)
        #expect(result.sizes.first?.height == 240)
        #expect(result.subtype == (clip.codec == .h264 ? kCMVideoCodecType_H264 : kCMVideoCodecType_HEVC))
    }

    @Test(arguments: ["h264-resize.h264", "h265-resize.h265"])
    func resolutionChange(name: String) throws {
        let clip = try Fixtures.load(name)
        let result = formats(clip)
        #expect(result.changes == 2)
        #expect(result.sizes.map { [Int($0.width), Int($0.height)] } == [[320, 240], [480, 270]])
    }

    @Test func incompleteSetsGiveNoDescription() throws {
        let clip = try Fixtures.load("h265-basic.h265")
        let nals = AnnexB.split(clip.data, codec: .h265)
        var tracker = ParameterSetTracker(codec: .h265)
        // VPS and SPS only: no PPS yet.
        for nal in nals.prefix(while: { $0.type != 34 }) where NALType.isParameterSet(nal.type, codec: .h265) {
            let dirty = tracker.record(type: nal.type, nal: clip.data.subdata(in: nal.range))
            let built = tracker.rebuildIfNeeded(dirty: dirty)
            #expect(!built)
        }
        #expect(tracker.formatDescription == nil)
    }
}
