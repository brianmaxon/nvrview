import Foundation
import Testing
import VideoKit

@Suite struct AnnexBTests {
    @Test func splitsThreeAndFourByteStartCodes() {
        let bytes: [UInt8] = [
            0, 0, 0, 1, 0x67, 1, 2, 0,  // 4-byte start code, H.264 SPS, trailing zero
            0, 0, 1, 0x68, 3,  // 3-byte start code, PPS
            0, 0, 1, 0x65, 0, 0, 3, 9,  // IDR with an emulation-prevention byte
        ]
        let nals = AnnexB.split(Data(bytes), codec: .h264)
        #expect(nals.map(\.type) == [7, 8, 5])
        #expect(nals[0].range == 4..<7)
        #expect(nals[1].range == 11..<13)
        #expect(nals[2].range == 16..<21)
    }

    @Test func ignoresGarbageAndEmpty() {
        #expect(AnnexB.split(Data(), codec: .h264).isEmpty)
        #expect(AnnexB.split(Data([1, 2, 3, 4]), codec: .h265).isEmpty)
    }

    @Test func classifiesTypes() {
        #expect(NALType.isParameterSet(7, codec: .h264))
        #expect(NALType.isParameterSet(8, codec: .h264))
        #expect(!NALType.isParameterSet(5, codec: .h264))
        #expect(NALType.isKeyframe(5, codec: .h264))
        #expect(!NALType.isKeyframe(1, codec: .h264))
        for t: UInt8 in 32...34 { #expect(NALType.isParameterSet(t, codec: .h265)) }
        for t: UInt8 in 16...21 { #expect(NALType.isKeyframe(t, codec: .h265)) }
        #expect(!NALType.isKeyframe(1, codec: .h265))
        #expect(!NALType.isKeyframe(22, codec: .h265))
    }

    @Test(arguments: ["h264-basic.h264", "h265-basic.h265"])
    func fixtureStructure(name: String) throws {
        let clip = try Fixtures.load(name)
        #expect(clip.units.count == 60)
        #expect(clip.units.filter(\.isKeyframe).count == 3)
        #expect(clip.units.enumerated().filter { $0.element.isKeyframe }.map(\.offset) == [0, 20, 40])

        // Units tile the file exactly.
        #expect(clip.units.first?.range.lowerBound == 0)
        #expect(clip.units.last?.range.upperBound == clip.data.count)
        for (a, b) in zip(clip.units, clip.units.dropFirst()) {
            #expect(a.range.upperBound == b.range.lowerBound)
        }

        // Every keyframe unit carries its parameter sets; no other unit does.
        for unit in clip.units {
            let nals = AnnexB.split(clip.data.subdata(in: unit.range), codec: clip.codec)
            let hasSets = nals.contains { NALType.isParameterSet($0.type, codec: clip.codec) }
            #expect(hasSets == unit.isKeyframe)
            let sets = Set(nals.filter { NALType.isParameterSet($0.type, codec: clip.codec) }.map(\.type))
            if unit.isKeyframe {
                #expect(sets == (clip.codec == .h264 ? [7, 8] : [32, 33, 34]))
            }
            #expect(nals.contains { NALType.isVCL($0.type, codec: clip.codec) })
        }
    }

    @Test func leadingPFixtureStartsWithoutKeyframe() throws {
        for name in ["h264-leading-p.h264", "h265-leading-p.h265"] {
            let clip = try Fixtures.load(name)
            #expect(clip.units.count == 59)
            #expect(clip.units.prefix(19).allSatisfy { !$0.isKeyframe })
            #expect(clip.units[19].isKeyframe)
        }
    }

    @Test func resizeFixtureHasTwoSegments() throws {
        for name in ["h264-resize.h264", "h265-resize.h265"] {
            let clip = try Fixtures.load(name)
            #expect(clip.units.count == 80)
            #expect(clip.units.enumerated().filter { $0.element.isKeyframe }.map(\.offset) == [0, 20, 40, 60])
        }
    }
}
