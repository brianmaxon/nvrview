import CoreGraphics
import Foundation
import NVRCore
import Testing

/// A window's picture settings per tile (docs/UX.md "Fill Tiles"): the
/// filled crop's pan at 1x, the effective-fill rule, the bound and the
/// saved form, tile identities and the migration from the per-camera
/// form. The vectors are `fixtures/synthetic/tile-views.json`, which
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

    /// A saved window (`window` members over a plain one) as read back.
    private static func window(_ c: [String: Any]) throws -> SavedWindow {
        let plain = SavedWindow(content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500))
        let base = try #require(try JSONSerialization.jsonObject(with: try JSONEncoder().encode(plain)) as? [String: Any])
        let members = try #require(c["window"] as? [String: Any])
        let json = base.merging(members) { _, new in new }
        return try JSONDecoder().decode(SavedWindow.self, from: try JSONSerialization.data(withJSONObject: json))
    }

    /// Entries as the vectors write them: [layout, tile, device, channel,
    /// lens, scale, x, y, fill or null].
    private static func rows(_ entries: [SavedTileView]) -> [[String]] {
        entries.map { e in
            [e.key.layout, e.key.tile, e.key.camera.device, "\(e.key.camera.channel)", "\(e.key.camera.lens)",
             "\(e.view.zoom.scale)", "\(Double(e.view.zoom.center.x))", "\(Double(e.view.zoom.center.y))",
             e.view.fill.map { "\($0)" } ?? "nil"]
        }
    }

    private static func rows(_ json: Any?) throws -> [[String]] {
        try #require(json as? [[Any]]).map { e in
            [e[0] as? String ?? "?", e[1] as? String ?? "?", e[2] as? String ?? "?",
             "\((e[3] as? NSNumber)?.intValue ?? -1)", "\((e[4] as? NSNumber)?.intValue ?? -1)",
             "\((e[5] as? NSNumber)?.doubleValue ?? -1)", "\((e[6] as? NSNumber)?.doubleValue ?? -1)",
             "\((e[7] as? NSNumber)?.doubleValue ?? -1)", (e[8] as? Bool).map { "\($0)" } ?? "nil"]
        }
    }

    @Test func savedVectors() throws {
        let cases = try #require(try Self.vectors()["saved"] as? [[String: Any]])
        for c in cases {
            let name = try #require(c["name"] as? String)
            let window = try Self.window(c)
            #expect(window.fill == (c["fill"] as? Bool), "\(name): fill")
            #expect(Self.rows(window.tileViews) == (try Self.rows(c["entries"])), "\(name)")
            #expect(window.legacyTileViews.isEmpty, "\(name): nothing to migrate")
            // What is read back is written back the same.
            let again = try JSONDecoder().decode(SavedWindow.self, from: try JSONEncoder().encode(window))
            #expect(again == window, "\(name): round trip")
        }
    }

    @Test func tileIDVectors() throws {
        let cases = try #require(try Self.vectors()["tileIDs"] as? [[String: Any]])
        #expect(cases.count >= 5)
        for c in cases {
            let name = try #require(c["name"] as? String)
            let have = try #require(c["have"] as? [Any]).map { $0 as? String }
            let want = try #require(c["want"] as? [String])
            #expect(LayoutNode.assignTileIDs(have) == want, "\(name)")
            // The same through a tree: a row of leaves with those members.
            let leaves = try #require(c["have"] as? [Any]).map { v -> LayoutNode in
                var node: [String: JSONValue] = [:]
                if let s = v as? String { node[LayoutNode.tileMember] = .string(s) }
                if let n = v as? NSNumber { node[LayoutNode.tileMember] = .integer(n.int64Value) }
                return .leaf(nil, extras: NodeExtras(node: node))
            }
            let tree = LayoutNode.chain(.row, leaves).withTileIDs
            #expect(tree.leaves.map { tree[$0.path]?.tileID } == want, "\(name): tree")
            #expect(tree.withTileIDs == tree, "\(name): stable")
        }
    }

    @Test func migrationVectors() throws {
        let cases = try #require(try Self.vectors()["migrate"] as? [[String: Any]])
        for c in cases {
            let name = try #require(c["name"] as? String)
            let window = try Self.window(c)
            let layout = try #require(c["layout"] as? String)
            let leaves = try #require(c["tiles"] as? [[Any]]).map { t -> LayoutNode in
                let camera = (t[1] as? String).map {
                    CameraRef(device: $0, channel: (t[2] as? NSNumber)?.intValue ?? 0, lens: (t[3] as? NSNumber)?.intValue ?? 0)
                }
                return .leaf(camera, extras: NodeExtras(node: [LayoutNode.tileMember: .string(t[0] as? String ?? "")]))
            }
            var views = window.views
            if !window.legacyTileViews.isEmpty {
                views.migrate(window.legacyTileViews, layout: layout, tree: LayoutNode.chain(.row, leaves))
            }
            #expect(views.fill == (c["fill"] as? Bool), "\(name): fill")
            #expect(Self.rows(views.savedEntries) == (try Self.rows(c["entries"])), "\(name)")
            // Saved again, it is in the per-tile form only.
            let saved = SavedWindow(content: window.content, frame: window.frame, fill: views.fill, tileViews: views.savedEntries)
            let json = try #require(try JSONSerialization.jsonObject(with: try JSONEncoder().encode(saved)) as? [String: Any])
            #expect(json["tileViews"] == nil, "\(name): the old form is not written")
        }
    }

    @Test func nothingIsWrittenForAPlainWindow() throws {
        let plain = SavedWindow(content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500))
        let json = try #require(try JSONSerialization.jsonObject(with: try JSONEncoder().encode(plain)) as? [String: Any])
        #expect(json["fill"] == nil && json["tiles"] == nil && json["tileViews"] == nil)
    }

    @Test func theTilesChangedLongestAgoGoFirst() {
        var views = TileViews<Int>()
        let limit = TileViews<Int>.limit
        for i in 0..<limit { views.setZoom(TileZoom(scale: 2, center: CGPoint(x: 0.5, y: 0.5)), for: i) }
        #expect(views.count == limit)
        // Changing tile 0 again makes it the newest.
        views.setFill(true, for: 0)
        views.setZoom(TileZoom(scale: 3, center: CGPoint(x: 0.5, y: 0.5)), for: limit)
        #expect(views.count == limit)
        #expect(views.zoom(0).scale == 2 && views.fills(0))
        #expect(views.zoom(1) == .none)
        #expect(views.zoom(limit).scale == 3)
        #expect(views.order.first == 2 && views.order.last == limit)
    }

    @Test func aSavedWindowKeepsTheNewestEntries() throws {
        let entries = (0..<(TileViews<TileKey>.limit + 44)).map {
            SavedTileView(
                key: TileKey(layout: "layout:layout-1", tile: "t\($0)", camera: CameraRef(device: "nvr-a", channel: $0)),
                view: TileView(zoom: TileZoom(scale: 2, center: CGPoint(x: 0.5, y: 0.5))))
        }
        let window = SavedWindow(
            content: .layout(id: "layout-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500), tileViews: entries)
        let back = try JSONDecoder().decode(SavedWindow.self, from: try JSONEncoder().encode(window))
        #expect(back.tileViews.count == TileViews<TileKey>.limit)
        #expect(back.tileViews.first?.key.tile == "t44" && back.tileViews.last?.key.camera.channel == entries.count - 1)
    }

    // MARK: Per tile

    private static let a = CameraRef(device: "nvr-a", channel: 0)
    private static let b = CameraRef(device: "nvr-a", channel: 1)
    private static let two = TileZoom(scale: 2, center: CGPoint(x: 0.3, y: 0.5))
    private static let three = TileZoom(scale: 3, center: CGPoint(x: 0.6, y: 0.4))

    @Test func twoTilesOfOneCameraAreIndependent() throws {
        let tree = LayoutNode.balanced([Self.a, Self.a, Self.b]).withTileIDs
        let keys = tree.tileKeys(layout: "L")
        #expect(Set(keys.map(\.tile)).count == 3)
        var views = TileViews<TileKey>()
        views.setZoom(Self.two, for: keys[0])
        views.setZoom(Self.three, for: keys[1])
        views.setFill(true, for: keys[1])
        #expect(views.zoom(keys[0]) == Self.two && !views.fills(keys[0]))
        #expect(views.zoom(keys[1]) == Self.three && views.fills(keys[1]))
        #expect(views.mixed(layout: "L") && !views.mixed(layout: "M"))
    }

    @Test func aSwapMovesEachViewWithItsCamera() throws {
        let tree = LayoutNode.balanced([Self.a, Self.b]).withTileIDs
        let (p, q) = (tree.leaves[0].path, tree.leaves[1].path)
        var views = TileViews<TileKey>()
        views.setZoom(Self.two, for: try #require(tree.tileKey(at: p, layout: "L")))
        views.setZoom(Self.three, for: try #require(tree.tileKey(at: q, layout: "L")))
        views.swapViews(layout: "L", tree: tree, p, q)
        let swapped = try #require(tree.swapping(p, q))
        // The tiles keep their identities; the cameras and views moved.
        #expect(swapped[p]?.tileID == tree[p]?.tileID && swapped[p]?.camera == Self.b)
        views.prune(layout: "L", tree: swapped)
        #expect(views.zoom(try #require(swapped.tileKey(at: p, layout: "L"))) == Self.three)
        #expect(views.zoom(try #require(swapped.tileKey(at: q, layout: "L"))) == Self.two)
        #expect(views.count == 2)
    }

    @Test func anotherCameraInATileStartsAtTheDefault() throws {
        let tree = LayoutNode.balanced([Self.a, Self.b]).withTileIDs
        let p = tree.leaves[0].path
        var views = TileViews<TileKey>()
        views.setZoom(Self.two, for: try #require(tree.tileKey(at: p, layout: "L")))
        views.setZoom(Self.three, for: TileKey(layout: "M", tile: "t0", camera: Self.a))
        let shown = try #require(tree.assigning(Self.b, at: p)).withTileIDs
        views.prune(layout: "L", tree: shown)
        #expect(views.zoom(try #require(shown.tileKey(at: p, layout: "L"))) == .none)
        // Showing the first camera again does not bring its old view back.
        let back = try #require(shown.assigning(Self.a, at: p))
        #expect(views.zoom(try #require(back.tileKey(at: p, layout: "L"))) == .none)
        // Another layout's views are kept.
        #expect(views.count == 1 && views.zoom(TileKey(layout: "M", tile: "t0", camera: Self.a)) == Self.three)
    }

    @Test func identitiesSurviveSplitsAndRemovals() throws {
        let tree = LayoutNode.balanced([Self.a, Self.b, Self.a, nil]).withTileIDs
        let ids = tree.leaves.map { tree[$0.path]?.tileID }
        // Split the first tile: it keeps its identity, the new one gets a fresh one.
        let (split, new) = try #require(tree.splitting(at: tree.leaves[0].path, axis: .row))
        let named = split.withTileIDs
        let after = named.leaves.map { named[$0.path]?.tileID }
        #expect(Set(ids.compactMap(\.self)).isSubset(of: Set(after.compactMap(\.self))))
        #expect(named[new]?.tileID != nil && !ids.contains(named[new]?.tileID))
        // Remove the second tile: the others keep theirs.
        let (removed, _) = try #require(named.removing(at: named.leaves[2].path))
        let kept = removed.withTileIDs
        #expect(kept == removed)
        #expect(Set(kept.leaves.compactMap { kept[$0.path]?.tileID }) == Set(after.compactMap(\.self)).subtracting([after[2]!]))
        // Stored and read back, the identities stay.
        let layout = SavedLayout(id: "layout-1", name: "x", root: kept)
        let read = try #require(SavedLayout.list(fromJSON: "[\(layout.json)]").first)
        #expect(read.root == kept)
    }

    @Test func renamingTheContentKeepsTheViews() throws {
        var views = TileViews<TileKey>()
        views.setZoom(Self.two, for: TileKey(layout: "scratch", tile: "t0", camera: Self.a))
        views.setZoom(Self.three, for: TileKey(layout: "layout:x", tile: "t0", camera: Self.a))
        views.rename(layout: "scratch", to: "layout:x")
        #expect(views.count == 1 && views.zoom(TileKey(layout: "layout:x", tile: "t0", camera: Self.a)) == Self.two)
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
