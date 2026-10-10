import CoreMedia
import CoreVideo
import Foundation
import Synchronization
import VideoToolbox

/// Why a `TileDecoder` action did not produce a picture.
public enum DecodeFailure: Sendable, Equatable {
    /// Configure got parameter sets that do not describe a stream.
    case invalidParameterSets
    /// VideoToolbox could not create a session for the format.
    case sessionCreateFailed(OSStatus)
    /// Decode arrived with no decoder configured.
    case notConfigured
    /// The access unit had no slice data, or no sample could be built from it.
    case invalidAccessUnit
    /// VideoToolbox refused the frame when it was submitted.
    case submitFailed(OSStatus)
    /// VideoToolbox reported an error for the frame.
    case decodeError(OSStatus)
    /// VideoToolbox finished the frame without producing a picture.
    case noPicture
}

/// Results reported to the pipeline (RENDERING.md: `decoded(pts)` and
/// `decode_failed(kind)`).
public enum TileDecoderEvent: Sendable, Equatable {
    case decoded(ptsMicros: Int64)
    case failed(DecodeFailure)
}

/// A decoded picture held for drawing.
public struct DecodedPicture: @unchecked Sendable {
    // @unchecked Sendable: the pixel buffer is never written after the
    // decoder hands it over; it is only read by the GPU.
    public let pixelBuffer: CVPixelBuffer
    public let ptsMicros: Int64
    /// Increases with every change to the held picture (a new picture or a
    /// Clear), so a renderer can tell whether anything changed.
    public let sequence: UInt64

    public init(pixelBuffer: CVPixelBuffer, ptsMicros: Int64, sequence: UInt64) {
        self.pixelBuffer = pixelBuffer
        self.ptsMicros = ptsMicros
        self.sequence = sequence
    }

    public var width: Int { CVPixelBufferGetWidth(pixelBuffer) }
    public var height: Int { CVPixelBufferGetHeight(pixelBuffer) }
}

/// Anything a `GridRenderer` tile can draw from.
public protocol TilePictureSource: AnyObject, Sendable {
    /// The newest picture, or nil to draw the tile black.
    func latestPicture() -> DecodedPicture?
    /// The `sequence` the next `latestPicture()` would return, without
    /// retaining anything; 0 when there has never been a picture.
    var pictureSequence: UInt64 { get }
}

/// Counters for one decoder.
public struct TileDecoderStats: Sendable, Equatable {
    public var sessionsCreated = 0
    public var sessionsInvalidated = 0
    public var submitted = 0
    public var decoded = 0
    public var failed = 0
    /// Pictures that finished after a Flush or Reset and were thrown away.
    public var discarded = 0

    public var liveSessions: Int { sessionsCreated - sessionsInvalidated }
}

/// Executes the decoder actions of RENDERING.md (Configure, Decode, Flush,
/// Reset, Clear) with one `VTDecompressionSession`, and holds the newest
/// decoded picture for the renderer. It makes no decisions: what to do after
/// a failure, when to wait for a keyframe, and which color matrix applies are
/// the pipeline's business.
///
/// Threading: actions may come from any thread, one at a time (the core
/// serializes them per tile). Decoding is asynchronous; `onEvent` is called on
/// a VideoToolbox thread (or, for a failure found while executing an action,
/// on the caller's thread after the action's work is done). The handler must
/// return quickly and must not run this decoder's actions itself: it reports
/// to the pipeline, which answers with actions on its own thread.
/// `latestPicture()` may be called from any thread at any time.
///
/// Pictures still in flight when Flush, Reset, or Configure runs are
/// discarded without an event.
public final class TileDecoder: TilePictureSource {
    public typealias EventHandler = @Sendable (TileDecoderEvent) -> Void

    // @unchecked Sendable: only reached through `state`'s lock; VideoToolbox
    // sessions and format descriptions are safe to use from any thread.
    private struct Session: @unchecked Sendable {
        var codec: VideoCodec = .h264
        var session: VTDecompressionSession?
        var format: CMVideoFormatDescription?
    }

    private let output: Output
    private let state = Mutex(Session())

    public init(onEvent: @escaping EventHandler) {
        output = Output(onEvent: onEvent)
    }

    deinit {
        state.withLock { s in
            if let session = s.session {
                // The callback refers to `output`, which goes away with us:
                // let every frame finish first.
                VTDecompressionSessionWaitForAsynchronousFrames(session)
                VTDecompressionSessionInvalidate(session)
            }
        }
    }

