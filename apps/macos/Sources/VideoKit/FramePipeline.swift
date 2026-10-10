import CoreMedia
import Foundation

/// One frame as the core delivers it: an access unit in Annex-B form.
public struct VideoFrame: Sendable {
    public var codec: VideoCodec
    /// The core's keyframe flag. The pipeline trusts the NAL types over this
    /// flag, so a mislabeled frame can never open the gate on a non-IRAP slice.
    public var isKeyframe: Bool
    /// Presentation timestamp in microseconds.
    public var ptsMicros: Int64
    public var annexB: Data

    public init(codec: VideoCodec, isKeyframe: Bool, ptsMicros: Int64, annexB: Data) {
        self.codec = codec
        self.isKeyframe = isKeyframe
        self.ptsMicros = ptsMicros
        self.annexB = annexB
    }
}

public enum DropReason: Sendable, Equatable {
    /// Waiting for a keyframe with parameter sets (start, flush, format change).
    case waitingForKeyframe
    /// The frame contained only parameter sets, SEI, or delimiters.
    case noPictureData
    /// Sample buffer creation failed.
    case buildFailed
}

public enum PipelineOutput: @unchecked Sendable {
    /// A decodable sample. `formatChanged` is true when its format description
    /// differs from the previous sample's (the display path must flush first).
    case sample(CMSampleBuffer, formatChanged: Bool)
    case dropped(DropReason)
}

/// The pure, synchronous heart of the video path: keyframe gating, parameter
/// set tracking, and conversion of Annex-B frames to `CMSampleBuffer`s.
/// Not thread-safe by itself; the owner serializes access.
public struct FramePipeline: Sendable {
    public let codec: VideoCodec
    private var tracker: ParameterSetTracker
    private var gateOpen = false
    /// Format description of the last emitted sample.
    private var activeFormat: CMVideoFormatDescription?

    public init(codec: VideoCodec) {
        self.codec = codec
        self.tracker = ParameterSetTracker(codec: codec)
    }

    public var isGateOpen: Bool { gateOpen }

    /// Closes the gate: everything is dropped until the next keyframe that
    /// arrives with (or after) complete parameter sets. Held parameter sets are
    /// kept unless `forgetParameterSets` is set.
    public mutating func closeGate(forgetParameterSets: Bool = false) {
        gateOpen = false
        activeFormat = nil
        if forgetParameterSets { tracker.reset() }
    }

    /// Supplies parameter sets out of band (NAL units without start codes), as
    /// carried by the core's format-changed event.
    public mutating func setParameterSets(_ nals: [Data]) {
        var dirty = false
        for nal in nals where !nal.isEmpty {
            let type: UInt8 = codec == .h264 ? nal[nal.startIndex] & 0x1F : (nal[nal.startIndex] >> 1) & 0x3F
            if tracker.record(type: type, nal: nal) { dirty = true }
        }
        _ = tracker.rebuildIfNeeded(dirty: dirty)
    }

    public mutating func process(_ frame: VideoFrame) -> PipelineOutput {
        let nals = AnnexB.split(frame.annexB, codec: codec)
        var dirty = false
        var hasKey = false
        var vcl: [NALUnit] = []
        for nal in nals {
            if NALType.isParameterSet(nal.type, codec: codec) {
                let bytes = frame.annexB.subdata(in: nal.range)
                if tracker.record(type: nal.type, nal: bytes) { dirty = true }
            } else if NALType.isVCL(nal.type, codec: codec) {
                vcl.append(nal)
                if NALType.isKeyframe(nal.type, codec: codec) { hasKey = true }
            }
        }
        _ = tracker.rebuildIfNeeded(dirty: dirty)

        guard !vcl.isEmpty else { return .dropped(.noPictureData) }

        if !gateOpen {
            guard hasKey, tracker.formatDescription != nil else {
                return .dropped(.waitingForKeyframe)
            }
        }
        guard let format = tracker.formatDescription else { return .dropped(.waitingForKeyframe) }

        var changed = false
        if let active = activeFormat {
            if !CMFormatDescriptionEqual(active, otherFormatDescription: format) {
                // New parameter sets must take effect at a keyframe.
                guard hasKey else {
                    closeGate()
                    return .dropped(.waitingForKeyframe)
                }
                changed = true
            }
        }
        guard let sample = SampleBuilder.makeSample(
            nals: vcl, from: frame.annexB, format: format,
            ptsMicros: frame.ptsMicros, isKeyframe: hasKey)
        else { return .dropped(.buildFailed) }

        gateOpen = true
        activeFormat = format
        return .sample(sample, formatChanged: changed)
    }
}

enum SampleBuilder {
    static func makeSample(
        nals: [NALUnit], from data: Data, format: CMVideoFormatDescription,
        ptsMicros: Int64, isKeyframe: Bool
    ) -> CMSampleBuffer? {
        let payload = nals.reduce(0) { $0 + 4 + $1.range.count }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: payload,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: payload, flags: 0, blockBufferOut: &block) == noErr,
            let block
        else { return nil }
        guard CMBlockBufferAssureBlockMemory(block) == noErr else { return nil }

        var dest: UnsafeMutablePointer<CChar>?
        guard CMBlockBufferGetDataPointer(
            block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: nil,
            dataPointerOut: &dest) == noErr,
            let dest
        else { return nil }
        let out = UnsafeMutableRawPointer(dest)
        var offset = 0
        data.withUnsafeBytes { src in
            for nal in nals {
                let length = UInt32(nal.range.count).bigEndian
                out.storeBytes(of: length, toByteOffset: offset, as: UInt32.self)
                offset += 4
                (out + offset).copyMemory(
                    from: src.baseAddress! + nal.range.lowerBound, byteCount: nal.range.count)
                offset += nal.range.count
            }
        }

        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: ptsMicros, timescale: 1_000_000),
            decodeTimeStamp: .invalid)
        var size = payload
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample) == noErr,
            let sample
        else { return nil }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
            CFArrayGetCount(attachments) > 0
        {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            if !isKeyframe {
                CFDictionarySetValue(
                    dict, Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
                    Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
            }
            // Live video: show as soon as decoded, no timebase needed.
            CFDictionarySetValue(
                dict, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}
