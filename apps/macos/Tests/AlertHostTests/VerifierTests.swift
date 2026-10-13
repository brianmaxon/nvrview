import DetectorKit
import Foundation
import NVRCore
import Testing
import VideoKit

@testable import AlertHost

// The verification host: a request's access units are decoded by VideoToolbox
// and the crops go to the detector; the boxes come back in the core's shape.

/// A detector that reports one person and one kite (not an alert class) in
/// every crop, and keeps what it was asked.
private final class StubDetector: CropDetecting, @unchecked Sendable {
    private let lock = NSLock()
    private var _crops: [[NormRect]] = []
    private var _sizes: [(Int, Int)] = []
    var crops: [[NormRect]] { lock.withLock { _crops } }
    var sizes: [(Int, Int)] { lock.withLock { _sizes } }

    func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
        lock.withLock {
            _crops.append(crops)
            _sizes.append((picture.width, picture.height))
        }
        return crops.map { crop in
            CropDetections(
                crop: crop,
                detections: [
                    RawDetection(classID: 0, score: 0.9, box: NormRect(x: 0.4, y: 0.3, width: 0.1, height: 0.4)),
                    RawDetection(classID: 33, score: 0.9, box: NormRect(x: 0, y: 0, width: 0.1, height: 0.1)),
                    RawDetection(classID: 2, score: 0.01, box: NormRect(x: 0.5, y: 0.5, width: 0.1, height: 0.1)),
                ])
        }
    }
}

private func clip() throws -> (data: Data, codec: VideoCodec) {
    var url = URL(fileURLWithPath: #filePath)
    for _ in 0..<5 { url.deleteLastPathComponent() }  // file, AlertHostTests, Tests, macos, apps
    let data = try Data(contentsOf: url.appendingPathComponent("fixtures/synthetic/h264-basic.h264"))
    return (data, .h264)
}

/// The first keyframe and the access units after it, as one request's frames.
private func frames(count: Int) throws -> AlertFrames {
    let (data, codec) = try clip()
    let units = AccessUnitSplitter.split(data, codec: codec)
    let upto = units[min(count, units.count) - 1].range.upperBound
    return AlertFrames(
        kind: .h264Keyframe, width: 0, height: 0, ptsMicros: 0, data: data.subdata(in: 0..<upto))
}

private func request(crops: [VerifyRequest.Crop] = []) throws -> VerifyRequest {
    let json = """
        {"type":"verify_request","request":"v1","purpose":"check","camera":{"device":1,"channel":2,"lens":0},
         "trigger_ids":[1],"codec":"h264","width":0,"height":0,"pts_us":0,"bytes":0,"frames":[],
         "from_cover":false,"at_ms":0,"deadline_ms":0,"classes":["person"],"min_score":0.5,"crops":[]}
        """
    var r = try JSONDecoder().decode(VerifyRequest.self, from: Data(json.utf8))
    r.crops = crops
    return r
}

@Suite struct VerifierTests {
    @Test func theLastPictureOfTheAccessUnitsDecodes() throws {
        let (data, codec) = try clip()
        let units = AccessUnitSplitter.split(data, codec: codec)
        #expect(units.count >= 3 && units[0].isKeyframe)
        let picture = try PictureDecoder.decode(annexB: try frames(count: 3).data, codec: .h264)
        #expect(picture.width > 0 && picture.height > 0)
        #expect(throws: PictureDecoder.Failure.noAccessUnits) { try PictureDecoder.decode(annexB: Data(), codec: .h264) }
    }

    @Test func cropsGoToTheDetectorAndBoxesComeBackAsAlertClasses() async throws {
        let stub = StubDetector()
        let handle = DetectorHandle(
            service: DetectorService(detector: stub), labels: DetectorProvider.cocoLabels)
        let verifier = AlertVerifier(core: { throw CoreStatus.closed }, detector: { handle })
        let crops = [
            VerifyRequest.Crop(what: "frame", x: 0, y: 0, w: 1, h: 1),
            VerifyRequest.Crop(what: "tile", x: 0, y: 0, w: 0.6, h: 0.6),
        ]
        let result = try await verifier.boxes(for: request(crops: crops), frames: frames(count: 3))
        let boxes = try result.get()
        // Per crop: the person only (a kite is no alert class; 0.01 is under the floor).
        #expect(boxes.count == 2 && boxes.allSatisfy { $0.label == "person" && $0.score > 0.8 })
        #expect(stub.crops == [[NormRect(x: 0, y: 0, width: 1, height: 1), NormRect(x: 0, y: 0, width: 0.6, height: 0.6)]])
        #expect(stub.sizes.first.map { $0.0 > 0 && $0.1 > 0 } == true)
        // The wire shape is the core's.
        let json = AlertDetection.json(boxes)
        let object = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        #expect((object["detections"] as? [[String: Any]])?.count == 2)
    }

    @Test func noCropsMeansTheWholePicture() async throws {
        let stub = StubDetector()
        let handle = DetectorHandle(service: DetectorService(detector: stub), labels: DetectorProvider.cocoLabels)
        let verifier = AlertVerifier(core: { throw CoreStatus.closed }, detector: { handle })
        _ = try await verifier.boxes(for: request(), frames: frames(count: 2)).get()
        #expect(stub.crops == [[.whole]])
    }

    @Test func noModelAnswersNothing() async throws {
        let verifier = AlertVerifier(core: { throw CoreStatus.closed }, detector: { nil })
        let result = try await verifier.boxes(for: request(), frames: frames(count: 2))
        #expect(result == .failure(.skipped))
    }

    @Test func aPictureThatDoesNotDecodeIsNotAnswered() async throws {
        let verifier = AlertVerifier(core: { throw CoreStatus.closed }, detector: { nil })
        let junk = AlertFrames(kind: .h264Keyframe, width: 0, height: 0, ptsMicros: 0, data: Data([0, 0, 0, 1, 0x65, 1, 2]))
        #expect(try await verifier.boxes(for: request(), frames: junk) == .failure(.failed))
    }

    @Test func theDecodedPictureIsKeptForTheSnapshot() async throws {
        let verifier = AlertVerifier(core: { throw CoreStatus.closed }, detector: { nil })
        let r = try request()
        _ = await verifier.boxes(for: r, frames: try frames(count: 3))
        let picture = try #require(verifier.pictures.latest(for: r.camera))
        let jpeg = try #require(SnapshotMaker.jpeg(of: picture, maxSide: 320))
        #expect(jpeg.starts(with: [0xFF, 0xD8]))
        #expect(verifier.pictures.latest(for: AlertCamera(device: 9, channel: 9)) == nil)
    }
}
