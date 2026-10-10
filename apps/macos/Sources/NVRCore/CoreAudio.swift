import Foundation

// Listening (docs/AUDIO.md, docs/FFI.md "Audio") as Swift sees it. Like the
// rest of NVRCore this calls no C function: `CCore` supplies the real render
// hook, `FakeCore` a synthetic one.

/// The function a real-time audio callback calls to fill its buffer:
/// `nv_audio_render`'s shape, with the core handle as `context`.
///
/// Both members are plain values (a C function pointer and a raw pointer),
/// so a render block that captures them does no reference counting when it
/// runs. Valid until the core is freed; stop rendering before that.
public struct AudioRenderHook: @unchecked Sendable {
    public typealias Function = @convention(c) (
        _ context: UnsafeMutableRawPointer?, _ out: UnsafeMutablePointer<Float>?, _ frames: UInt32,
        _ channels: UInt32, _ hostTimeNanos: UInt64
    ) -> Int32

    public let function: Function
    public let context: UnsafeMutableRawPointer?

    public init(function: Function, context: UnsafeMutableRawPointer?) {
        self.function = function
        self.context = context
    }

    /// Renders silence (no core).
    public static let silent = AudioRenderHook(
        function: { _, out, frames, channels, _ in
            out?.update(repeating: 0, count: Int(frames) * Int(channels))
            return -3  // NV_ERR_CLOSED
        }, context: nil)
}

/// One block of a tile's tapped audio (`NvAudioBlock`): 16 kHz mono. The
/// samples are borrowed for the duration of the callback.
public struct CoreAudioBlock {
    public var samples: UnsafeBufferPointer<Float>
    public var sampleRate: UInt32
    public var epoch: UInt32
    public var firstSample: UInt64
    /// Camera time (video timestamp units) of the first sample, once known.
    public var cameraTimeMicros: Int64?
    /// Arrival of the record that completed the block, µs since 1970.
    public var wallClockMicros: Int64?
    public var discontinuity: Bool

    public init(
        samples: UnsafeBufferPointer<Float>, sampleRate: UInt32 = 16_000, epoch: UInt32, firstSample: UInt64,
        cameraTimeMicros: Int64?, wallClockMicros: Int64?, discontinuity: Bool
    ) {
        self.samples = samples
        self.sampleRate = sampleRate
        self.epoch = epoch
        self.firstSample = firstSample
        self.cameraTimeMicros = cameraTimeMicros
        self.wallClockMicros = wallClockMicros
        self.discontinuity = discontinuity
    }
}

/// The parts of an `audio_stats` event the app shows.
public struct AudioStatsEvent: Sendable, Equatable {
    public var records = 0
    public var decoded = 0
    public var audible = false
    public var tapped = false
    public var codec: String?
    public var playoutDelayMilliseconds: Double?
    public var playoutDelayTargetMilliseconds: Double?
    public var avOffsetMilliseconds: Double?
    public var underruns = 0
    public var resyncs = 0
    public var tapDropped = 0

    public init() {}
}

// Defaults for cores without audio (test doubles that only record).
extension CoreAPI {
    public func audioSetOutput(sampleRate: UInt32, channels: UInt32) throws(CoreStatus) { throw .invalidArgument }
    public func tileAudioSet(_ tile: UInt64, audible: Bool, volume: Float) throws(CoreStatus) { throw .invalidArgument }
    public func tileAudioTap(_ tile: UInt64, enabled: Bool) throws(CoreStatus) { throw .invalidArgument }
    public var audioRenderHook: AudioRenderHook { .silent }
    public func clockNowNanos() -> UInt64 { HostClock.nowNanos() }
}

/// The host clock the core uses (`nv_clock_now_ns`): mach_absolute_time in
/// nanoseconds. Computed here from the mach timebase, so code without the
/// library (and FakeCore) uses the same clock.
public enum HostClock {
    /// numerator and denominator of the mach timebase (ticks to ns).
    public static let timebase: (numer: UInt64, denom: UInt64) = {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        return (UInt64(info.numer), UInt64(max(info.denom, 1)))
    }()

    public static func nanos(hostTicks: UInt64) -> UInt64 {
        let (numer, denom) = timebase
        if numer == denom { return hostTicks }
        let (high, low) = hostTicks.multipliedFullWidth(by: numer)
        return denom.dividingFullWidth((high, low)).quotient
    }

    public static func nowNanos() -> UInt64 { nanos(hostTicks: mach_absolute_time()) }
}
