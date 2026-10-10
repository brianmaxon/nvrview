import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

public enum DecodeError: Error, CustomStringConvertible {
    case formatDescription(OSStatus)
    case session(OSStatus)
    case sampleBuffer(OSStatus)
    case decode(OSStatus)

    public var description: String {
        switch self {
        case .formatDescription(let s): return "could not build a format description (OSStatus \(s))"
        case .session(let s): return "could not create a decompression session (OSStatus \(s))"
        case .sampleBuffer(let s): return "could not build a sample buffer (OSStatus \(s))"
        case .decode(let s): return "decode failed (OSStatus \(s))"
        }
    }
}

/// Decodes access units with VideoToolbox, synchronously, one at a time.
///
/// Output pixel buffers are IOSurface-backed NV12 (4:2:0 bi-planar, video
/// range), the format the hardware decoder produces natively, so nothing is
/// converted or copied between the decoder and Vision.
public final class Decoder {
    public let codec: Codec
    public let requireHardware: Bool
    private var session: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var currentParams: [Data] = []
    /// True once a session was created with hardware decoding confirmed.
    public private(set) var usingHardware = false

    public init(codec: Codec, requireHardware: Bool = true) {
        self.codec = codec
        self.requireHardware = requireHardware
    }

    deinit {
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
        }
    }

    /// Decodes one picture. Returns nil when the picture cannot be decoded
    /// yet (no parameter sets or no keyframe since a format change).
    public func decode(_ unit: AccessUnit) throws -> CVPixelBuffer? {
        if unit.parameterSets != currentParams {
            guard unit.isKeyframe else { return nil }  // keyframe gating
            try makeSession(unit.parameterSets)
        }
        guard let session, let format else { return nil }

        var avcc = Data()
        for nal in unit.slices {
            var length = UInt32(nal.bytes.count).bigEndian
            avcc.append(Data(bytes: &length, count: 4))
            avcc.append(nal.bytes)
        }

        var block: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
            dataLength: avcc.count, flags: 0, blockBufferOut: &block)
        guard status == noErr, let block else { throw DecodeError.sampleBuffer(status) }
        status = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(
                with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: avcc.count)
        }
        guard status == noErr else { throw DecodeError.sampleBuffer(status) }

        var sample: CMSampleBuffer?
        var size = avcc.count
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample)
        guard status == noErr, let sample else { throw DecodeError.sampleBuffer(status) }

        let box = OutputBox()
        var info = VTDecodeInfoFlags()
        status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: [], infoFlagsOut: &info
        ) { decodeStatus, _, imageBuffer, _, _ in
            box.status = decodeStatus
            box.image = imageBuffer
        }
        guard status == noErr else { throw DecodeError.decode(status) }
        guard box.status == noErr else { throw DecodeError.decode(box.status) }
        return box.image
    }

    private func makeSession(_ params: [Data]) throws {
        if let session {
            VTDecompressionSessionWaitForAsynchronousFrames(session)
            VTDecompressionSessionInvalidate(session)
            self.session = nil
        }
        var fmt: CMFormatDescription?
        let status: OSStatus = withPointers(params) { pointers, sizes in
            switch codec {
            case .h264:
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: params.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &fmt)
            case .h265:
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: params.count,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fmt)
            }
        }
        guard status == noErr, let fmt else { throw DecodeError.formatDescription(status) }

        let spec: [CFString: Any] = [
            kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true,
            kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: requireHardware,
        ]
        let attributes: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any],
            kCVPixelBufferMetalCompatibilityKey: true,
        ]
        var newSession: VTDecompressionSession?
        let s = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: fmt,
            decoderSpecification: spec as CFDictionary,
            imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil, decompressionSessionOut: &newSession)
        guard s == noErr, let newSession else { throw DecodeError.session(s) }

        var hw: CFBoolean?
        let hs = withUnsafeMutablePointer(to: &hw) {
            VTSessionCopyProperty(
                newSession, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                allocator: kCFAllocatorDefault, valueOut: UnsafeMutableRawPointer($0))
        }
        usingHardware = hs == noErr && hw.map { CFBooleanGetValue($0) } == true

        session = newSession
        format = fmt
        currentParams = params
    }
}

/// Receives the synchronous decode callback's result.
private final class OutputBox: @unchecked Sendable {
    var status: OSStatus = noErr
    var image: CVImageBuffer?
}

private func withPointers<R>(
    _ params: [Data], _ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> R
) -> R {
    let copies = params.map { d -> UnsafeMutablePointer<UInt8> in
        let p = UnsafeMutablePointer<UInt8>.allocate(capacity: max(d.count, 1))
        d.copyBytes(to: p, count: d.count)
        return p
    }
    defer { copies.forEach { $0.deallocate() } }
    let pointers = copies.map { UnsafePointer($0) }
    let sizes = params.map(\.count)
    return pointers.withUnsafeBufferPointer { pp in
        sizes.withUnsafeBufferPointer { sp in body(pp.baseAddress!, sp.baseAddress!) }
    }
}
