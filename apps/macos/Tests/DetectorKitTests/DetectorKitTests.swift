import CoreML
import CoreVideo
import Foundation
import Testing

@testable import DetectorKit

// MARK: - Stubs

/// A model that "finds" one object of a chosen class at a chosen place in the
/// input, and records the inputs it was given.
private final class StubInference: ModelInference, @unchecked Sendable {
    let inputSize = 640
    let classID: Int
    let box: NormRect
    private let lock = NSLock()
    private var _centerPixels: [(b: UInt8, g: UInt8, r: UInt8)] = []
    private var _batchSizes: [Int] = []

    init(classID: Int = 2, box: NormRect = NormRect(x: 0.4, y: 0.3, width: 0.2, height: 0.4)) {
        self.classID = classID
        self.box = box
    }

    var centerPixels: [(b: UInt8, g: UInt8, r: UInt8)] { lock.withLock { _centerPixels } }
    var batchSizes: [Int] { lock.withLock { _batchSizes } }

    func infer(_ inputs: [CVPixelBuffer]) throws -> [ModelOutput] {
        lock.withLock { _batchSizes.append(inputs.count) }
        return inputs.map { input in
            CVPixelBufferLockBaseAddress(input, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(input, .readOnly) }
            let base = CVPixelBufferGetBaseAddress(input)!.assumingMemoryBound(to: UInt8.self)
            let row = CVPixelBufferGetBytesPerRow(input)
            let p = base + 320 * row + 320 * 4
            lock.withLock { _centerPixels.append((p[0], p[1], p[2])) }
            var logits = [Float](repeating: -10, count: 300 * 80)
            logits[7 * 80 + classID] = 3  // sigmoid(3) = 0.95
            logits[9 * 80 + 0] = -4  // 0.018: under the default floor
            var boxes = [Float](repeating: 0, count: 300 * 4)
            boxes[7 * 4] = Float(box.x + box.width / 2)
            boxes[7 * 4 + 1] = Float(box.y + box.height / 2)
            boxes[7 * 4 + 2] = Float(box.width)
            boxes[7 * 4 + 3] = Float(box.height)
            return ModelOutput(queries: 300, classes: 80, logits: logits, boxes: boxes)
        }
    }
}

private func bgraPicture(width: Int, height: Int, pixel: (Int, Int) -> (b: UInt8, g: UInt8, r: UInt8)) -> DetectorPicture {
    var buffer: CVPixelBuffer?
    CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &buffer)
    let b = buffer!
    CVPixelBufferLockBaseAddress(b, [])
    defer { CVPixelBufferUnlockBaseAddress(b, []) }
    let base = CVPixelBufferGetBaseAddress(b)!.assumingMemoryBound(to: UInt8.self)
    let row = CVPixelBufferGetBytesPerRow(b)
    for y in 0..<height {
        for x in 0..<width {
            let c = pixel(x, y)
            let p = base + y * row + x * 4
            p[0] = c.b; p[1] = c.g; p[2] = c.r; p[3] = 255
        }
    }
    return DetectorPicture(b)
}

private func close(_ a: Double, _ b: Double, _ tolerance: Double = 1e-6) -> Bool { abs(a - b) <= tolerance }

// MARK: - Geometry

@Suite struct CropGeometryTests {
    @Test func stretchedCropMapsLinearly() {
        // A box in the middle of the second tile, stretched: the crop's
        // origin plus the box scaled by the crop's size.
        let tile = CropPlan.tiles[3]  // (0.4, 0.4, 0.6, 0.6)
        let box = NormRect(x: 0.5, y: 0.25, width: 0.2, height: 0.5)
        let mapped = CropGeometry.toPicture(box, crop: tile, cropPixelWidth: 768, cropPixelHeight: 432, fit: .stretch)
        #expect(close(mapped.x, 0.4 + 0.5 * 0.6))
        #expect(close(mapped.y, 0.4 + 0.25 * 0.6))
        #expect(close(mapped.width, 0.12))
        #expect(close(mapped.height, 0.3))
    }

    @Test func letterboxUndoesPadding() {
        // A 2:1 crop in a square input: content is the full width and the
        // middle half of the height (0.25...0.75).
        let content = CropGeometry.contentRect(cropPixelWidth: 800, cropPixelHeight: 400, fit: .letterbox)
        #expect(close(content.x, 0) && close(content.width, 1) && close(content.y, 0.25) && close(content.height, 0.5))
        // A box covering exactly the content area is the whole crop.
        let whole = CropGeometry.toPicture(
            content, crop: NormRect(x: 0.1, y: 0.2, width: 0.5, height: 0.4), cropPixelWidth: 800, cropPixelHeight: 400,
            fit: .letterbox)
        #expect(close(whole.x, 0.1) && close(whole.y, 0.2) && close(whole.width, 0.5) && close(whole.height, 0.4))
        // A box in the top-left quarter of the content.
        let part = CropGeometry.toPicture(
            NormRect(x: 0, y: 0.25, width: 0.5, height: 0.25), crop: .whole, cropPixelWidth: 800, cropPixelHeight: 400,
            fit: .letterbox)
        #expect(close(part.x, 0) && close(part.y, 0) && close(part.width, 0.5) && close(part.height, 0.5))
    }

