import CoreGraphics
import Foundation
import NVRCore
import Testing

/// A window's picture settings per camera (docs/UX.md "Fill Tiles"): the
/// filled crop's pan at 1x, the effective-fill rule, the bound and the
/// saved form. The vectors are `fixtures/synthetic/tile-views.json`, which
/// the Windows tests (TileViewsTests.cs, ShellTests.cs) read too.
@Suite struct TileViewsTests {
    private static func vectors() throws -> [String: Any] {
        let data = try Data(contentsOf: Fixtures.directory.appendingPathComponent("tile-views.json"))
        return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private static func zoom(_ json: Any?) throws -> TileZoom {
        let v = try #require(json as? [Double])
        return TileZoom(scale: v[0], center: CGPoint(x: v[1], y: v[2]))
    }

    private static func close(_ a: Double, _ b: Double) -> Bool { abs(a - b) < 1e-9 }

    @Test func theLimitMatchesTheVectors() throws {
        #expect(try Self.vectors()["limit"] as? Int == TileViews<String>.limit)
    }

    @Test func fillPanVectors() throws {
        let cases = try #require(try Self.vectors()["pan"] as? [[String: Any]])
        #expect(cases.count >= 8)
        for c in cases {
            let name = try #require(c["name"] as? String)
            let picture = try #require(c["picture"] as? [Double]), tile = try #require(c["tile"] as? [Double])
            let base = ZoomBase(
                picture: CGSize(width: picture[0], height: picture[1]), tile: CGSize(width: tile[0], height: tile[1]),
                fill: try #require(c["fill"] as? Bool))
            let z = try Self.zoom(c["zoom"])
            #expect(z.canPan(in: base) == (c["canPan"] as? Bool), "\(name): can pan")
            let out: TileZoom
            if let to = c["zoomTo"] as? Double {
                out = z.zoomed(to: to, keeping: z.picturePoint(atUnit: CGPoint(x: 0.5, y: 0.5), in: base),
                    atUnit: CGPoint(x: 0.5, y: 0.5), in: base)
            } else {
                let d = try #require(c["drag"] as? [Double])
                out = z.panned(byUnit: CGVector(dx: d[0], dy: d[1]), in: base)
            }
            let want = try Self.zoom(c["result"])
            #expect(
                Self.close(out.scale, want.scale) && Self.close(out.center.x, want.center.x)
                    && Self.close(out.center.y, want.center.y), "\(name): \(out)")
            let v = try #require(c["visible"] as? [Double]), got = out.visible(in: base)
            #expect(
                Self.close(got.minX, v[0]) && Self.close(got.minY, v[1]) && Self.close(got.width, v[2])
                    && Self.close(got.height, v[3]), "\(name): visible \(got)")
        }
    }

    @Test func effectiveFillVectors() throws {
        let cases = try #require(try Self.vectors()["fill"] as? [[String: Any]])
        for c in cases {
            let name = try #require(c["name"] as? String)
            var views = TileViews<String>()
            for op in try #require(c["ops"] as? [[Any]]) {
                switch op[0] as? String {
                case "window": views.setWindowFill(try #require(op[1] as? Bool))
                case "tile": views.setFill(try #require(op[2] as? Bool), for: try #require(op[1] as? String))
                case "zoom":
                    let n = op.dropFirst(2).compactMap { ($0 as? NSNumber)?.doubleValue }
                    views.setZoom(
                        TileZoom(scale: n[0], center: CGPoint(x: n[1], y: n[2])), for: try #require(op[1] as? String))
                default: Issue.record("\(name): unknown op \(op)")
                }
            }
            for (key, want) in try #require(c["fills"] as? [String: Bool]) {
                #expect(views.fills(key) == want, "\(name): \(key) fills")
            }
            let choices = try #require(c["choices"] as? [String: Bool])
            #expect(Dictionary(uniqueKeysWithValues: views.order.compactMap { k in views.fillChoice(k).map { (k, $0) } }) == choices, "\(name): choices")
            #expect(views.mixed == (c["mixed"] as? Bool), "\(name): mixed")
            #expect(views.count == (c["count"] as? Int), "\(name): count")
            for (key, z) in c["zooms"] as? [String: Any] ?? [:] {
                #expect(views.zoom(key) == (try Self.zoom(z)), "\(name): \(key) zoom")
            }
        }
    }

    @Test func savedVectors() throws {
        let cases = try #require(try Self.vectors()["saved"] as? [[String: Any]])
        let plain = SavedWindow(content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500))
        let base = try #require(try JSONSerialization.jsonObject(with: try JSONEncoder().encode(plain)) as? [String: Any])
        for c in cases {
            let name = try #require(c["name"] as? String)
            let members = try #require(c["window"] as? [String: Any])
            let json = base.merging(members) { _, new in new }
            let window = try JSONDecoder().decode(SavedWindow.self, from: try JSONSerialization.data(withJSONObject: json))
            #expect(window.fill == (c["fill"] as? Bool), "\(name): fill")
            let entries = try #require(c["entries"] as? [[Any]])
            let got = window.tileViews.map { e -> [String] in
                [e.camera.device, "\(e.camera.channel)", "\(e.camera.lens)", "\(e.view.zoom.scale)",
                 "\(Double(e.view.zoom.center.x))", "\(Double(e.view.zoom.center.y))", e.view.fill.map { "\($0)" } ?? "nil"]
            }
            let want = entries.map { e -> [String] in
                [e[0] as? String ?? "?", "\((e[1] as? NSNumber)?.intValue ?? -1)", "\((e[2] as? NSNumber)?.intValue ?? -1)",
                 "\((e[3] as? NSNumber)?.doubleValue ?? -1)", "\((e[4] as? NSNumber)?.doubleValue ?? -1)",
                 "\((e[5] as? NSNumber)?.doubleValue ?? -1)", (e[6] as? Bool).map { "\($0)" } ?? "nil"]
            }
            #expect(got == want, "\(name)")
            // What is read back is written back the same.
            let again = try JSONDecoder().decode(SavedWindow.self, from: try JSONEncoder().encode(window))
            #expect(again == window, "\(name): round trip")
        }
    }

