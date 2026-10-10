import CoreGraphics
import FakeCore
import Foundation
import NVRCore
import Testing

private func cam(_ channel: Int, _ device: String = "dev-a") -> CameraRef {
    CameraRef(device: device, channel: channel)
}

/// The layout the contract shows (docs/FFI-ABI3.md section 4).
private let contractTree = LayoutNode.split(
    .row, ratio: 0.62, a: .leaf(cam(3)),
    b: .split(.col, ratio: 0.34, a: .leaf(nil), b: .leaf(cam(9))))

/// The shape of a tree without its cameras, for "swap keeps the shape".
private func shape(_ n: LayoutNode) -> String {
    switch n {
    case .leaf: "L"
    case .split(let axis, let ratio, let a, let b): "\(axis.rawValue)\(ratio)(\(shape(a)),\(shape(b)))"
    }
}

@Suite struct LayoutTreeTests {
    @Test func readingLeavesAndCameras() {
        let leaves = contractTree.leaves
        #expect(leaves.map(\.path) == [[.a], [.b, .a], [.b, .b]])
        #expect(leaves.map(\.camera) == [cam(3), nil, cam(9)])
        #expect(contractTree.leafCount == 3)
        #expect(contractTree.depth == 3)
        #expect(contractTree.cameras == [cam(3), cam(9)])
        #expect(contractTree[[.b, .b]] == .leaf(cam(9)))
        #expect(contractTree[[.a, .a]] == nil)
    }

    @Test func splitAddsOneEmptyTileNextToTheOld() throws {
        let (tree, new) = try #require(contractTree.splitting(at: [.a], axis: .col))
        #expect(new == [.a, .b])
        #expect(tree.leafCount == 4)
        #expect(tree[[.a]] == .split(.col, ratio: 0.5, a: .leaf(cam(3)), b: .leaf(nil)))
        #expect(tree[[.b]] == contractTree[[.b]])
        // Only leaves split.
        #expect(contractTree.splitting(at: [.b], axis: .row) == nil)
        #expect(contractTree.splitting(at: [.a, .a], axis: .row) == nil)
    }

    @Test func removeLetsTheNeighborTakeTheSpace() throws {
        let (tree, survivor) = try #require(contractTree.removing(at: [.b, .a]))
        #expect(survivor == [.b])
        #expect(tree == .split(.row, ratio: 0.62, a: .leaf(cam(3)), b: .leaf(cam(9))))
        let (single, _) = try #require(tree.removing(at: [.a]))
        #expect(single == .leaf(cam(9)))
        // Removing the last tile leaves one empty tile, never nothing.
        let (empty, path) = try #require(single.removing(at: []))
        #expect(empty == .empty)
        #expect(path == [])
        // Only leaves are removed.
        #expect(contractTree.removing(at: [.b]) == nil)
    }

    @Test func swapExchangesCamerasOnly() throws {
        let swapped = try #require(contractTree.swapping([.a], [.b, .b]))
        #expect(swapped.cameraList == [cam(9), nil, cam(3)])
        #expect(shape(swapped) == shape(contractTree))
        #expect(contractTree.swapping([.a], [.a]) == contractTree)
        #expect(contractTree.swapping([.a], [.b]) == nil)
        // Swapping with an empty tile moves the camera there.
        let moved = try #require(contractTree.swapping([.a], [.b, .a]))
        #expect(moved.cameraList == [nil, cam(3), cam(9)])
    }