    @Test func tallLetterboxPadsTheSides() {
        let content = CropGeometry.contentRect(cropPixelWidth: 100, cropPixelHeight: 400, fit: .letterbox)
        #expect(close(content.width, 0.25) && close(content.x, 0.375) && close(content.height, 1))
    }

    @Test func boxesAreClampedToThePicture() {
        let mapped = CropGeometry.toPicture(
            NormRect(x: 0.9, y: -0.1, width: 0.3, height: 0.3), crop: .whole, cropPixelWidth: 10, cropPixelHeight: 10,
            fit: .stretch)
        #expect(close(mapped.maxX, 1) && close(mapped.y, 0) && close(mapped.maxY, 0.2))
    }

    @Test func pixelRectRoundsOutwardAndStaysInside() {
        let r = CropGeometry.pixelRect(of: NormRect(x: 0.4, y: 0.4, width: 0.6, height: 0.6), pictureWidth: 1280, pictureHeight: 720)
        #expect(r.x == 512 && r.y == 288 && r.x + r.width == 1280 && r.y + r.height == 720)
        let tiny = CropGeometry.pixelRect(of: NormRect(x: 1.5, y: -2, width: 0, height: 0), pictureWidth: 100, pictureHeight: 50)
        #expect(tiny.width >= 1 && tiny.height >= 1 && tiny.x + tiny.width <= 100 && tiny.y + tiny.height <= 50)
    }

    @Test func tilePlanCoversThePictureWithOverlap() {
        #expect(CropPlan.wholeFramePlusTiles.count == 5)
        #expect(CropPlan.tiles.allSatisfy { close($0.width, 0.6) && $0.maxX <= 1 + 1e-9 && $0.maxY <= 1 + 1e-9 })
    }
}

// MARK: - Decoding

@Suite struct DecoderTests {
    @Test func bestClassPerQueryAboveTheFloor() {
        let output = try! #require(StubInference().infer([bgraPicture(width: 640, height: 640) { _, _ in (0, 0, 0) }.buffer]).first)
        let boxes = RTDETRDecoder.decode(output)
        #expect(boxes.count == 1)
        #expect(boxes[0].classID == 2)
        #expect(abs(boxes[0].score - 0.9526) < 1e-3)
        #expect(close(boxes[0].box.x, 0.4) && close(boxes[0].box.width, 0.2))
        // Floor 0 reports every query.
        #expect(RTDETRDecoder.decode(output, scoreFloor: 0).count == 300)
    }
}

// MARK: - Preprocessing and the whole synchronous path

@Suite struct CropDetectorTests {
    @Test func wholeAndTileBoxesLandInPictureCoordinates() throws {
        let stub = StubInference()
        let detector = CropDetector(inference: stub)
        let picture = bgraPicture(width: 1280, height: 720) { _, _ in (10, 20, 30) }
        let results = try detector.detect(picture: picture, crops: CropPlan.wholeFramePlusTiles)
        #expect(results.count == 5)
        #expect(stub.batchSizes == [5])  // one batch for all crops
        let whole = try #require(results[0].detections.first)
        #expect(close(whole.box.x, 0.4) && close(whole.box.y, 0.3) && close(whole.box.height, 0.4))
        // The bottom-right tile reports the same model box; in the picture it
        // is smaller and shifted into the tile.
        let tile = try #require(results[4].detections.first)
        #expect(close(tile.box.x, 0.4 + 0.4 * 0.6) && close(tile.box.width, 0.2 * 0.6))
        #expect(tile.classID == 2)
        #expect(results[4].crop == CropPlan.tiles[3])
    }

    @Test func cropsAreCutAndScaledFromTheRightRegion() throws {
        // Left half red, right half blue; the right-half crop's center is blue.
        let stub = StubInference()
        let detector = CropDetector(inference: stub)
        let picture = bgraPicture(width: 200, height: 100) { x, _ in x < 100 ? (0, 0, 255) : (255, 0, 0) }
        _ = try detector.detect(
            picture: picture,
            crops: [.whole, NormRect(x: 0.5, y: 0, width: 0.5, height: 1), NormRect(x: 0, y: 0, width: 0.5, height: 1)])
        let centers = stub.centerPixels
        #expect(centers.count == 3)
        #expect(centers[1].b > 240 && centers[1].r < 15)  // right half: blue
        #expect(centers[2].r > 240 && centers[2].b < 15)  // left half: red
    }

