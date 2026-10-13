import CoreGraphics
import DetectorKit
import Foundation
import NVRCore
import Synchronization
import VideoKit

/// The detector as the verifier uses it: the off-main-thread queue and the
/// model's class names.
public struct DetectorHandle: Sendable {
    public var service: DetectorService
    /// Class names by the model's class index (COCO order).
    public var labels: [String]

    public init(service: DetectorService, labels: [String]) {
        self.service = service
        self.labels = labels
    }
}

/// Loads the Core ML model once, when the first picture needs it.
public actor DetectorProvider {
    private let installer: ModelInstaller
    private var handle: DetectorHandle?
    private var failed = false

    public init(installer: ModelInstaller) { self.installer = installer }

    /// The detector, or nil when there is no model (or it does not load).
    public func detector() async -> DetectorHandle? {
        if let handle { return handle }
        if failed { return nil }
        guard let url = await installer.status.url else { return nil }
        do {
            let inference = try await CoreMLInference.load(url: url)
            let labels = inference.info.labels.isEmpty ? Self.cocoLabels : inference.info.labels
            let made = DetectorHandle(
                service: DetectorService(detector: CropDetector(inference: inference)), labels: labels)
            handle = made
            return made
        } catch {
            failed = true
            return nil
        }
    }

    /// COCO class names for the classes that matter here, used when the
    /// model carries no label list.
    public static let cocoLabels: [String] = {
        var names = [String](repeating: "", count: 80)
        for (index, name) in [
            0: "person", 1: "bicycle", 2: "car", 3: "motorcycle", 5: "bus", 7: "truck", 15: "cat", 16: "dog",
        ] { names[index] = name }
        return names
    }()
}

/// The last picture checked for each camera, kept for the snapshot of an
/// alert that follows. Bounded.
public final class PictureCache: Sendable {
    private let held = Mutex<[AlertCamera: CheckedPicture]>([:])
    private let capacity = 16

    public init() {}

    public func store(_ picture: CheckedPicture, for camera: AlertCamera) {
        held.withLock { pictures in
            if pictures[camera] == nil, pictures.count >= capacity, let drop = pictures.keys.first {
                pictures[drop] = nil
            }
            pictures[camera] = picture
        }
    }

    public func latest(for camera: AlertCamera) -> CheckedPicture? { held.withLock { $0[camera] } }
}

/// Which checks leave evidence for review (docs/NOTIFICATIONS.md,
/// "Evidence"): outdoor person, animal and vehicle checks, from a camera's AI
/// or an external sensor, whatever their outcome; not motion-only checks (they
/// ask for no class) and not rechecks. Cameras the user marked as inside a
/// building are left out.
public final class EvidencePolicy: Sendable {
    private let inside = Mutex<Set<AlertCamera>>([])
    private let counts = Mutex<[UInt64: Int]>([:])

    public init() {}

    public func setInside(_ cameras: Set<AlertCamera>) { inside.withLock { $0 = cameras } }

    public func keeps(_ request: VerifyRequest) -> Bool {
        guard request.purpose == "check", !inside.withLock({ $0.contains(request.camera) }) else { return false }
        return !Set(request.classes).isDisjoint(with: ["person", "vehicle", "animal"])
    }

    /// The next frame index for a trigger, nil once it has the 8 the core keeps.
    func nextFrame(trigger: UInt64) -> Int? {
        counts.withLock { c in
            let n = c[trigger] ?? 0
            guard n < 8 else { return nil }
            c[trigger] = n + 1
            if c.count > 256, let drop = c.keys.min() { c[drop] = nil }
            return n
        }
    }
}

/// Answers the core's verification requests (docs/FFI.md, "Verification"):
/// fetches the access units, decodes the picture, runs the detector on the
/// crops, and returns the boxes. All of it off the main thread. With no
/// model nothing is answered and the core counts the picture as not
/// checked (failing open for always-alert cases).
public final class AlertVerifier: Sendable {
    private let core: @Sendable () throws -> any CoreAPI
    private let detector: @Sendable () async -> DetectorHandle?
    public let pictures: PictureCache
    public let evidence: EvidencePolicy

    public init(
        core: @escaping @Sendable () throws -> any CoreAPI,
        detector: @escaping @Sendable () async -> DetectorHandle?,
        pictures: PictureCache = PictureCache(),
        evidence: EvidencePolicy = EvidencePolicy()
    ) {
        self.core = core
        self.detector = detector
        self.pictures = pictures
        self.evidence = evidence
    }

