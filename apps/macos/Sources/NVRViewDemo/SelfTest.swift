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
    private var sawFailedState: [Bool]
    private var midpoint: [RendererStats]?
    private var timer: Timer?

    private static let interval = 0.2

    init(players: [FilePlayer], names: [String], seconds: Double) {
        self.players = players
        self.names = names
        self.seconds = seconds
        sawFailedState = Array(repeating: false, count: players.count)
    }

    func start(window: NSWindow) {
        let start = Date()
        timer = Timer.scheduledTimer(withTimeInterval: Self.interval, repeats: true) { _ in
            MainActor.assumeIsolated {
                let elapsed = Date().timeIntervalSince(start)
                for (i, p) in self.players.enumerated() where p.renderer.hasFailed {
                    self.sawFailedState[i] = true
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
