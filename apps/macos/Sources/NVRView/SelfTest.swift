import AppKit
import Foundation
import NVRCore
import VideoKit

/// `--selftest SECONDS`: runs the real window (behind all others, no focus),
/// then checks that the status line reached "connected", the tile is playing,
/// a non-flat picture is on screen, and, with `--fake`, that the fake core saw
/// no contract violation. Exits 0 or 1.
@MainActor
final class SelfTest {
    private unowned let app: AppDelegate
    init(app: AppDelegate) { self.app = app }

    func start(seconds: Double) {
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [self] in
            guard let grid = app.grid else { finish(["no view"]) }
            grid.captureNextFrame { [self] image in
                MainActor.assumeIsolated { check(image) }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds + 5) { [self] in
            finish(["no frame was drawn"])
        }
    }

    private func check(_ image: RenderedImage) {
        var problems: [String] = []
        guard let session = app.session else { finish(["no session"]) }
        let status = session.status
        print("status line: \(app.statusLabel.stringValue)")
        if status.device != .connected { problems.append("device state is \(String(describing: status.device))") }
        if session.tile?.snapshot.status != .playing { problems.append("tile is not playing") }
        var sum = 0.0, sumSq = 0.0, n = 0.0
        for y in stride(from: 0, to: image.height, by: 4) {
            for x in stride(from: 0, to: image.width, by: 4) {
                let v = Double(image.rgb(x: x, y: y).wrappedSum()) / 3
                sum += v
                sumSq += v * v
                n += 1
            }
        }
        let mean = sum / max(n, 1)
        let spread = max(sumSq / max(n, 1) - mean * mean, 0).squareRoot()
        print(String(format: "frame %dx%d, spread %.1f", image.width, image.height, spread))
        if spread < 8 { problems.append("the picture is flat") }
        if let fake = session.fake {
            problems += fake.violations.map { "core contract: \($0)" }
        }
        finish(problems)
    }

    private func finish(_ problems: [String]) -> Never {
        app.session?.shutdown()
        if problems.isEmpty {
            print("selftest: PASS")
            exit(0)
        }
        for p in problems { print("  \(p)") }
        print("selftest: FAIL")
        exit(1)
    }
}
