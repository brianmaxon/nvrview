import CoreML
import CoreVideo
import Foundation
import Testing
import Vision

@testable import VisionLab

// These tests use only the synthetic fixtures (ffmpeg's test pattern) and
// hand-made inputs. They check that the pipeline runs and the arithmetic is
// right, not how accurate any detector is.

let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent()
    .appendingPathComponent("fixtures/synthetic")

func fixture(_ name: String) -> URL { fixtures.appendingPathComponent(name) }

@Suite struct AnnexBTests {
    @Test func splitsThreeAndFourByteStartCodes() {
        let bytes: [UInt8] = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 0x88, 4, 0]
        let nals = splitAnnexB(Data(bytes))
        #expect(nals.map { Array($0.bytes) } == [[0x67, 1, 2], [0x68, 3], [0x65, 0x88, 4]])
        #expect(nals.map { $0.type(.h264) } == [7, 8, 5])
        #expect(nals[2].isKeyframe(.h264))
        #expect(nals[2].startsPicture(.h264))
    }

    @Test func groupsSlicesIntoPictures() throws {
        for (name, codec, count, keys) in [
            ("h264-basic.h264", Codec.h264, 60, 3), ("h265-basic.h265", Codec.h265, 60, 3),
        ] {
            let units = accessUnits(splitAnnexB(try Data(contentsOf: fixture(name))), codec: codec)
            #expect(units.count == count, "\(name)")
            #expect(units.filter(\.isKeyframe).count == keys, "\(name)")
            #expect(units.first?.isKeyframe == true)
            #expect(units.first?.parameterSets.count == (codec == .h264 ? 2 : 3))
        }
    }

    @Test func codecFromExtension() {
        #expect(Codec.from(path: "a/b.h264") == .h264)
        #expect(Codec.from(path: "b.HEVC") == .h265)
        #expect(Codec.from(path: "b.mp4") == nil)
    }
}

@Suite struct DecoderTests {
    func decodeAll(_ name: String, _ codec: Codec) throws -> [CVPixelBuffer] {
        let units = accessUnits(splitAnnexB(try Data(contentsOf: fixture(name))), codec: codec)
        let decoder = Decoder(codec: codec)
        return try units.compactMap { try decoder.decode($0) }
    }

    @Test(arguments: [("h264-basic.h264", Codec.h264), ("h265-basic.h265", Codec.h265)])
    func decodesEveryPictureWithoutConversion(_ name: String, _ codec: Codec) throws {
        let pictures = try decodeAll(name, codec)
        #expect(pictures.count == 60)
        let first = try #require(pictures.first)
        #expect(CVPixelBufferGetWidth(first) == 320)
        #expect(CVPixelBufferGetHeight(first) == 240)
        #expect(CVPixelBufferGetPixelFormatType(first) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
        #expect(CVPixelBufferGetIOSurface(first) != nil)
    }

    @Test(arguments: [("h264-leading-p.h264", Codec.h264), ("h265-leading-p.h265", Codec.h265)])
    func waitsForTheFirstKeyframe(_ name: String, _ codec: Codec) throws {
        #expect(try decodeAll(name, codec).count == 40)
    }

    @Test(arguments: [("h264-resize.h264", Codec.h264), ("h265-resize.h265", Codec.h265)])
    func followsAResolutionChange(_ name: String, _ codec: Codec) throws {
        let pictures = try decodeAll(name, codec)
        #expect(pictures.count == 80)
        #expect(CVPixelBufferGetWidth(pictures[0]) == 320)
        #expect(CVPixelBufferGetWidth(pictures[79]) == 480)
    }
}

@Suite struct SamplingTests {
    @Test func rateSamplesOnePicturePerInterval() {
        let picked = (0..<100).filter { Sampling.rate(2).selects(index: $0, sourceFPS: 10) }
        #expect(picked.count == 20)
        #expect(picked.prefix(3) == [0, 5, 10])
    }

    @Test func rateAboveSourceTakesEverything() {
        #expect((0..<30).allSatisfy { Sampling.rate(50).selects(index: $0, sourceFPS: 10) })
    }

    @Test func explicitFrames() {
        let s = Sampling.frames([3, 7])
        #expect((0..<10).filter { s.selects(index: $0, sourceFPS: 10) } == [3, 7])
    }
}

@Suite struct BenchTests {
    @Test func runsTheVisionBuiltInsOnSampledPictures() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("vision-lab-\(UUID())")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let results = dir.appendingPathComponent("results.jsonl")

        var config = BenchConfig(
            input: fixture("h264-basic.h264"), codec: .h264, sourceFPS: 20, sampling: .rate(5),
            detectors: [
                try HumanDetector(upperBodyOnly: false, device: .auto),
                try HumanDetector(upperBodyOnly: true, device: .auto),
                try AnimalDetector(device: .auto),
                try FaceDetector(device: .auto),
                try SceneClassifier(device: .auto),
            ])
        config.resultsURL = results
        let report = try runBench(config)

        #expect(report.framesDecoded == 60)
        #expect(report.framesSampled == 15)
        #expect(report.width == 320 && report.height == 240)
        #expect(report.pixelFormat == "420v")
        #expect(report.ioSurfaceBacked)
        #expect(Set(report.detectorMs.keys) == ["human", "human-upper", "animal", "face", "classify"])
        #expect(report.detectorMs["human"]?.count == 15)