    @Test func letterboxPadsWithBlack() throws {
        // A 2:1 picture letterboxed: the input's center row is picture, and
        // a pixel in the top padding is black. (The stub only samples the
        // center, so check the preprocessor directly.)
        let pre = CropPreprocessor(size: 64, fit: .letterbox)
        let picture = bgraPicture(width: 200, height: 100) { _, _ in (200, 200, 200) }
        let out = try pre.input(for: .whole, of: picture)
        CVPixelBufferLockBaseAddress(out, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(out, .readOnly) }
        let base = CVPixelBufferGetBaseAddress(out)!.assumingMemoryBound(to: UInt8.self)
        let row = CVPixelBufferGetBytesPerRow(out)
        #expect(base[2 * row + 32 * 4] == 0)  // top padding
        #expect(base[32 * row + 32 * 4] > 190)  // content
        #expect(base[61 * row + 32 * 4] == 0)  // bottom padding
    }

    @Test func nonBGRAPicturesGoThroughCoreImage() throws {
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, 320, 180, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, nil, &buffer)
        let b = try #require(buffer)
        CVPixelBufferLockBaseAddress(b, [])
        memset(CVPixelBufferGetBaseAddressOfPlane(b, 0), 128, CVPixelBufferGetBytesPerRowOfPlane(b, 0) * 180)
        memset(CVPixelBufferGetBaseAddressOfPlane(b, 1), 128, CVPixelBufferGetBytesPerRowOfPlane(b, 1) * 90)
        CVPixelBufferUnlockBaseAddress(b, [])
        let pre = CropPreprocessor(size: 64)
        let out = try pre.input(for: NormRect(x: 0.2, y: 0.2, width: 0.5, height: 0.5), of: DetectorPicture(b))
        CVPixelBufferLockBaseAddress(out, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(out, .readOnly) }
        let p = CVPixelBufferGetBaseAddress(out)!.assumingMemoryBound(to: UInt8.self)
        let row = CVPixelBufferGetBytesPerRow(out)
        let c = p + 32 * row + 32 * 4
        #expect(abs(Int(c[0]) - 128) < 12 && abs(Int(c[1]) - 128) < 12 && abs(Int(c[2]) - 128) < 12)
    }
}

// MARK: - The queue

/// A detector that blocks each call until the test lets it go, and records
/// which jobs it ran (identified by the first crop's x).
private final class GatedDetector: CropDetecting, @unchecked Sendable {
    private let gate = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var _ran: [Double] = []
    var ran: [Double] { lock.withLock { _ran } }
    func open(_ times: Int = 1) { for _ in 0..<times { gate.signal() } }

    func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
        gate.wait()
        lock.withLock { _ran.append(crops[0].x) }
        return crops.map { CropDetections(crop: $0, detections: []) }
    }
}

private func until(_ condition: () async -> Bool) async {
    // A hang guard, not a pace: the condition is polled by yielding.
    let deadline = Date().addingTimeInterval(120)
    while !(await condition()) {
        if Date() >= deadline {
            Issue.record("the condition did not hold within 120 s")
            return
        }
        await Task.yield()
    }
}

@Suite struct DetectorServiceTests {
    private let picture = DetectorPicture(
        bgraPicture(width: 4, height: 4) { _, _ in (0, 0, 0) }.buffer)

    private func crops(_ id: Int) -> [NormRect] { [NormRect(x: Double(id), y: 0, width: 1, height: 1)] }

    @Test func fullQueueDropsTheOldestAndCancellationLeavesTheQueue() async throws {
        let detector = GatedDetector()
        let service = DetectorService(detector: detector, capacity: 2)
        let picture = picture

        let a = Task { try await service.detect(picture: picture, crops: crops(1)) }
        await until { await service.isRunning }  // A holds the worker
        let b = Task { try await service.detect(picture: picture, crops: crops(2)) }
        await until { await service.waitingCount == 1 }
        let c = Task { try await service.detect(picture: picture, crops: crops(3)) }
        await until { await service.waitingCount == 2 }
        let d = Task { try await service.detect(picture: picture, crops: crops(4)) }

        // D arrives at a full queue: B, the oldest, is dropped.
        await #expect(throws: DetectorServiceError.dropped) { try await b.value }
        #expect(await service.waitingCount == 2)
        #expect(await service.droppedCount == 1)

        // C is canceled while waiting and leaves at once.
        c.cancel()
        await #expect(throws: CancellationError.self) { try await c.value }
        await until { await service.waitingCount == 1 }

        detector.open(2)  // A, then D
        _ = try await a.value
        let dResult = try await d.value
        #expect(dResult.first?.crop.x == 4)
        #expect(detector.ran == [1, 4])
        await until { await !service.isRunning }
    }