    public var stats: TileDecoderStats { output.stats }

    /// The live session, for tests that check sessions are released.
    var sessionForTesting: VTDecompressionSession? { state.withLock { $0.session } }

    // MARK: Actions

    /// Creates a decoder for a format, replacing any existing one.
    /// `parameterSets` are NAL units without start codes (H.264 SPS and PPS,
    /// H.265 VPS, SPS and PPS). `fullRange` selects the output pixel format
    /// to match the stream (RENDERING.md `color`), so no range conversion
    /// happens before the shader. Failures are reported through `onEvent`.
    public func configure(codec: VideoCodec, parameterSets: [Data], fullRange: Bool) {
        var tracker = ParameterSetTracker(codec: codec)
        var dirty = false
        for nal in parameterSets where !nal.isEmpty {
            let header = nal[nal.startIndex]
            let type = codec == .h264 ? header & 0x1F : (header >> 1) & 0x3F
            if tracker.record(type: type, nal: nal) { dirty = true }
        }
        _ = tracker.rebuildIfNeeded(dirty: dirty)

        let failure = state.withLock { s -> DecodeFailure? in
            invalidate(&s)
            s.codec = codec
            guard let format = tracker.formatDescription else { return .invalidParameterSets }
            var record = VTDecompressionOutputCallbackRecord(
                decompressionOutputCallback: decompressionOutput,
                decompressionOutputRefCon: Unmanaged.passUnretained(output).toOpaque())
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: fullRange
                    ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
                    : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                kCVPixelBufferMetalCompatibilityKey: true,
                kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any](),
            ]
            let specification: [CFString: Any] = [
                kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true
            ]
            var session: VTDecompressionSession?
            let status = VTDecompressionSessionCreate(
                allocator: kCFAllocatorDefault, formatDescription: format,
                decoderSpecification: specification as CFDictionary,
                imageBufferAttributes: attributes as CFDictionary,
                outputCallback: &record, decompressionSessionOut: &session)
            guard status == noErr, let session else { return .sessionCreateFailed(status) }
            VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
            output.sessionCreated()
            s.session = session
            s.format = format
            return nil
        }
        if let failure { output.report(.failed(failure)) }
    }

    /// Submits one access unit, Annex-B as the device sent it. The bytes are
    /// copied (as length-prefixed NAL units, parameter sets and other non-slice
    /// NAL units left out) before this returns; decoding is asynchronous.
    public func decode(annexB: UnsafeRawBufferPointer, keyframe: Bool, ptsMicros: Int64) {
        let failure = state.withLock { s -> DecodeFailure? in
            guard let session = s.session, let format = s.format else { return .notConfigured }
            let codec = s.codec
            let slices = AnnexB.split(annexB, codec: codec).filter { NALType.isVCL($0.type, codec: codec) }
            guard let sample = SampleBuilder.makeSample(
                nals: slices, from: annexB, format: format, ptsMicros: ptsMicros, isKeyframe: keyframe)
            else { return .invalidAccessUnit }
            let epoch = output.submitted()
            let status = VTDecompressionSessionDecodeFrame(
                session, sampleBuffer: sample, flags: [._EnableAsynchronousDecompression],
                frameRefcon: UnsafeMutableRawPointer(bitPattern: UInt(epoch)), infoFlagsOut: nil)
            return status == noErr ? nil : .submitFailed(status)
        }
        if let failure { output.report(.failed(failure)) }
    }

    public func decode(annexB: Data, keyframe: Bool, ptsMicros: Int64) {
        annexB.withUnsafeBytes { decode(annexB: $0, keyframe: keyframe, ptsMicros: ptsMicros) }
    }

    /// Drops everything in flight and keeps the decoder.
    public func flush() {
        state.withLock { s in
            output.discardInFlight()
            if let session = s.session { VTDecompressionSessionFinishDelayedFrames(session) }
        }
    }

    /// Destroys the decoder; a Configure must follow before any Decode.
    public func reset() {
        state.withLock { invalidate(&$0) }
    }

    /// Drops the held picture, so the tile draws black until a new picture
    /// decodes.
    public func clear() {
        output.clearPicture()
    }

    /// Blocks until every submitted frame has been reported. Not an action:
    /// for tests and orderly shutdown.
    public func waitForPendingFrames() {
        state.withLock { s in
            if let session = s.session { VTDecompressionSessionWaitForAsynchronousFrames(session) }
        }
    }

    // MARK: Picture slot

    public func latestPicture() -> DecodedPicture? { output.latestPicture() }
    public var pictureSequence: UInt64 { output.pictureSequence }

    private func invalidate(_ s: inout Session) {
        output.discardInFlight()
        if let session = s.session {
            VTDecompressionSessionInvalidate(session)
            output.sessionInvalidated()
        }
        s.session = nil
        s.format = nil
    }
}

