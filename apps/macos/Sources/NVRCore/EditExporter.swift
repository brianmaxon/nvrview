import AVFoundation
import CoreVideo
import Foundation
import VideoKit

/// Writes a follow cut or a grid to an MP4 (docs/UX.md "Recordings",
/// "Export"): the same edit the player shows, drawn frame by frame with the
/// live view's Metal renderer from the VideoToolbox-decoded files
/// (`EditSession.render`), and encoded with VideoToolbox as H.264 or HEVC
/// through `AVAssetWriter`. The sound follows the live view's rules
/// (`ExportGeometry.audioPieces`): the shown camera's in a follow cut, one
/// cell's in a grid, taken from the downloaded files and written as AAC.
///
/// Everything runs off the main thread, one frame at a time: the writer asks
/// for frames only when it can take them (`requestMediaDataWhenReady`), so
/// memory holds one rendered frame, the decoders' two pictures per file, and
/// the writer's own few buffers, however long the edit is. A canceled or
/// failed export leaves no file.
public enum EditExporter {
    public struct Settings: Sendable, Equatable {
        public var codec: ExportCodec
        public var fps: Int
        public var shareSafe: Bool
        /// The grid cell whose sound is written.
        public var gridCell: Int
        /// Whether the sound is written (the Export tab's switch).
        public var sound: Bool

        public init(
            codec: ExportCodec = .h264, fps: Int = ExportGeometry.framesPerSecond, shareSafe: Bool = false, gridCell: Int = 0,
            sound: Bool = true
        ) {
            self.codec = codec
            self.fps = fps
            self.shareSafe = shareSafe
            self.gridCell = gridCell
            self.sound = sound
        }
    }

    public static func export(plan: EditPlan, to url: URL, settings: Settings, control: ExportControl) async -> ExportOutcome {
        let partial = ExportFile.partial(for: url)
        func fail(_ why: String) -> ExportOutcome {
            ExportFile.discard(partial)
            return ExportOutcome(result: .failed(why), url: url)
        }
        let size = ExportGeometry.outputSize(plan)
        let fps = max(1, settings.fps)
        let frames = ExportGeometry.frameCount(plan, fps: fps)
        let session = await EditSession.open(plan)
        defer { session.close() }
        guard !session.files.isEmpty else { return fail("The downloaded files could not be opened") }
        do {
            let writer = try AVAssetWriter(outputURL: partial, fileType: .mp4)
            writer.shouldOptimizeForNetworkUse = false
            let videoInput = AVAssetWriterInput(
                mediaType: .video, outputSettings: settings.codec.settings(width: size.width, height: size.height, fps: fps))
            videoInput.expectsMediaDataInRealTime = false
            let adaptor = AVAssetWriterInputPixelBufferAdaptor(
                assetWriterInput: videoInput,
                sourcePixelBufferAttributes: [
                    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                    kCVPixelBufferWidthKey as String: size.width, kCVPixelBufferHeightKey as String: size.height,
                ])
            guard writer.canAdd(videoInput) else { return fail("The video format is not available") }
            writer.add(videoInput)

            // The sound: the pieces' audio on one composition track, read as
            // PCM and written as AAC.
            var audio: (reader: AVAssetReader, output: AVAssetReaderOutput, input: AVAssetWriterInput)?
            var keepAlive: [AVURLAsset] = []
            defer { withExtendedLifetime(keepAlive) {} }
            if settings.sound, let track = await soundTrack(plan, pieces: ExportGeometry.audioPieces(plan, gridCell: settings.gridCell), composition: AVMutableComposition()) {
                let reader = try AVAssetReader(asset: track.composition)
                let output = AVAssetReaderAudioMixOutput(audioTracks: [track.track], audioSettings: ExportAudio.pcm)
                let input = AVAssetWriterInput(mediaType: .audio, outputSettings: ExportAudio.aacSettings)
                if reader.canAdd(output), writer.canAdd(input) {
                    reader.add(output)
                    writer.add(input)
                    audio = (reader, output, input)
                    keepAlive = track.assets
                }
            }
            guard writer.startWriting() else { return fail("The file could not be written: \(writer.error?.localizedDescription ?? "?")") }
            writer.startSession(atSourceTime: .zero)
            if let audio, !audio.reader.startReading() { return fail("The sound could not be read: \(audio.reader.error.map(String.init(describing:)) ?? "?")") }

            let group = DispatchGroup()
            let feeder = FrameFeeder(
                session: session, plan: plan, input: videoInput, adaptor: adaptor, size: size, fps: fps, frames: frames,
                control: control)
            group.enter()
            feeder.run { group.leave() }
            if let audio {
                group.enter()
                SampleCopier(
                    output: audio.output, input: audio.input, control: control,
                    queue: DispatchQueue(label: "nvrview.export.audio", qos: .userInitiated)
                ).run { group.leave() }
            }
            let drained = await WriterWait.drain(group, writer: writer, ceiling: WriterWait.exportCeiling)
            if control.isCanceled {
                audio?.reader.cancelReading()
                writer.cancelWriting()
                ExportFile.discard(partial)
                return ExportOutcome(result: .canceled, url: url)
            }
            if let why = feeder.failure ?? writer.error?.localizedDescription ?? (drained == .drained ? nil : "The writer stopped taking frames") {
                audio?.reader.cancelReading()
                writer.cancelWriting()
                return fail(why)
            }
            writer.endSession(atSourceTime: CMTime(value: Int64(frames), timescale: Int32(fps)))
            _ = await WriterWait.finish(writer, ceiling: WriterWait.exportCeiling)
            guard writer.status == .completed else { return fail("The file could not be finished: \(writer.error?.localizedDescription ?? "?")") }
            control.fraction = 1
            let bytes = try ExportFile.commit(partial, to: url, shareSafe: settings.shareSafe)
            return ExportOutcome(result: .done, url: url, width: size.width, height: size.height, frames: feeder.written, bytes: bytes)
        } catch {
            return fail("The export failed: \(error.localizedDescription)")
        }
    }

