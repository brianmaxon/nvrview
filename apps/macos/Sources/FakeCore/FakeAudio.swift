import Foundation
import NVRCore
import Synchronization

/// FakeCore's audio: a real-time renderer that plays a click whenever the
/// host time crosses a multiple of `clickPeriodNanos`, at the loudest volume
/// of the audible tiles. Because the clicks are placed by host time, a test
/// can check that whoever calls the render function (the `AudioPlayer`
/// bridge) computes the time each buffer is heard correctly.
///
/// Like the real core's, the render path takes no lock and allocates
/// nothing: it reads atomics and writes the caller's buffer.
public final class FakeAudio: Sendable {
    public static let clickPeriodNanos: UInt64 = 500_000_000
    public static let clickAmplitude: Float = 0.8

    let sampleRate = Atomic<UInt32>(0)
    let channels = Atomic<UInt32>(0)
    /// Gain of the loudest audible tile, as Float bits; 0 when none is.
    let gainBits = Atomic<UInt32>(0)
    let renders = Atomic<Int>(0)
    let silentRenders = Atomic<Int>(0)

    /// Render calls so far, and those that wrote silence (no output set or a
    /// channel count other than the output's).
    public var renderCount: Int { renders.load(ordering: .relaxed) }
    public var silentRenderCount: Int { silentRenders.load(ordering: .relaxed) }

    /// `nv_audio_render`'s behavior.
    func render(_ out: UnsafeMutablePointer<Float>?, frames: UInt32, channels ch: UInt32, host: UInt64) -> Int32 {
        guard let out else { return CoreStatus.invalidArgument.code }
        renders.add(1, ordering: .relaxed)
        let n = Int(frames) * Int(ch)
        let rate = sampleRate.load(ordering: .acquiring)
        guard rate > 0, ch == channels.load(ordering: .acquiring) else {
            out.update(repeating: 0, count: n)
            silentRenders.add(1, ordering: .relaxed)
            return rate == 0 ? CoreStatus.closed.code : CoreStatus.invalidArgument.code
        }
        let gain = Float(bitPattern: gainBits.load(ordering: .relaxed))
        let nsPerFrame = 1e9 / Double(rate)
        let period = Double(Self.clickPeriodNanos)
        for i in 0..<Int(frames) {
            let t0 = Double(host) + Double(i) * nsPerFrame
            // The frame whose span holds the multiple of the period clicks.
            let click = (t0 / period).rounded(.up) < ((t0 + nsPerFrame) / period).rounded(.up)
            let v: Float = click ? Self.clickAmplitude * gain : 0
            for c in 0..<Int(ch) { out[i * Int(ch) + c] = v }
        }
        return CoreStatus.ok.code
    }

    var hook: AudioRenderHook {
        AudioRenderHook(
            function: { context, out, frames, channels, host in
                guard let context else { return CoreStatus.invalidArgument.code }
                return Unmanaged<FakeAudio>.fromOpaque(context)._withUnsafeGuaranteedRef {
                    $0.render(out, frames: frames, channels: channels, host: host)
                }
            }, context: Unmanaged.passUnretained(self).toOpaque())
    }
}

/// One tile's audio settings in the fake.
struct FakeTileAudio: Sendable {
    var audible = false
    var volume: Float = 1
    var tapped = false
    /// Tap blocks emitted (1024 samples each).
    var blocks: UInt64 = 0
}