/// The state VideoToolbox's callback touches. Every decoder session's
/// callback gets this object as its refcon; the decoder outlives its sessions.
private final class Output: Sendable {
    // @unchecked Sendable: only reached through `slot`'s lock; the pixel
    // buffer is never written after VideoToolbox hands it over.
    private struct Slot: @unchecked Sendable {
        var picture: CVPixelBuffer?
        var ptsMicros: Int64 = 0
        var sequence: UInt64 = 0
        /// Frames carry the epoch they were submitted in; Flush, Reset, and
        /// Configure start a new epoch, and results from older ones are dropped.
        var epoch: UInt64 = 1
        var stats = TileDecoderStats()
    }

    private let slot = Mutex(Slot())
    private let sequence = Atomic<UInt64>(0)
    private let onEvent: TileDecoder.EventHandler

    init(onEvent: @escaping TileDecoder.EventHandler) {
        self.onEvent = onEvent
    }

    var stats: TileDecoderStats { slot.withLock { $0.stats } }
    var pictureSequence: UInt64 { sequence.load(ordering: .acquiring) }

    func latestPicture() -> DecodedPicture? {
        slot.withLock { s in
            s.picture.map { DecodedPicture(pixelBuffer: $0, ptsMicros: s.ptsMicros, sequence: s.sequence) }
        }
    }

    func report(_ event: TileDecoderEvent) {
        if case .failed = event { slot.withLock { $0.stats.failed += 1 } }
        onEvent(event)
    }

    func sessionCreated() { slot.withLock { $0.stats.sessionsCreated += 1 } }
    func sessionInvalidated() { slot.withLock { $0.stats.sessionsInvalidated += 1 } }

    /// Counts a submission; returns the epoch to tag it with.
    func submitted() -> UInt64 {
        slot.withLock { s in
            s.stats.submitted += 1
            return s.epoch
        }
    }

    func discardInFlight() { slot.withLock { $0.epoch &+= 1 } }

    func clearPicture() {
        slot.withLock { s in
            s.picture = nil
            bump(&s)
        }
    }

    private func bump(_ s: inout Slot) {
        s.sequence += 1
        sequence.store(s.sequence, ordering: .releasing)
    }

    /// Called by VideoToolbox, on its own thread, once per submitted frame.
    func receive(epoch: UInt64, status: OSStatus, flags: VTDecodeInfoFlags, image: CVImageBuffer?, pts: CMTime) {
        let event = slot.withLock { s -> TileDecoderEvent? in
            guard epoch == s.epoch else {
                s.stats.discarded += 1
                return nil
            }
            guard status == noErr else {
                s.stats.failed += 1
                return .failed(.decodeError(status))
            }
            guard let image, !flags.contains(.frameDropped) else {
                s.stats.failed += 1
                return .failed(.noPicture)
            }
            let micros = pts.isValid ? CMTimeConvertScale(pts, timescale: 1_000_000, method: .default).value : 0
            // Newest wins: the previous picture is released here.
            s.picture = image
            s.ptsMicros = micros
            s.stats.decoded += 1
            bump(&s)
            return .decoded(ptsMicros: micros)
        }
        if let event { onEvent(event) }
    }
}

private func decompressionOutput(
    refCon: UnsafeMutableRawPointer?, frameRefCon: UnsafeMutableRawPointer?,
    status: OSStatus, flags: VTDecodeInfoFlags, image: CVImageBuffer?,
    pts: CMTime, duration: CMTime
) {
    guard let refCon else { return }
    let output = Unmanaged<Output>.fromOpaque(refCon).takeUnretainedValue()
    output.receive(
        epoch: UInt64(UInt(bitPattern: frameRefCon)), status: status, flags: flags, image: image, pts: pts)
}