    /// What happened to a request (for tests and the status line).
    public enum Outcome: Error, Sendable, Equatable {
        case answered(boxes: Int)
        /// No model, or the detector's queue dropped it: nothing was sent.
        case skipped
        /// The picture did not decode, or the request was gone.
        case failed
    }

    @discardableResult
    public func answer(_ request: VerifyRequest) async -> Outcome {
        let frames: AlertFrames
        do {
            frames = try core().alertFramesGet(request: request.request)
        } catch {
            return .failed
        }
        switch await boxes(for: request, frames: frames) {
        case .failure(let outcome):
            return outcome
        case .success(let boxes):
            // The evidence goes first: the core adds the decision to it.
            keepEvidence(request, boxes: boxes)
            do {
                try core().alertDetectionsPut(request: request.request, json: AlertDetection.json(boxes))
            } catch {
                return .failed
            }
            return .answered(boxes: boxes.count)
        }
    }

    /// Puts the checked picture and our detector's boxes with the core for
    /// each trigger of the request, when the policy says so. Failures are
    /// not the check's: it goes on without evidence.
    private func keepEvidence(_ request: VerifyRequest, boxes: [AlertDetection]) {
        guard evidence.keeps(request), let picture = pictures.latest(for: request.camera),
            let jpeg = SnapshotMaker.jpeg(of: picture)
        else { return }
        let evidenceBoxes = boxes.map {
            EvidenceBox(label: $0.label, score: $0.score, x: $0.x, y: $0.y, w: $0.w, h: $0.h)
        }
        for trigger in request.triggerIDs {
            guard let frame = evidence.nextFrame(trigger: trigger) else { continue }
            let json = EvidenceDocument.request(
                triggerID: trigger, frame: frame, atMs: request.atMs,
                methods: ["detector": (claimed: nil, boxes: evidenceBoxes)])
            try? core().alertEvidencePut(json: json, data: jpeg)
        }
    }

    /// Decodes the request's picture, keeps it for the snapshot, and runs
    /// the detector: the boxes to answer with, or why there are none.
    public func boxes(for request: VerifyRequest, frames: AlertFrames) async -> Result<[AlertDetection], Outcome> {
        let codec: VideoCodec = frames.kind == .h265Keyframe ? .h265 : .h264
        let data = Data(frames.data)
        // Decoding blocks: keep it off the cooperative pool's main work.
        let picture: CheckedPicture
        do {
            picture = try await Task.detached(priority: .utility) {
                try PictureDecoder.decode(annexB: data, codec: codec)
            }.value
        } catch {
            return .failure(.failed)
        }
        pictures.store(picture, for: request.camera)
        guard let handle = await detector() else { return .failure(.skipped) }
        let crops =
            request.crops.isEmpty
            ? [NormRect.whole]
            : request.crops.map { NormRect(x: $0.x, y: $0.y, width: $0.w, height: $0.h) }
        let results: [CropDetections]
        do {
            results = try await handle.service.detect(picture: DetectorPicture(picture.buffer), crops: crops)
        } catch {
            return .failure(.skipped)
        }
        let floor = Float(min(request.minScore, 0.5) * 0.5)
        return .success(Self.boxes(from: results, labels: handle.labels, floor: floor))
    }

    /// The boxes the core can use: labels that map to an alert class (a
    /// person, a vehicle, an animal), above `floor`. Thresholds and merging
    /// across crops are the core's.
    static func boxes(from crops: [CropDetections], labels: [String], floor: Float) -> [AlertDetection] {
        var out: [AlertDetection] = []
        for crop in crops {
            for d in crop.detections where d.score >= floor && labels.indices.contains(d.classID) {
                let label = labels[d.classID]
                guard Self.alertLabels.contains(label) else { continue }
                out.append(
                    AlertDetection(
                        label: label, score: Double(d.score), x: d.box.x, y: d.box.y, w: d.box.width,
                        h: d.box.height))
            }
        }
        return out
    }

    static let alertLabels: Set<String> = ["person", "car", "truck", "bus", "motorcycle", "dog", "cat"]
}

/// A small JPEG of a checked picture, for the alert's snapshot.
public enum SnapshotMaker {
    public static func jpeg(of picture: CheckedPicture, maxSide: Int = 1280) -> Data? {
        picture.image().flatMap { KeyframeImage.small($0, maxSide: maxSide, quality: 0.8)?.jpeg }
    }
}
