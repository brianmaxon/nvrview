import Foundation
import NVRCore
import Testing

/// The recording player's stream choice (docs/UX.md "Recordings", "Quality"):
/// Auto's rule by layout, speed and zoom, the forced High and Low, which
/// cameras change, and what leaves with a camera. Windows has the same tests
/// (PlayerStreamsTests.cs).
@Suite struct PlayerQualityTests {
    private let a = RecordingCamera(device: "dev", channel: 0)
    private let b = RecordingCamera(device: "dev", channel: 1)
    private let c = RecordingCamera(device: "dev", channel: 2)

    private func tiers(_ s: inout PlayerStreams, _ cams: [RecordingCamera], _ mode: PlayerGridMode, rate: Double = 1,
        zoom: [RecordingCamera: Double] = [:]) -> [StreamTier] {
        s.opening(cams, mode: mode, rate: rate, zoom: { zoom[$0] ?? 1 })
    }

    @Test func autoIsTheSubStreamInTheGridAndTheMainStreamAlone() {
        var s = PlayerStreams()
        #expect(tiers(&s, [a, b, c], .grid) == [.sub, .sub, .sub])
        #expect(tiers(&s, [a, b, c], .emphasis(1)) == [.sub, .sub, .sub])
        #expect(tiers(&s, [a, b, c], .single(1)) == [.sub, .main, .sub])
        // One camera is a large view.
        #expect(tiers(&s, [a], .grid) == [.main])
    }

    @Test func autoUsesTheSubStreamFromTwiceTheSpeed() {
        var s = PlayerStreams()
        #expect(tiers(&s, [a, b], .single(0), rate: 0.25) == [.main, .sub])
        #expect(tiers(&s, [a, b], .single(0), rate: 1.0) == [.main, .sub])
        #expect(tiers(&s, [a, b], .single(0), rate: 2) == [.sub, .sub])
        #expect(tiers(&s, [a, b], .single(0), rate: 16) == [.sub, .sub])
    }

    @Test func autoUsesTheMainStreamWhenZoomedPastTwiceWithHysteresis() {
        var s = PlayerStreams()
        let cams = [a, b]
        func wanted(_ scale: Double, rate: Double = 1) -> StreamTier {
            s.wanted(a, cameras: cams, mode: .grid, rate: rate, zoomScale: scale)
        }
        #expect(wanted(1.9) == .sub)
        #expect(wanted(2.0) == .main)
        #expect(wanted(1.7) == .main, "stays main until below 1.6x")
        #expect(wanted(1.5) == .sub)
        #expect(wanted(2.5, rate: 4) == .sub, "speed wins")
        #expect(wanted(1.0) == .sub)
    }

    @Test func highAndLowAreForcedWhateverTheLayoutSpeedOrZoom() {
        var s = PlayerStreams()
        s.setQuality(.high, for: a)
        s.setQuality(.low, for: b)
        #expect(tiers(&s, [a, b], .grid, rate: 16) == [.main, .sub])
        #expect(tiers(&s, [a, b], .single(1), rate: 1, zoom: [b: 4]) == [.main, .sub])
        s.setQuality(.auto, for: a)
        #expect(s.quality(a) == .auto)
        #expect(s.wanted(a, cameras: [a, b], mode: .grid, rate: 1, zoomScale: 1) == .sub)
    }

    @Test func onlyTheCamerasWhoseStreamChangedAreReported() {
        var s = PlayerStreams()
        let cams = [a, b, c]
        _ = tiers(&s, cams, .grid)
        func changes(_ mode: PlayerGridMode, rate: Double = 1) -> [String] {
            s.changes(cams, mode: mode, rate: rate, zoom: { _ in 1 }).map { "\($0.camera.channel) \($0.tier.name)" }
        }
        #expect(changes(.grid).isEmpty)
        #expect(changes(.single(2)) == ["2 main"])
        #expect(changes(.single(2)).isEmpty, "applied once")
        s.setQuality(.high, for: a)
        #expect(changes(.single(2)) == ["0 main"])
        #expect(changes(.single(2), rate: 4) == ["2 sub"], "the forced camera stays")
        #expect(changes(.grid, rate: 1).isEmpty)
        s.setQuality(.auto, for: a)
        #expect(changes(.grid) == ["0 sub"])
        #expect(s.applied[a] == .sub && s.applied[c] == .sub)
    }

    @Test func aCameraThatLeavesForgetsItsChoice() {
        var s = PlayerStreams()
        s.setQuality(.high, for: a)
        _ = tiers(&s, [a, b], .grid)
        s.retain([b])
        #expect(s.quality(a) == .auto)
        #expect(s.applied[a] == nil && s.applied[b] == .sub)
    }

    @Test func theHeaderNamesTheStream() {
        var s = PlayerStreams()
        _ = tiers(&s, [a, b], .single(0))
        #expect(s.label(a) == "main stream")
        #expect(s.label(b) == "sub stream")
        s.setQuality(.low, for: a)
        _ = s.changes([a, b], mode: .single(0), rate: 1, zoom: { _ in 1 })
        #expect(s.label(a) == "Low · sub stream")
    }
}
