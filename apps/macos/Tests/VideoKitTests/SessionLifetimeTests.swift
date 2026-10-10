import Foundation
import Testing
@testable import VideoKit
import VideoToolbox

@Suite struct SessionLifetimeTests {
    @Test func replacedSessionsAreDeallocated() throws {
        let clip = try Fixtures.load("h264-basic.h264")
        let key = clip.data.subdata(in: clip.units[0].range)
        let sets = AnnexB.split(key, codec: .h264)
            .filter { NALType.isParameterSet($0.type, codec: .h264) }
            .map { key.subdata(in: $0.range) }
        let decoder = TileDecoder { _ in }
        weak var first: VTDecompressionSession?
        weak var second: VTDecompressionSession?
        decoder.configure(codec: .h264, parameterSets: sets, fullRange: false)
        decoder.decode(annexB: key, keyframe: true, ptsMicros: 0)
        decoder.waitForPendingFrames()
        first = decoder.sessionForTesting
        #expect(first != nil)
        decoder.configure(codec: .h264, parameterSets: sets, fullRange: false)
        second = decoder.sessionForTesting
        #expect(second != nil)
        #expect(first == nil)
        decoder.reset()
        #expect(second == nil)
    }
}
