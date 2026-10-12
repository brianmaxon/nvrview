import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import Synchronization

extension YCbCrToRGB {
    /// The matrix for luma coefficients `kr` and `kb` over 8-bit samples
    /// read as code / 255 (the type's convention), video or full range.
    public static func standard(kr: Float, kb: Float, fullRange: Bool) -> YCbCrToRGB {
        let kg = 1 - kr - kb
        let ys: Float = fullRange ? 1 : 255 / 219
        let yo: Float = fullRange ? 0 : -16 / 219
        let cs: Float = fullRange ? 1 : 255 / 224
        let co: Float = fullRange ? -128 / 255 : -128 / 224
        let crR = 2 * (1 - kr)
        let cbB = 2 * (1 - kb)
        let cbG = -2 * kb * (1 - kb) / kg
        let crG = -2 * kr * (1 - kr) / kg
        func row(_ cbK: Float, _ crK: Float) -> SIMD4<Float> {
            SIMD4(ys, cbK * cs, crK * cs, yo + (cbK + crK) * co)
        }
        return YCbCrToRGB(r: row(0, crR), g: row(cbG, crG), b: row(cbB, 0))
    }

    public static func bt601(fullRange: Bool) -> YCbCrToRGB { standard(kr: 0.299, kb: 0.114, fullRange: fullRange) }
    public static func bt709(fullRange: Bool) -> YCbCrToRGB { standard(kr: 0.2126, kb: 0.0722, fullRange: fullRange) }
}

public enum RecordedFileError: Error, Equatable {
    case noVideo
    case unreadable(String)
}

/// A finished recording (an MP4 a download wrote) as a picture source for
/// `GridRenderer`: its video track decoded by VideoToolbox through
/// `AVAssetReader`, positioned by file time. Seeking is frame accurate: the
/// reader starts at the keyframe at or before the position (read from the
/// file's sync samples) and decodes forward to the picture shown at that
/// position; moving forward within a group of pictures only decodes on.
/// With `keyframesOnly` (fast speeds, dragging) only keyframes are decoded.
///
/// Positions are milliseconds on the file's presentation timeline (the
/// edit list applied), the same timeline `recordings::sync_samples` reads,
/// so a plan's `source_in_ms` addresses the same picture here.
///
/// `want` decodes on the source's own serial queue, coalescing requests (the
/// newest wins, so nothing queues up behind a slow decode); `show` decodes on
/// the caller's thread, for the self-test and export. At most two decoded
/// pictures are held: the one shown and the next one.
public final class RecordedFileSource: TilePictureSource, @unchecked Sendable {
    // @unchecked Sendable: the reader state is touched only on `queue`; the
    // shown picture and the pending request are behind Mutexes.

    /// What the file holds.
    public struct Info: Sendable, Equatable {
        public var durationMs: Int64
        public var width: Int
        public var height: Int
        public var fullRange: Bool
        public var color: YCbCrToRGB
        /// Keyframe positions in milliseconds, ascending (empty when the
        /// file does not say; every seek then starts from the beginning).
        public var keyframesMs: [Int64]
        /// "h264", "h265", or the four-character code.
        public var codec: String
    }

    /// Most samples whose sync flags are read.
    public static let maxSamplesScanned = 500_000

    public let url: URL
    public let info: Info
    private let asset: AVURLAsset
    private let track: AVAssetTrack
    private let queue: DispatchQueue

    private struct Slot {
        var picture: DecodedPicture?
        var positionMs: Int64?
        var sequence: UInt64 = 0
    }

    private struct Want {
        var target: (ms: Int64, keyframesOnly: Bool)?
        var busy = false
    }

    private let slot = Mutex(Slot())
    /// Picture sequences are unique across every file: a renderer tile that
    /// switches from one file to another (a follow cut) then always sees a
    /// new sequence and takes the new file's picture.
    private static let sequences = Atomic<UInt64>(0)
    private static func nextSequence() -> UInt64 { sequences.wrappingAdd(1, ordering: .relaxed).newValue }
    private let wanted = Mutex(Want())

    // Reader state, on `queue` only.
    private var reader: AVAssetReader?
    private var output: AVAssetReaderTrackOutput?
    private var shownMs: Int64?
    private var next: (buffer: CVPixelBuffer, ms: Int64)?
    private var readerEnded = false
    /// Readers started (the self-test and tests count seeks with it).
    private let counters = Mutex((readers: 0, decoded: 0))