    /// The pieces' sound placed on one track of `composition`; nil when no
    /// piece has any.
    private static func soundTrack(
        _ plan: EditPlan, pieces: [ExportGeometry.AudioPiece], composition: AVMutableComposition
    ) async -> (composition: AVMutableComposition, track: AVMutableCompositionTrack, assets: [AVURLAsset])? {
        var sources: [Int: AVAssetTrack?] = [:]
        var assets: [AVURLAsset] = []
        var target: AVMutableCompositionTrack?
        for piece in pieces {
            if sources[piece.source] == nil {
                var found: AVAssetTrack?
                if let file = plan.source(piece.source)?.file {
                    let asset = AVURLAsset(url: URL(fileURLWithPath: file))
                    assets.append(asset)
                    found = try? await asset.loadTracks(withMediaType: .audio).first
                }
                sources[piece.source] = .some(found)
            }
            guard let track = sources[piece.source] ?? nil, piece.fileEndMs > piece.fileStartMs else { continue }
            if target == nil { target = composition.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid) }
            let range = CMTimeRange(
                start: CMTime(value: max(piece.fileStartMs, 0), timescale: 1000),
                duration: CMTime(value: piece.fileEndMs - max(piece.fileStartMs, 0), timescale: 1000))
            do {
                try target?.insertTimeRange(range, of: track, at: CMTime(value: piece.atMs, timescale: 1000))
            } catch {
                NSLog("export: sound not placed: %@", String(describing: error))
            }
        }
        return target.map { (composition, $0, assets) }
    }

    /// Draws the edit's frames and hands them to the writer as it asks.
    private final class FrameFeeder: @unchecked Sendable {
        private let session: EditSession
        private let plan: EditPlan
        private let input: AVAssetWriterInput
        private let adaptor: AVAssetWriterInputPixelBufferAdaptor
        private let size: (width: Int, height: Int)
        private let fps: Int
        private let frames: Int
        private let control: ExportControl
        private let queue = DispatchQueue(label: "nvrview.export.video", qos: .userInitiated)
        private var renderer: GridRenderer?
        private var next = 0
        private var finished = false
        private let state = NSLock()
        private var failureText: String?

        init(
            session: EditSession, plan: EditPlan, input: AVAssetWriterInput, adaptor: AVAssetWriterInputPixelBufferAdaptor,
            size: (width: Int, height: Int), fps: Int, frames: Int, control: ExportControl
        ) {
            self.session = session
            self.plan = plan
            self.input = input
            self.adaptor = adaptor
            self.size = size
            self.fps = fps
            self.frames = frames
            self.control = control
        }

        var failure: String? { state.withLock { failureText } }
        var written: Int { state.withLock { next } }

        private func fail(_ why: String) { state.withLock { failureText = why } }

        func run(finish: @escaping @Sendable () -> Void) {
            input.requestMediaDataWhenReady(on: queue) { [self] in
                while input.isReadyForMoreMediaData, !finished {
                    if control.isCanceled || failure != nil || next >= frames {
                        finished = true
                        input.markAsFinished()
                        finish()
                        return
                    }
                    if !append(frame: next) {
                        finished = true
                        input.markAsFinished()
                        finish()
                        return
                    }
                    state.withLock { next += 1 }
                    control.fraction = Double(next) / Double(frames)
                }
            }
        }

        private func append(frame i: Int) -> Bool {
            if renderer == nil { renderer = try? GridRenderer() }
            guard let renderer else {
                fail("Metal is not available")
                return false
            }
            let t = ExportGeometry.time(plan, frame: i, fps: fps)
            guard let image = session.render(at: t, width: size.width, height: size.height, with: renderer),
                let pool = adaptor.pixelBufferPool
            else {
                fail("A frame could not be drawn")
                return false
            }
            var out: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &out) == kCVReturnSuccess, let buffer = out else {
                fail("A frame buffer could not be made")
                return false
            }
            CVPixelBufferLockBaseAddress(buffer, [])
            if let base = CVPixelBufferGetBaseAddress(buffer) {
                let stride = CVPixelBufferGetBytesPerRow(buffer), row = image.width * 4
                image.bgra.withUnsafeBytes { bytes in
                    for y in 0..<image.height {
                        memcpy(base + y * stride, bytes.baseAddress! + y * row, row)
                    }
                }
            }
            CVPixelBufferUnlockBaseAddress(buffer, [])
            guard adaptor.append(buffer, withPresentationTime: CMTime(value: Int64(i), timescale: Int32(fps))) else {
                fail("The video could not be written")
                return false
            }
            return true
        }
    }
}