    @Test func ratiosAreClamped() throws {
        let tight = try #require(contractTree.settingRatio(0.01, at: []))
        if case .split(_, let r, _, _) = tight { #expect(r == LayoutNode.minRatio) } else { Issue.record("not a split") }
        let wide = try #require(contractTree.settingRatio(5, at: [.b]))
        if case .split(_, let r, _, _)? = wide[[.b]] { #expect(r == LayoutNode.maxRatio) } else { Issue.record("not a split") }
        #expect(contractTree.settingRatio(0.5, at: [.a]) == nil)
    }

    @Test func assignAndClearDevice() throws {
        let assigned = try #require(contractTree.assigning(cam(1, "dev-b"), at: [.b, .a]))
        #expect(assigned.cameraList == [cam(3), cam(1, "dev-b"), cam(9)])
        #expect(assigned.clearing(device: "dev-a").cameraList == [nil, cam(1, "dev-b"), nil])
        #expect(contractTree.assigning(cam(1), at: [.b]) == nil)
    }

    @Test(arguments: [0, 1, 2, 3, 4, 5, 7, 9, 10, 16])
    func balancedGridsHaveEqualTiles(count: Int) {
        let cameras = (0..<count).map { cam($0) }
        let tree = LayoutNode.balanced(cameras)
        #expect(tree.leafCount == max(count, 1))
        #expect(tree.cameraList == (count == 0 ? [nil] : cameras))
        guard count > 0 else { return }
        let frames = tree.frames(in: CGRect(x: 0, y: 0, width: 1200, height: 1200))
        let columns = Int(Double(count).squareRoot().rounded(.up))
        let rows = (count + columns - 1) / columns
        for f in frames.leaves {
            #expect(abs(f.rect.height - 1200 / CGFloat(rows)) <= 1)
        }
    }

    @Test func presets() {
        for preset in LayoutNode.Preset.allCases {
            let tree = preset.tree(cameras: [cam(0), cam(1)])
            let expected = [LayoutNode.Preset.single: 1, .twoByTwo: 4, .onePlusThree: 4, .threeByThree: 9][preset]
            #expect(tree.leafCount == expected)
            #expect(tree.cameraList.first == cam(0))
        }
    }

    /// Random edits keep every invariant: ratios in range, leaf counts as
    /// each operation promises, swap keeps the camera multiset, the frames
    /// tile the bounds exactly, and the JSON round trip is lossless.
    @Test func randomEditsKeepInvariants() throws {
        var rng = SplitMix(seed: 0x5eed)
        var tree = LayoutNode.empty
        let bounds = CGRect(x: 0, y: 0, width: 1600, height: 900)
        for _ in 0..<2000 {
            let leaves = tree.leaves
            let pick = leaves[Int(rng.next() % UInt64(leaves.count))].path
            switch rng.next() % 5 {
            case 0:
                let before = tree.leafCount
                if let (t, new) = tree.splitting(at: pick, axis: rng.next() % 2 == 0 ? .row : .col) {
                    #expect(t.leafCount == before + 1)
                    #expect(t[new] == .leaf(nil))
                    tree = t
                }
            case 1:
                let before = tree.leafCount
                let (t, _) = try #require(tree.removing(at: pick))
                #expect(t.leafCount == max(before - 1, 1))
                tree = t
            case 2:
                let other = leaves[Int(rng.next() % UInt64(leaves.count))].path
                let t = try #require(tree.swapping(pick, other))
                #expect(t.cameraList.compactMap(\.self).sorted { $0.channel < $1.channel }
                    == tree.cameraList.compactMap(\.self).sorted { $0.channel < $1.channel })
                #expect(shape(t) == shape(tree))
                tree = t
            case 3:
                tree = try #require(tree.assigning(rng.next() % 4 == 0 ? nil : cam(Int(rng.next() % 16)), at: pick))
            default:
                if let split = tree.frames(in: bounds).dividers.first {
                    tree = try #require(tree.settingRatio(Double(rng.next() % 1000) / 1000, at: split.path))
                }
            }
            #expect(tree.leafCount <= LayoutNode.maxLeaves)
            checkRatios(tree)
            checkFrames(tree, bounds)
            let layout = SavedLayout(id: "l", name: "Random", version: 1, root: tree)
            #expect(SavedLayout.list(fromJSON: #"{"layouts":[\#(layout.json)]}"#) == [layout])
        }
    }

    private func checkRatios(_ n: LayoutNode) {
        if case .split(_, let r, let a, let b) = n {
            #expect(r >= LayoutNode.minRatio && r <= LayoutNode.maxRatio)
            checkRatios(a)
            checkRatios(b)
        }
    }

    private func checkFrames(_ tree: LayoutNode, _ bounds: CGRect) {
        let frames = tree.frames(in: bounds)
        #expect(frames.leaves.count == tree.leafCount)
        #expect(frames.dividers.count == tree.leafCount - 1)
        let area = frames.leaves.reduce(0) { $0 + $1.rect.width * $1.rect.height }
        #expect(abs(area - bounds.width * bounds.height) < 0.5)
        for f in frames.leaves { #expect(bounds.contains(f.rect)) }
    }
}

/// A small deterministic generator for the random edit test.
private struct SplitMix {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}

@Suite struct LayoutJSONTests {
    @Test func encodesTheContractShape() throws {
        let layout = SavedLayout(id: "l1", name: "Layout 1", version: 1, root: contractTree)
        let object = try #require(try JSONSerialization.jsonObject(with: Data(layout.json.utf8)) as? [String: Any])
        #expect(object["id"] as? String == "l1")
        #expect(object["name"] as? String == "Layout 1")
        #expect(object["version"] as? Int == 1)
        let root = try #require(object["root"] as? [String: Any])
        #expect(root["split"] as? String == "row")
        #expect(root["ratio"] as? Double == 0.62)
        let a = try #require(root["a"] as? [String: Any])
        let camera = try #require(a["camera"] as? [String: Any])
        #expect(camera["device"] as? String == "dev-a")
        #expect(camera["channel"] as? Int == 3)
        let b = try #require(root["b"] as? [String: Any])
        let empty = try #require(b["a"] as? [String: Any])
        // An empty tile is `"camera": null`, not a missing key.
        #expect(empty.keys.contains("camera"))
        #expect(empty["camera"] is NSNull)
    }

    @Test func readsTheContractExample() {
        let json = #"""
            {"layouts":[{"id":"entrances","name":"Entrances","version":1,
             "root":{"split":"row","ratio":0.62,
               "a":{"camera":{"device":"dev-a","channel":3}},
               "b":{"split":"col","ratio":0.34,"a":{"camera":null},"b":{"camera":{"device":"dev-a","channel":9}}}}}]}
            """#
        #expect(SavedLayout.list(fromJSON: json) == [SavedLayout(id: "entrances", name: "Entrances", version: 1, root: contractTree)])
    }

    @Test func skipsWhatItCannotRead() {
        let json = #"""
            [{"id":"ok","name":"OK","root":{"camera":null}},
             {"id":"bad","name":"Bad","root":{"split":"row","ratio":2,"a":{"camera":null},"b":{"camera":null}}},
             {"id":"odd","name":"Odd","root":{"hexagon":true}},
             {"name":"no id","root":{"camera":null}}]
            """#
        #expect(SavedLayout.list(fromJSON: json).map(\.id) == ["ok"])
        #expect(SavedLayout.list(fromJSON: "nonsense").isEmpty)
    }

    @Test func roundTripsThroughTheFakeStore() throws {
        let rig = try Rig(try Fixtures.clip("h264-basic.h264"))
        defer { rig.bridge.shutdown() }
        let layout = SavedLayout(id: "l1", name: "Layout 1", root: contractTree)
        try rig.bridge.putLayout(json: layout.json)
        var stored = layout
        stored.version = 1
        #expect(SavedLayout.list(fromJSON: try rig.bridge.layoutsJSON()) == [stored])
    }
}

@Suite struct LayoutGeometryTests {
    private let bounds = CGRect(x: 0, y: 0, width: 1000, height: 600)

    @Test func framesWithAGap() {
        let frames = contractTree.frames(in: bounds, gap: 4)
        #expect(frames.leaves.count == 3)
        let a = frames.leaves[0].rect
        #expect(a == CGRect(x: 0, y: 0, width: 618, height: 600))
        let top = frames.leaves[1].rect
        let bottom = frames.leaves[2].rect
        #expect(top.minX == 622)
        #expect(top.maxX == 1000)
        #expect(bottom.minY == top.maxY + 4)
        #expect(bottom.maxY == 600)
        let root = frames.dividers.first { $0.path == [] }
        #expect(root?.rect == CGRect(x: 618, y: 0, width: 4, height: 600))
    }

    @Test func hitTesting() {
        let frames = contractTree.frames(in: bounds, gap: 2)
        #expect(frames.leaf(at: CGPoint(x: 10, y: 10))?.path == [.a])
        #expect(frames.leaf(at: CGPoint(x: 900, y: 590))?.path == [.b, .b])
        // The gap widens to a grabbable divider.
        #expect(frames.divider(at: CGPoint(x: 616, y: 300))?.path == [])
        // Where two dividers meet, the inner one wins.
        let inner = frames.dividers.first { $0.path == [.b] }!
        #expect(frames.divider(at: CGPoint(x: 700, y: inner.rect.midY))?.path == [.b])
        #expect(frames.divider(at: CGPoint(x: 300, y: 300)) == nil)
    }

    @Test func draggingADividerSetsItsRatio() {
        let frames = contractTree.frames(in: bounds, gap: 4)
        let root = frames.dividers.first { $0.path == [] }!
        let r = LayoutNode.ratio(dragging: root, to: CGPoint(x: 500, y: 100), gap: 4)
        #expect(abs(r - 0.5) < 0.001)
        #expect(LayoutNode.ratio(dragging: root, to: CGPoint(x: -50, y: 0), gap: 4) == LayoutNode.minRatio)
        let inner = frames.dividers.first { $0.path == [.b] }!
        let ri = LayoutNode.ratio(dragging: inner, to: CGPoint(x: 800, y: 450), gap: 4)
        #expect(abs(ri - 0.75) < 0.01)
        // Applying the ratio puts the divider under the pointer.
        let moved = contractTree.settingRatio(r, at: [])!.frames(in: bounds, gap: 4)
        #expect(abs((moved.dividers.first { $0.path == [] }!.rect.midX) - 500) <= 1)
    }

    @Test func pictureFit() {
        let cell = CGRect(x: 100, y: 50, width: 400, height: 400)
        let fit = PictureFit.fit(picture: CGSize(width: 1920, height: 1080), in: cell, fill: false)
        #expect(fit.destination == CGRect(x: 100, y: 137, width: 400, height: 225))
        #expect(fit.source == CGRect(x: 0, y: 0, width: 1920, height: 1080))
        let fill = PictureFit.fit(picture: CGSize(width: 1920, height: 1080), in: cell, fill: true)
        #expect(fill.destination == cell)
        #expect(fill.source == CGRect(x: 420, y: 0, width: 1080, height: 1080))
        let tall = PictureFit.fit(picture: CGSize(width: 1080, height: 1920), in: CGRect(x: 0, y: 0, width: 800, height: 450), fill: false)
        #expect(tall.destination.height == 450)
        #expect(abs(tall.destination.midX - 400) <= 1)
        let none = PictureFit.fit(picture: .zero, in: cell, fill: false)
        #expect(none.destination == cell)
    }

    @Test func focusAnimationMovesOnlyTheFocusedTile() {
        let grid = contractTree.frames(in: bounds, gap: 4)
        let start = RectAnimation.focusFrames(grid: grid, focused: [.b, .b], bounds: bounds, progress: 0)
        #expect(start.count == 3)
        #expect(start.last?.path == [.b, .b])  // drawn last, over the others
        #expect(start.last?.rect == grid.leaf([.b, .b])?.rect)
        let mid = RectAnimation.focusFrames(grid: grid, focused: [.b, .b], bounds: bounds, progress: 0.5)
        let r = mid.last!.rect
        #expect(r.width > grid.leaf([.b, .b])!.rect.width && r.width < bounds.width)
        #expect(mid.dropLast().map(\.rect) == grid.leaves.filter { $0.path != [.b, .b] }.map(\.rect))
        let end = RectAnimation.focusFrames(grid: grid, focused: [.b, .b], bounds: bounds, progress: 1)
        #expect(end.count == 1)
        #expect(end[0].rect == bounds)
        #expect(RectAnimation.focusFrames(grid: grid, focused: nil, bounds: bounds, progress: 1) == grid.leaves)
        #expect(RectAnimation.ease(0) == 0 && RectAnimation.ease(1) == 1 && RectAnimation.ease(0.5) == 0.5)
    }

    @Test func pixelRounding() {
        #expect(CGRect(x: 10.2, y: 0.4, width: 100.5, height: 50).pixels(scale: 2) == CGRect(x: 20, y: 1, width: 201, height: 100))
    }
}

@Suite struct ViewerStateTests {
    @Test func detectionTracker() {
        var t = DetectionTracker()
        let c = cam(3)
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "vehicle", active: true, atNanos: 10), camera: c)
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "people", active: true, atNanos: 20), camera: c)
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "motion", active: true, atNanos: 5), camera: c)
        #expect(t.active(for: c).map(\.kind) == ["people", "vehicle", "motion"])
        // The same kind again replaces, a stop removes.
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "people", active: true, atNanos: 30), camera: c)
        #expect(t.active(for: c).count == 3)
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "vehicle", active: false), camera: c)
        #expect(t.active(for: c).map(\.kind) == ["people", "motion"])
        // A local detection of the same kind is separate.
        t.apply(DetectionEvent(device: 1, channel: 3, kind: "people", active: true, source: .local, atNanos: 40), camera: c)
        #expect(t.active(for: c).count == 3)
        t.expire(now: 1_000, maxAgeNanos: 980)
        #expect(t.active(for: c).map(\.sinceNanos) == [30, 40])
        t.clear(device: "dev-a")
        #expect(t.active(for: c).isEmpty)
        #expect(DetectionTracker.label("people") == "Person")
        #expect(DetectionTracker.label("dog_cat") == "Animal")
        #expect(DetectionTracker.label("baby_crying") == "Baby Crying")
        #expect(DetectionTracker.sourceLabel(.device) == "camera AI")
        #expect(DetectionTracker.sourceLabel(.local) == "this Mac")
    }

    @Test func listenPolicy() {
        let a = cam(1), b = cam(2)
        #expect(ListenPolicy.toggle(a, in: [], additive: false) == [a])
        // One click moves listening; the previous tile is muted.
        #expect(ListenPolicy.toggle(b, in: [a], additive: false) == [b])
        // With the modifier, several.
        #expect(ListenPolicy.toggle(b, in: [a], additive: true) == [a, b])
        // Clicking the audible tile mutes it.
        #expect(ListenPolicy.toggle(a, in: [a, b], additive: false) == [b])
        #expect(ListenPolicy.toggle(a, in: [a], additive: true) == [])
    }

    @Test func veilWords() {
        func veil(_ s: DeviceState?, _ status: TileStatus? = nil, picture: Bool = false, since: Double = 100, now: Double = 100) -> TileVeil? {
            TileVeil.make(link: DeviceLink(state: s, since: since), status: status, hasPicture: picture, now: now)
        }
        #expect(veil(.connecting) == TileVeil(title: "Connecting"))
        #expect(
            veil(.backoff(attempt: 3, retryMilliseconds: 8000), now: 103.5)
                == TileVeil(title: "Reconnecting", detail: "Connection lost · attempt 3 · retrying in 5 s"))
        #expect(veil(.backoff(attempt: 1, retryMilliseconds: 1000), now: 200)?.detail?.hasSuffix("retrying now") == true)
        #expect(veil(.loginFailed(status: 401, remainTimes: 2))?.detail == "Check the credentials in Settings · 2 attempts left")
        #expect(veil(.loginFailed(status: 407, remainTimes: 0, reason: "account locked", unlockSeconds: 300))?.detail == "Account locked · locked for 5 min")
        #expect(veil(.connected, .waiting) == TileVeil(title: "Waiting for video"))
        #expect(veil(.connected, .playing) == nil)
        #expect(veil(.connected, .recovering, picture: true) == nil)
        #expect(veil(.connected, .undecodable, picture: true)?.title == "Cannot decode this stream")
        #expect(veil(.closed)?.title == "Disconnected")
    }
}

@Suite struct TileLeasesTests {
    private func rig() throws -> (Rig, TileLeases) {
        var o = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        o.loop = true
        let rig = try Rig(o)
        let leases = TileLeases(bridge: rig.bridge)
        let device = rig.device
        leases.resolve = { $0 == "dev-a" ? device : nil }
        return (rig, leases)
    }

    @Test func aCameraIsStreamedOnceHoweverManyTilesShowIt() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        var changes: [(CameraRef, Bool)] = []
        leases.onTileChange = { changes.append(($0, $1 != nil)) }
        let t1 = try #require(leases.acquire(cam(1)))
        let t2 = try #require(leases.acquire(cam(1)))
        #expect(t1 === t2)
        #expect(rig.bridge.tiles.count == 1)
        #expect(t1.openedTier == .sub)
        leases.acquire(cam(2))
        #expect(rig.bridge.tiles.count == 2)
        leases.release(cam(1))
        #expect(rig.bridge.tiles.count == 2)
        leases.release(cam(1))
        #expect(rig.bridge.tiles.count == 1)
        #expect(leases.tile(for: cam(1)) == nil)
        #expect(changes.map(\.1) == [true, true, false])
        // A device without an ID yet: the lease waits.
        #expect(leases.acquire(cam(1, "dev-unknown")) == nil)
        #expect(leases.leaseCount(cam(1, "dev-unknown")) == 1)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func updateDiffsLeases() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        leases.update(from: [:], to: [cam(1): 2, cam(2): 1])
        #expect(leases.leaseCount(cam(1)) == 2)
        leases.update(from: [cam(1): 2, cam(2): 1], to: [cam(1): 1, cam(3): 1])
        #expect(leases.leaseCount(cam(1)) == 1)
        #expect(leases.leaseCount(cam(2)) == 0)
        #expect(leases.leaseCount(cam(3)) == 1)
        #expect(Set(rig.bridge.tiles.map(\.channel)) == [1, 3])
    }

    @Test func focusSwitchesTiersOncePerCamera() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        let tile = try #require(leases.acquire(cam(1)))
        leases.acquire(cam(1))
        leases.setFocused(cam(1), true)
        #expect(tile.tierState.requested == .main)
        #expect(rig.fake.generations(tile: tile.id)?.running == [0, 1])
        // A second view focusing the same camera asks for nothing new.
        leases.setFocused(cam(1), true)
        #expect(rig.fake.generations(tile: tile.id)?.running == [0, 1])
        leases.setFocused(cam(1), false)
        #expect(tile.tierState.requested == .main)
        leases.setFocused(cam(1), false)
        #expect(tile.tierState.requested == .sub)
        #expect(rig.fake.violations.isEmpty)
    }

    @Test func tilesComeBackWithTheirDevice() throws {
        let (rig, leases) = try rig()
        defer { rig.bridge.shutdown() }
        var device: UInt64? = rig.device
        leases.resolve = { $0 == "dev-a" ? device : nil }
        let first = try #require(leases.acquire(cam(1)))
        leases.closeTiles(device: "dev-a")
        #expect(leases.tile(for: cam(1)) == nil)
        try rig.bridge.removeDevice(rig.device)
        device = try rig.bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "example")
        leases.reopen(device: "dev-a")
        let second = try #require(leases.tile(for: cam(1)))
        #expect(second !== first)
        #expect(second.device == device)
        // The core ending a tile releases our side; reopen brings it back.
        leases.tileEnded(second.id)
        #expect(leases.tile(for: cam(1)) == nil)
        leases.reopen(device: "dev-a")
        #expect(leases.tile(for: cam(1)) != nil)
        leases.closeAll()
        #expect(rig.bridge.tiles.isEmpty)
        #expect(rig.fake.violations.isEmpty)
    }
}