    private init(url: URL, asset: AVURLAsset, track: AVAssetTrack, info: Info) {
        self.url = url
        self.asset = asset
        self.track = track
        self.info = info
        queue = DispatchQueue(label: "nvrview.recorded-file", qos: .userInitiated)
    }

    /// Opens a file and reads its video track's description and keyframes.
    public static func open(_ url: URL) async throws(RecordedFileError) -> RecordedFileSource {
        let asset = AVURLAsset(url: url, options: [AVURLAssetPreferPreciseDurationAndTimingKey: true])
        do {
            guard let track = try await asset.loadTracks(withMediaType: .video).first else { throw RecordedFileError.noVideo }
            let (formats, size) = try await track.load(.formatDescriptions, .naturalSize)
            let duration = try await asset.load(.duration)
            let format = formats.first
            let fullRange =
                format.flatMap { CMFormatDescriptionGetExtension($0, extensionKey: kCMFormatDescriptionExtension_FullRangeVideo) as? Bool }
                ?? false
            let matrix = format.flatMap {
                CMFormatDescriptionGetExtension($0, extensionKey: kCMFormatDescriptionExtension_YCbCrMatrix) as? String
            }
            let height = Int(size.height.rounded())
            // RENDERING.md's default without a color description: BT.709 at
            // 720 lines and above, BT.601 below.
            let bt709 =
                matrix.map { $0 == (kCMFormatDescriptionYCbCrMatrix_ITU_R_709_2 as String) } ?? (height >= 720)
            let codec: String = {
                guard let f = format else { return "" }
                switch CMFormatDescriptionGetMediaSubType(f) {
                case kCMVideoCodecType_H264: return "h264"
                case kCMVideoCodecType_HEVC: return "h265"
                case let t:
                    let bytes = [24, 16, 8, 0].map { UInt8(truncatingIfNeeded: t >> $0) }
                    return String(decoding: bytes, as: UTF8.self)
                }
            }()
            let info = Info(
                durationMs: Self.ms(duration), width: Int(size.width.rounded()), height: height, fullRange: fullRange,
                color: bt709 ? .bt709(fullRange: fullRange) : .bt601(fullRange: fullRange),
                keyframesMs: Self.keyframes(track, cursors: (try? await track.load(.canProvideSampleCursors)) ?? false), codec: codec)
            return RecordedFileSource(url: url, asset: asset, track: track, info: info)
        } catch let e as RecordedFileError {
            throw e
        } catch {
            throw .unreadable(String(describing: error))
        }
    }

    /// The sync samples' presentation times, from the sample table (no
    /// decoding), bounded by `maxSamplesScanned`.
    private static func keyframes(_ track: AVAssetTrack, cursors: Bool) -> [Int64] {
        guard cursors, let cursor = track.makeSampleCursorAtFirstSampleInDecodeOrder() else { return [] }
        var out: [Int64] = []
        var scanned = 0
        repeat {
            if cursor.currentSampleSyncInfo.sampleIsFullSync.boolValue { out.append(ms(cursor.presentationTimeStamp)) }
            scanned += 1
        } while scanned < maxSamplesScanned && cursor.stepInDecodeOrder(byCount: 1) == 1
        return out.sorted()
    }

    static func ms(_ t: CMTime) -> Int64 {
        guard t.isNumeric else { return 0 }
        return Int64((CMTimeGetSeconds(t) * 1000).rounded())
    }

    // MARK: TilePictureSource

    public func latestPicture() -> DecodedPicture? { slot.withLock { $0.picture } }
    public var pictureSequence: UInt64 { slot.withLock { $0.sequence } }

    /// The file position of the picture held, nil before the first.
    public var shownPositionMs: Int64? { slot.withLock { $0.positionMs } }

    /// Readers started and pictures decoded so far.
    public var stats: (readers: Int, decoded: Int) { counters.withLock { $0 } }

    // MARK: Positioning

    /// The keyframe at or before `ms` (the first one before the first).
    public func keyframe(atOrBefore ms: Int64) -> Int64 {
        let k = info.keyframesMs
        guard !k.isEmpty else { return 0 }
        var lo = 0, hi = k.count - 1, found = k[0]
        while lo <= hi {
            let mid = (lo + hi) / 2
            if k[mid] <= ms {
                found = k[mid]
                lo = mid + 1
            } else {
                hi = mid - 1
            }
        }
        return found
    }