        let lines = try parseResults(try Data(contentsOf: results))
        #expect(lines.count == 15)
        #expect(lines.map(\.frame).prefix(3) == [0, 4, 8])
        #expect(lines.allSatisfy { $0.results.count == 5 })
    }

    @Test func decodesAloneWithNoDetectors() throws {
        let config = BenchConfig(
            input: fixture("h265-basic.h265"), codec: .h265, sourceFPS: 20, sampling: .all, detectors: [])
        let report = try runBench(config)
        #expect(report.framesDecoded == 60)
        #expect(report.decodeMs.count == 60)
        #expect(report.detectMsTotal.max == 0)
    }

    /// Runs a Core ML detector when a model is supplied (VISION_LAB_MODEL);
    /// models are never part of the repository.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["VISION_LAB_MODEL"] != nil))
    func runsACoreMLDetector() throws {
        let model = URL(fileURLWithPath: ProcessInfo.processInfo.environment["VISION_LAB_MODEL"]!)
        let config = BenchConfig(
            input: fixture("h264-basic.h264"), codec: .h264, sourceFPS: 20, sampling: .rate(2),
            detectors: [try CoreMLDetector(modelURL: model, units: .all)])
        let report = try runBench(config)
        #expect(report.framesSampled == 6)
    }
}

@Suite struct EvalTests {
    func frame(_ index: Int, _ time: Double, _ detections: [Detection]) -> FrameResult {
        FrameResult(frame: index, time: time, results: ["d": DetectorResult(ms: 1, detections: detections)])
    }

    let person = Detection(label: "person", cls: .person, confidence: 0.8, box: [0.1, 0.1, 0.2, 0.4])
    let weakPerson = Detection(label: "person", cls: .person, confidence: 0.2, box: [0.1, 0.1, 0.2, 0.4])

    @Test func parsesLabels() throws {
        let labels = try parseLabels("# comment\nframe,person,vehicle,animal\n0,1,0,0\n5,0,0,1\n")
        #expect(labels.count == 2)
        #expect(labels[0].present == [.person])
        #expect(labels[1].present == [.animal])
        #expect(throws: LabError.self) { try parseLabels("frame,person\nx,1\n") }
    }

    @Test func scoresPictures() {
        let labels = [
            Label(frame: 0, present: [.person]), Label(frame: 1, present: [.person]),
            Label(frame: 2, present: []), Label(frame: 3, present: []),
        ]
        let results = [
            frame(0, 0, [person]), frame(1, 1, [weakPerson]), frame(2, 2, [person]), frame(3, 3, []),
        ]
        let s = scoreFrames(labels: labels, results: results, detector: "d", cls: .person, threshold: 0.5)
        #expect((s.tp, s.fp, s.fn, s.tn) == (1, 1, 1, 1))
        #expect(s.precision == 0.5)
        #expect(s.recall == 0.5)
        let low = scoreFrames(labels: labels, results: results, detector: "any", cls: .person, threshold: 0.1)
        #expect(low.recall == 1)
    }

    @Test func mergesSpans() {
        #expect(spans([1, 2, 3, 10, 11], gap: 2, pad: 0.5) == [Span(start: 0.5, end: 3.5), Span(start: 9.5, end: 11.5)])
        #expect(spans([], gap: 2, pad: 0).isEmpty)
    }

    @Test func scoresEvents() {
        let truth = [Span(start: 10, end: 20), Span(start: 40, end: 45)]
        let results = [
            frame(0, 5, []), frame(1, 12, [person]), frame(2, 15, [person]), frame(3, 42, [weakPerson]),
            frame(4, 80, [person]),
        ]
        let e = scoreEvents(
            truth: truth, results: results, detector: "d", cls: .person, threshold: 0.5, gap: 4, pad: 0.5)
        #expect(e.events == 2)
        #expect(e.detected == 1)
        #expect(e.latencySeconds == [2])
        #expect(e.falseEpisodes == 1)
    }

    @Test func mapsLabelsToClasses() {
        #expect(classify(label: "truck") == .vehicle)
        #expect(classify(label: "Cat") == .animal)
        #expect(classify(label: "person") == .person)
        #expect(classify(label: "chair") == nil)
    }

    @Test func readsMultiArraysOfEitherPrecision() throws {
        for type in [MLMultiArrayDataType.float16, .float32] {
            let a = try MLMultiArray(shape: [1, 2, 3], dataType: type)
            for i in 0..<6 { a[i] = NSNumber(value: Float(i) / 2) }
            #expect(floats(a) == [0, 0.5, 1, 1.5, 2, 2.5])
        }
    }

    @Test func parsesPSTimes() {
        #expect(parsePSTime("0:01.50") == 1.5)
        #expect(parsePSTime("1:02:03.00") == 3723)
        #expect(parsePSTime("2-00:00:01.00") == 172_801)
    }
}

@Suite struct MetricsTests {
    @Test func summarizes() {
        let s = Summary([4, 1, 3, 2, 5])
        #expect(s.count == 5 && s.mean == 3 && s.p50 == 3 && s.max == 5)
        #expect(Summary([]).count == 0)
    }

    @Test func readsProcessCounters() {
        #expect(processCPUSeconds() > 0)
        #expect(memoryFootprint().current > 0)
    }
}
