import AppKit
import VideoKit

/// Plays the grid for a fixed time and checks, from what the renderers
/// report, that every tile is working:
///
/// - frames keep reaching the display layer;
/// - no frame failed to decode and no layer ended up in its failed state;
/// - the window is on screen at a usable size.
///
/// It cannot see the picture itself: `AVSampleBufferDisplayLayer` does not
/// hand back what it shows. Prints one line per tile and a verdict, then exits
/// 0 (pass) or 1 (fail).
@MainActor
final class SelfTest {
    private let players: [FilePlayer]
    private let names: [String]
    private let seconds: Double
    /// When set, the run ends once every tile has shown twice this many
    /// frames (`seconds` is then only the limit for a run that stalls).
    private let untilFrames: Int?
    private var sawFailedState: [Bool]
    private var midpoint: [RendererStats]?
    private var timer: Timer?

    private static let interval = 0.2
    private static let framesInterval = 0.02

    init(players: [FilePlayer], names: [String], seconds: Double, untilFrames: Int? = nil) {
        self.players = players
        self.names = names
        self.seconds = seconds
        self.untilFrames = untilFrames
        sawFailedState = Array(repeating: false, count: players.count)
    }

    func start(window: NSWindow) {
        let start = Date()
        timer = Timer.scheduledTimer(withTimeInterval: untilFrames == nil ? Self.interval : Self.framesInterval, repeats: true) { _ in
            MainActor.assumeIsolated {
                let elapsed = Date().timeIntervalSince(start)
                for (i, p) in self.players.enumerated() where p.renderer.hasFailed {
                    self.sawFailedState[i] = true
                }
                if let frames = self.untilFrames {
                    let shown = self.players.map(\.renderer.stats.framesEnqueued)
                    if self.midpoint == nil, shown.allSatisfy({ $0 >= frames }) {
                        self.midpoint = self.players.map(\.renderer.stats)
                    }
                    if shown.allSatisfy({ $0 >= 2 * frames }) || elapsed >= self.seconds { self.finish(window: window) }
                    return
                }
                if self.midpoint == nil, elapsed >= self.seconds / 2 {
                    self.midpoint = self.players.map(\.renderer.stats)
                }
                if elapsed >= self.seconds { self.finish(window: window) }
            }
        }
    }

    private func finish(window: NSWindow) {
        timer?.invalidate()
        var failures: [String] = []

        let content = window.contentLayoutRect.size
        let onScreen = window.isVisible && window.screen != nil
        print(String(format: "window: %.0fx%.0f, on screen: %@", content.width, content.height, onScreen ? "yes" : "no"))
        if !onScreen { failures.append("window is not on screen") }
        if content.width < 480 || content.height < 360 { failures.append("window is smaller than 480x360") }

        for (i, player) in players.enumerated() {
            let s = player.renderer.stats
            let halfEnqueued = midpoint?[i].framesEnqueued ?? 0
            var problems: [String] = []
            if s.framesEnqueued == 0 { problems.append("no frames reached the display layer") }
            if s.framesEnqueued <= halfEnqueued { problems.append("frames stopped reaching the display layer") }
            if s.decodeFailures > 0 || sawFailedState[i] { problems.append("decode failures: \(s.decodeFailures)") }
            print(
                "tile \(i) \(names[i]): in \(s.framesReceived), shown \(s.framesEnqueued), "
                    + "gated \(s.droppedWaitingForKeyframe), "
                    + "dropped \(s.droppedQueueOverflow + s.droppedDisplayBusy), "
                    + "format changes \(s.formatChanges), failures \(s.decodeFailures) "
                    + (problems.isEmpty ? "OK" : "FAIL"))
            failures += problems.map { "tile \(i): \($0)" }
        }

        if failures.isEmpty {
            print("selftest: PASS")
            exit(0)
        }
        for f in failures { print("  \(f)") }
        print("selftest: FAIL")
        exit(1)
    }
}