    /// Shows the picture at `ms`, decoded on the source's queue; a newer
    /// request replaces one not yet started.
    public func want(_ ms: Int64, keyframesOnly: Bool = false) {
        let start = wanted.withLock { w -> Bool in
            w.target = (ms, keyframesOnly)
            if w.busy { return false }
            w.busy = true
            return true
        }
        if start { queue.async { self.drain() } }
    }

    /// Shows the picture at `ms` before returning.
    public func show(_ ms: Int64, keyframesOnly: Bool = false) {
        queue.sync { seek(ms, keyframesOnly: keyframesOnly) }
    }

    /// Drops the picture (the tile draws black) and the reader.
    public func close() {
        queue.sync {
            reader?.cancelReading()
            reader = nil
            output = nil
            next = nil
            shownMs = nil
        }
        slot.withLock { s in
            s.picture = nil
            s.positionMs = nil
            s.sequence = Self.nextSequence()
        }
    }

    private func drain() {
        while let t = wanted.withLock({ w -> (ms: Int64, keyframesOnly: Bool)? in
            let t = w.target
            w.target = nil
            if t == nil { w.busy = false }
            return t
        }) {
            seek(t.ms, keyframesOnly: t.keyframesOnly)
        }
    }

    /// On `queue`.
    private func seek(_ requested: Int64, keyframesOnly: Bool) {
        let ms = min(max(requested, 0), max(info.durationMs - 1, 0))
        let k = keyframe(atOrBefore: ms)
        if keyframesOnly {
            guard shownMs != k else { return }
            restart(at: k)
            if let f = read() { publish(f.buffer, f.ms) }
            return
        }
        // Restart when going back, before the first picture, or when a
        // keyframe lies between the picture shown and the target (decoding
        // from there is no more work than decoding on).
        if let shown = shownMs, reader != nil, shown <= ms, k <= shown {
            // Decode on from the picture shown.
        } else {
            restart(at: k)
        }
        // The last picture at or before the target; right after a restart
        // the first picture is taken even if it is later (better than black).
        var best: (buffer: CVPixelBuffer, ms: Int64)?
        while true {
            let f: (buffer: CVPixelBuffer, ms: Int64)
            if let n = next {
                f = n
                next = nil
            } else if let r = read() {
                f = r
            } else {
                break
            }
            if f.ms <= ms || (best == nil && shownMs == nil) {
                best = f
                if f.ms >= ms { break }
            } else {
                next = f
                break
            }
        }
        if let best { publish(best.buffer, best.ms) }
    }

    private func restart(at k: Int64) {
        reader?.cancelReading()
        reader = nil
        output = nil
        next = nil
        shownMs = nil
        readerEnded = false
        guard let r = try? AVAssetReader(asset: asset) else { return }
        let format = info.fullRange ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        let o = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [
                kCVPixelBufferPixelFormatTypeKey as String: format,
                kCVPixelBufferMetalCompatibilityKey as String: true,
                kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
            ])
        o.alwaysCopiesSampleData = false
        guard r.canAdd(o) else { return }
        r.add(o)
        r.timeRange = CMTimeRange(start: CMTime(value: k, timescale: 1000), end: .positiveInfinity)
        guard r.startReading() else { return }
        reader = r
        output = o
        counters.withLock { $0.readers += 1 }
    }

    private func read() -> (buffer: CVPixelBuffer, ms: Int64)? {
        guard let o = output, !readerEnded else { return nil }
        while let sample = o.copyNextSampleBuffer() {
            guard let image = CMSampleBufferGetImageBuffer(sample) else { continue }
            counters.withLock { $0.decoded += 1 }
            return (image, Self.ms(CMSampleBufferGetPresentationTimeStamp(sample)))
        }
        readerEnded = true
        return nil
    }

    private func publish(_ buffer: CVPixelBuffer, _ ms: Int64) {
        shownMs = ms
        slot.withLock { s in
            s.sequence = Self.nextSequence()
            s.positionMs = ms
            s.picture = DecodedPicture(pixelBuffer: buffer, ptsMicros: ms * 1000, sequence: s.sequence)
        }
    }
}
