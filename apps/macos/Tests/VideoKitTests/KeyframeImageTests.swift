import Foundation
import Testing
import VideoKit

/// Cover thumbnails: a recording's keyframe (Annex-B with its parameter
/// sets) decoded, shrunk, and encoded as JPEG.
@Suite struct KeyframeImageTests {
    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func aKeyframeBecomesASmallJPEG(name: String) throws {
        let clip = try Fixtures.load(name)
        let unit = try #require(clip.units.first { $0.isKeyframe })
        let keyframe = clip.data.subdata(in: unit.range)
        let image = try #require(KeyframeImage.decode(annexB: keyframe, codec: clip.codec))
        #expect(image.width == 320 && image.height == 240)

        let small = try #require(KeyframeImage.small(image, maxSide: 160))
        #expect(small.width == 160 && small.height == 120)
        #expect(small.jpeg.starts(with: [0xff, 0xd8]))
        // Never enlarged.
        let same = try #require(KeyframeImage.small(image, maxSide: 640))
        #expect(same.width == 320 && same.height == 240)
    }

    @Test func whatDoesNotDecodeIsNil() throws {
        #expect(KeyframeImage.thumbnail(annexB: Data([0, 0, 0, 1, 0x65, 0x88]), codec: .h264) == nil)
        #expect(KeyframeImage.thumbnail(annexB: Data(), codec: .h264) == nil)
        // Parameter sets without a keyframe.
        let clip = try Fixtures.load("h264-basic.h264")
        let p = try #require(clip.units.first { !$0.isKeyframe })
        #expect(KeyframeImage.thumbnail(annexB: clip.data.subdata(in: p.range), codec: .h264) == nil)
    }
}