    @Test func nothingIsWrittenForAPlainWindow() throws {
        let plain = SavedWindow(content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500))
        let json = try #require(try JSONSerialization.jsonObject(with: try JSONEncoder().encode(plain)) as? [String: Any])
        #expect(json["fill"] == nil && json["tileViews"] == nil)
    }

    @Test func theCamerasChangedLongestAgoGoFirst() {
        var views = TileViews<Int>()
        let limit = TileViews<Int>.limit
        for i in 0..<limit { views.setZoom(TileZoom(scale: 2, center: CGPoint(x: 0.5, y: 0.5)), for: i) }
        #expect(views.count == limit)
        // Changing camera 0 again makes it the newest.
        views.setFill(true, for: 0)
        views.setZoom(TileZoom(scale: 3, center: CGPoint(x: 0.5, y: 0.5)), for: limit)
        #expect(views.count == limit)
        #expect(views.zoom(0).scale == 2 && views.fills(0))
        #expect(views.zoom(1) == .none)
        #expect(views.zoom(limit).scale == 3)
        #expect(views.order.first == 2 && views.order.last == limit)
    }

    @Test func aSavedWindowKeepsTheNewestEntries() throws {
        let entries = (0..<(TileViews<CameraRef>.limit + 44)).map {
            SavedTileView(camera: CameraRef(device: "nvr-a", channel: $0), view: TileView(zoom: TileZoom(scale: 2, center: CGPoint(x: 0.5, y: 0.5))))
        }
        let window = SavedWindow(
            content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500), tileViews: entries)
        let back = try JSONDecoder().decode(SavedWindow.self, from: try JSONEncoder().encode(window))
        #expect(back.tileViews.count == TileViews<CameraRef>.limit)
        #expect(back.tileViews.first?.camera.channel == 44 && back.tileViews.last?.camera.channel == entries.count - 1)
    }

    @Test func resetEasesTheCenterHomeWithTheScale() {
        let base = ZoomBase(picture: CGSize(width: 1600, height: 900), tile: CGSize(width: 300, height: 300), fill: true)
        var easing = ZoomEasing<Int>()
        var z = TileZoom(scale: 4, center: CGPoint(x: 0.2, y: 0.3))
        easing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5), recenter: true), for: 0, now: 0)
        var t = 0.0, steps = 0
        while easing.isEasing, steps < 1000 {
            t += 1.0 / 60
            steps += 1
            for step in easing.tick(now: t, current: { _ in z }, base: { _, _ in base }) { z = step.zoom }
        }
        #expect(z == .none)
        // Only panned at 1x: back at once.
        z = TileZoom(scale: 1, center: CGPoint(x: 0.3, y: 0.5))
        easing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5), recenter: true), for: 0, now: t)
        for step in easing.tick(now: t + 1.0 / 60, current: { _ in z }, base: { _, _ in base }) { z = step.zoom }
        #expect(z == .none && !easing.isEasing)
        // Without recentering, zooming out keeps the crop where it ends.
        z = TileZoom(scale: 2, center: CGPoint(x: 0.3, y: 0.5))
        easing.set(.init(scale: 1, unit: CGPoint(x: 0.5, y: 0.5)), for: 0, now: t)
        while easing.isEasing, steps < 2000 {
            t += 1.0 / 60
            steps += 1
            for step in easing.tick(now: t, current: { _ in z }, base: { _, _ in base }) { z = step.zoom }
        }
        #expect(z.scale == 1 && abs(z.center.x - 0.3) < 1e-9)
    }
}