    @Test func cancelingTheRunningJobReturnsAtOnceAndDiscardsItsResult() async throws {
        let detector = GatedDetector()
        let service = DetectorService(detector: detector, capacity: 1)
        let picture = picture
        let a = Task { try await service.detect(picture: picture, crops: crops(1)) }
        await until { await service.isRunning }
        a.cancel()
        await #expect(throws: CancellationError.self) { try await a.value }
        // The worker is still busy with A, so B waits for it.
        let b = Task { try await service.detect(picture: picture, crops: crops(2)) }
        await until { await service.waitingCount == 1 }
        detector.open(2)
        _ = try await b.value
        #expect(detector.ran == [1, 2])
    }

    @Test func aTaskCanceledBeforeSubmittingNeverRuns() async throws {
        let detector = GatedDetector()
        let service = DetectorService(detector: detector)
        let picture = picture
        let t = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return try await service.detect(picture: picture, crops: crops(1))
        }
        await #expect(throws: CancellationError.self) { try await t.value }
        let waiting = await service.waitingCount
        let busy = await service.isRunning
        #expect(waiting == 0 && !busy)
    }

    @Test func detectorErrorsReachTheCaller() async throws {
        struct Failing: CropDetecting {
            func detect(picture: DetectorPicture, crops: [NormRect]) throws -> [CropDetections] {
                throw PreprocessError.cannotRender
            }
        }
        let service = DetectorService(detector: Failing())
        await #expect(throws: PreprocessError.cannotRender) {
            try await service.detect(picture: picture, crops: [.whole])
        }
    }
}

// MARK: - The real model (opt in)

/// Runs only with NVRVIEW_DETECTOR_MODEL set to a converted model
/// (tools/vision-lab/scripts/convert-rtdetr.py). With
/// NVRVIEW_DETECTOR_TEST_IMAGE set to a picture that contains a person or a
/// vehicle, it also checks that one is found.
@Suite struct ModelInTheLoopTests {
    private static var modelURL: URL? {
        ProcessInfo.processInfo.environment[DetectorModelLocator.environmentVariable].map { URL(fileURLWithPath: $0) }
    }

    @Test(.enabled(if: modelURL != nil)) func syntheticFrameRunsAndStaysInRange() async throws {
        let inference = try await CoreMLInference.load(url: Self.modelURL!)
        #expect(inference.inputSize == 640)
        #expect(inference.info.labels.count == 80 && inference.info.labels[0] == "person")
        let detector = CropDetector(inference: inference)
        let results = try detector.detect(
            picture: DetectorBenchmark.syntheticPicture(), crops: CropPlan.wholeFramePlusTiles)
        #expect(results.count == 5)
        for r in results {
            for d in r.detections {
                #expect(d.score >= 0.05 && d.score <= 1)
                #expect(d.box.x >= 0 && d.box.y >= 0 && d.box.maxX <= 1 + 1e-9 && d.box.maxY <= 1 + 1e-9)
            }
        }
    }

    @Test(.enabled(if: modelURL != nil && ProcessInfo.processInfo.environment["NVRVIEW_DETECTOR_TEST_IMAGE"] != nil))
    func findsAPersonOrVehicleInATestImage() async throws {
        let path = try #require(ProcessInfo.processInfo.environment["NVRVIEW_DETECTOR_TEST_IMAGE"])
        let picture = try Self.load(URL(fileURLWithPath: path))
        let inference = try await CoreMLInference.load(url: Self.modelURL!)
        let results = try CropDetector(inference: inference).detect(
            picture: picture, crops: CropPlan.wholeFramePlusTiles)
        let wanted: Set<Int> = [0, 2, 3, 5, 7]  // person, car, motorcycle, bus, truck
        let hits = results.flatMap(\.detections).filter { wanted.contains($0.classID) && $0.score >= 0.5 }
        #expect(!hits.isEmpty, "no person or vehicle at 0.5; best: \(results[0].detections.map(\.score).max() ?? 0)")
    }

    private static func load(_ url: URL) throws -> DetectorPicture {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
            let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { throw DetectorModelError.notFound }
        let w = image.width, h = image.height
        var buffer: CVPixelBuffer?
        CVPixelBufferCreate(nil, w, h, kCVPixelFormatType_32BGRA, nil, &buffer)
        let b = try #require(buffer)
        CVPixelBufferLockBaseAddress(b, [])
        defer { CVPixelBufferUnlockBaseAddress(b, []) }
        let context = CGContext(
            data: CVPixelBufferGetBaseAddress(b), width: w, height: h, bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(b), space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)!
        context.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        return DetectorPicture(b)
    }
}
