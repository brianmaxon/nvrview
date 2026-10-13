import Foundation
import NVRCore
import Testing

/// The tile header model (docs/UX.md "Tile header"). The vectors are
/// `fixtures/synthetic/tile-header.json`, which NvrView.Tiles.Tests
/// (TileHeaderTests.cs) reads too.
@Suite struct TileHeaderTests {
    private static func vectors() throws -> [String: Any] {
        let data = try Data(contentsOf: Fixtures.directory.appendingPathComponent("tile-header.json"))
        return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private static func tile(_ json: [String: Any]) throws -> TileHeaderTile {
        let known: Set<String> = [
            "audio", "talk", "doorbell", "panTilt", "presets", "lens", "fisheye", "picture", "recording", "zoomed",
            "alert", "listening", "conversation", "call", "panned", "playback",
        ]
        for key in json.keys { #expect(known.contains(key), "unknown tile key \(key)") }
        func flag(_ key: String, _ fallback: Bool = false) -> Bool { json[key] as? Bool ?? fallback }
        let call = try #require(TileHeaderCall(rawValue: json["call"] as? String ?? "none"))
        return TileHeaderTile(
            audio: flag("audio"), talk: flag("talk"), doorbell: flag("doorbell"), panTilt: flag("panTilt"),
            presets: flag("presets"), lens: flag("lens"), fisheye: flag("fisheye"), picture: flag("picture", true),
            recording: flag("recording"), zoomed: flag("zoomed"), panned: flag("panned"), alert: flag("alert"),
            listening: flag("listening"), conversation: flag("conversation"), call: call,
            playback: flag("playback"))
    }

    private static func items(_ json: Any?) throws -> [TileHeaderItem] {
        let ids = try #require(json as? [String])
        return try ids.map { try #require(TileHeaderItem(rawValue: $0), "unknown item \($0)") }
    }

    @Test func metricsMatchTheVectors() throws {
        let m = try #require(try Self.vectors()["metrics"] as? [String: Double])
        let s = TileHeaderMetrics.standard
        #expect(m["icon"] == s.icon)
        #expect(m["titleMin"] == s.titleMin)
        #expect(m["pill"] == s.pill)
        #expect(m["padding"] == s.padding)
        #expect(m["hysteresis"] == s.hysteresis)
        #expect(m["narrow"] == s.narrow)
    }

    @Test func sharedVectors() throws {
        let cases = try #require(try Self.vectors()["cases"] as? [[String: Any]])
        #expect(cases.count >= 10)
        for c in cases {
            let name = try #require(c["name"] as? String)
            var tile = try Self.tile(try #require(c["tile"] as? [String: Any]))
            let titleWidth = try #require(c["titleWidth"] as? Double)
            var previous: TileHeaderLayout?
            for (i, step) in try #require(c["steps"] as? [[String: Any]]).enumerated() {
                let at = "\(name), step \(i)"
                if let t = step["tile"] as? [String: Any] { tile = try Self.tile(t) }
                let width = try #require(step["width"] as? Double)
                let r = TileHeaderLayout.compute(width: width, titleWidth: titleWidth, tile: tile, previous: previous)
                #expect(r.visible == (try Self.items(step["visible"])), "\(at): visible")
                #expect(r.overflow == (try Self.items(step["overflow"])), "\(at): overflow")
                #expect(r.titleTruncated == (step["titleTruncated"] as? Bool), "\(at): title truncated")
                #expect(r.anchor == (step["anchor"] as? Double), "\(at): anchor")
                if let menu = step["menu"] as? [[String]] {
                    let got = r.contextMenu.map { [$0.item.rawValue, $0.label, $0.macShortcut, $0.windowsShortcut] }
                    #expect(got == menu, "\(at): menu")
                }
                if let more = step["moreMenu"] {
                    #expect(r.moreMenu.map(\.item) == (try Self.items(more)), "\(at): more menu")
                }
                previous = r
            }
        }
    }

    /// Rules every layout keeps, over a sweep of widths and capabilities.
    @Test func invariants() {
        let flags: [WritableKeyPath<TileHeaderTile, Bool>] = [
            \.audio, \.talk, \.doorbell, \.panTilt, \.presets, \.lens, \.fisheye, \.picture, \.recording,
            \.zoomed, \.alert, \.panned,
        ]
        for mask in 0..<(1 << flags.count) {
            var tile = TileHeaderTile()
            for (bit, flag) in flags.enumerated() { tile[keyPath: flag] = mask & (1 << bit) != 0 }
            let menu = TileHeaderLayout.menu(for: tile).map(\.item)
            for width in stride(from: 40.0, through: 640, by: 30) {
                let r = TileHeaderLayout.compute(width: width, titleWidth: 80, tile: tile)
                #expect(r.visible.contains(.title))
                #expect(r.showsMore == !r.overflow.isEmpty)
                // Nothing is only in the header: every visible action is in the menu, in the same order.
                let actions = r.visible.filter { $0 != .title && $0 != .recordingPill && $0 != .more }
                #expect(menu.filter { actions.contains($0) } == actions)
                #expect(!r.overflow.contains(.recordingPill) && !r.overflow.contains(.dismissAlert))
                if !tile.picture || width < 120 {
                    #expect(actions.isEmpty)
                }
                if !tile.picture { #expect(!r.visible.contains(.recordingPill)) }
                // Fresh layouts fit, "…" included, once anything is hidden.
                if tile.picture && width >= 120 {
                    let used =
                        r.visible.reduce(0.0) { sum, item in
                            switch item {
                            case .title: sum + 48
                            case .recordingPill: sum + 64
                            default: sum + 26
                            }
                        }
                    #expect(used <= max(width - 16, 48 + (tile.recording ? 64 : 0) + (tile.alert ? 26 : 0) + 26))
                }
            }
        }
    }
}
