import CoreGraphics
import Foundation
import NVRCore
import Testing

private func cam(_ channel: Int) -> CameraRef { CameraRef(device: "dev-a", channel: channel) }

private let bounds = CGRect(x: 0, y: 0, width: 1000, height: 600)

/// The path of the leaf showing `channel`.
private func path(_ tree: LayoutNode, _ channel: Int) -> NodePath {
    tree.leaves.first { $0.camera == cam(channel) }!.path
}

@Suite struct SpatialNavigationTests {
    @Test func evenGridMovesByPosition() {
        // 2x2: 0 1 / 2 3, as rows of columns.
        let tree = LayoutNode.split(
            .col, ratio: 0.5, a: .split(.row, ratio: 0.5, a: .leaf(cam(0)), b: .leaf(cam(1))),
            b: .split(.row, ratio: 0.5, a: .leaf(cam(2)), b: .leaf(cam(3))))
        let f = tree.frames(in: bounds, gap: 2)
        #expect(f.neighbor(of: path(tree, 0), .right) == path(tree, 1))
        #expect(f.neighbor(of: path(tree, 0), .down) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 3), .up) == path(tree, 1))
        #expect(f.neighbor(of: path(tree, 3), .left) == path(tree, 2))
        // Edges stay put.
        #expect(f.neighbor(of: path(tree, 0), .left) == nil)
        #expect(f.neighbor(of: path(tree, 0), .up) == nil)
        #expect(f.neighbor(of: path(tree, 3), .right) == nil)
        #expect(f.neighbor(of: path(tree, 3), .down) == nil)
    }

    @Test func unevenSplitsPreferTheAdjacentTile() {
        // A big tile on the left; on the right a short one above two stacked.
        //  +--------+----+
        //  |        | 1  |
        //  |   0    +----+
        //  |        | 2  |
        //  |        +----+
        //  |        | 3  |
        //  +--------+----+
        let tree = LayoutNode.split(
            .row, ratio: 0.62, a: .leaf(cam(0)),
            b: .split(.col, ratio: 0.34, a: .leaf(cam(1)), b: .split(.col, ratio: 0.5, a: .leaf(cam(2)), b: .leaf(cam(3)))))
        let f = tree.frames(in: bounds)
        // From the big tile, right goes to the one nearest its center.
        #expect(f.neighbor(of: path(tree, 0), .right) == path(tree, 2))
        // Every right-hand tile goes back left to the big one.
        for c in [1, 2, 3] { #expect(f.neighbor(of: path(tree, c), .left) == path(tree, 0)) }
        #expect(f.neighbor(of: path(tree, 1), .down) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 2), .down) == path(tree, 3))
        #expect(f.neighbor(of: path(tree, 3), .up) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 0), .up) == nil)
        #expect(f.neighbor(of: path(tree, 0), .down) == nil)
    }

    @Test func overlapBeatsADiagonalThatIsCloser() {
        //  +---+-------+
        //  | 0 |       |
        //  +---+   2   |
        //  | 1 |       |
        //  +---+---+---+
        //  |   3   | 4 |
        //  +-------+---+
        // From 1, down must be 3 (directly below) even though 4 is beyond too.
        let tree = LayoutNode.split(
            .col, ratio: 0.6,
            a: .split(.row, ratio: 0.3, a: .split(.col, ratio: 0.5, a: .leaf(cam(0)), b: .leaf(cam(1))), b: .leaf(cam(2))),
            b: .split(.row, ratio: 0.7, a: .leaf(cam(3)), b: .leaf(cam(4))))
        let f = tree.frames(in: bounds)
        #expect(f.neighbor(of: path(tree, 1), .down) == path(tree, 3))
        #expect(f.neighbor(of: path(tree, 2), .down) == path(tree, 3))
        #expect(f.neighbor(of: path(tree, 4), .up) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 3), .up) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 0), .right) == path(tree, 2))
        #expect(f.neighbor(of: path(tree, 2), .left) == path(tree, 0))
    }

    @Test func singleTileAndUnknownPathHaveNoNeighbor() {
        let f = LayoutNode.leaf(cam(0)).frames(in: bounds)
        for d in MoveDirection.allCases { #expect(f.neighbor(of: [], d) == nil) }
        #expect(f.neighbor(of: [.a], .right) == nil)
    }

    @Test func everyTileIsReachableInABalancedGrid() {
        let tree = LayoutNode.balanced((0..<7).map { cam($0) })
        let f = tree.frames(in: bounds, gap: 2)
        var seen: Set<NodePath> = [f.leaves[0].path]
        var frontier = [f.leaves[0].path]
        while let p = frontier.popLast() {
            for d in MoveDirection.allCases {
                if let n = f.neighbor(of: p, d), seen.insert(n).inserted { frontier.append(n) }
            }
        }
        #expect(seen.count == f.leaves.count)
    }
}

@Suite struct KeyRoutingTests {
    private func key(
        _ code: UInt16, _ chars: String? = nil, _ mods: KeyModifiers = [], repeat r: Bool = false,
        in context: KeyContext = .stage
    ) -> ViewerShortcut? {
        ViewerKeys.shortcut(keyCode: code, characters: chars, modifiers: mods, isRepeat: r, context: context)
    }

    @Test func singleKeysOnTheStage() {
        #expect(key(3, "f") == .toggleFullScreen)
        #expect(key(46, "m") == .toggleListening)
        #expect(key(46, "M", .shift) == .toggleListening)
        #expect(key(46, "m", .command) == nil)  // Cmd-M stays Minimize
        #expect(key(1, "s") == .toggleSidebar)
        #expect(key(29, "0") == .resetZoom)
        #expect(key(82, "0") == .resetZoom)  // keypad
        #expect(key(ViewerKeys.space, " ") == .toggleFocus)
        #expect(key(ViewerKeys.leftArrow) == .move(.left))
        #expect(key(ViewerKeys.rightArrow) == .move(.right))
        #expect(key(ViewerKeys.upArrow) == .move(.up))
        #expect(key(ViewerKeys.downArrow) == .move(.down))
        #expect(key(ViewerKeys.returnKey, "\r") == .confirm)
        #expect(key(ViewerKeys.enter, "\u{3}") == .confirm)
        #expect(key(2, "d") == nil)
    }

    @Test func captureAndCameraKeys() {
        #expect(key(35, "p") == .savePicture)
        #expect(key(35, "P") == .savePicture)  // Caps Lock
        #expect(key(35, "P", .shift) == .savePictureAs)
        #expect(key(15, "r") == .toggleRecording)
        #expect(key(15, "R", .shift) == .toggleRecording)
        #expect(key(17, "t") == .toggleConversation)
        // ⇧⌘P and ⇧⌘R are menu key equivalents, not single keys.
        #expect(key(35, "p", [.command, .shift]) == nil)
        #expect(key(15, "r", [.command, .shift]) == nil)
        #expect(key(35, "p", repeat: true) == nil)
        #expect(key(15, "r", repeat: true) == nil)
        for (code, chars) in [(35, "p"), (15, "r"), (17, "t")] as [(UInt16, String)] {
            #expect(key(code, chars, in: .textEditing) == nil)
            #expect(key(code, chars, .shift, in: .textEditing) == nil)
        }
    }

    @Test func lettersInEitherCase() {
        #expect(key(3, "F", .shift) == .toggleFullScreen)
        #expect(key(3, "F") == .toggleFullScreen)  // Caps Lock
        #expect(key(1, "S", .shift) == .toggleSidebar)
    }

    @Test func menuModifiersLeaveTheKeyToTheMenus() {
        for m: KeyModifiers in [.command, .control, .option, [.command, .control]] {
            #expect(key(3, "f", m) == nil)
            #expect(key(ViewerKeys.leftArrow, nil, m) == nil)
            #expect(key(37, "l", m) == nil)
        }
    }

    @Test func nothingFiresWhileEditingText() {
        for (code, chars) in [(3, "f"), (1, "s"), (29, "0"), (49, " "), (123, ""), (36, "\r")] as [(UInt16, String)] {
            #expect(key(code, chars, in: .textEditing) == nil)
            #expect(key(code, chars, .shift, in: .textEditing) == nil)
        }
    }

    @Test func theSidebarKeepsArrowsSpaceAndReturn() {
        #expect(key(ViewerKeys.leftArrow, nil, in: .sidebar) == nil)
        #expect(key(ViewerKeys.downArrow, nil, in: .sidebar) == nil)
        #expect(key(ViewerKeys.space, " ", in: .sidebar) == nil)
        #expect(key(ViewerKeys.returnKey, "\r", in: .sidebar) == nil)
        #expect(key(3, "f", in: .sidebar) == .toggleFullScreen)
        #expect(key(1, "s", in: .sidebar) == .toggleSidebar)
    }

    @Test func togglesIgnoreRepeatArrowsRepeat() {
        #expect(key(3, "f", repeat: true) == nil)
        #expect(key(ViewerKeys.space, " ", repeat: true) == nil)
        #expect(key(ViewerKeys.rightArrow, nil, repeat: true) == .move(.right))
    }
}

/// One step of `idle` (a mutating call cannot sit inside `#expect`).
private func step(_ idle: inout ChromeIdle, _ what: String, _ now: Double) -> Bool {
    what == "tick" ? idle.tick(now: now) : idle.interact(now: now)
}

@Suite struct ChromeIdleTests {
    @Test func hidesAfterTheDelayAndReturnsOnInteraction() {
        var idle = ChromeIdle(delay: 3, now: 100)
        #expect(idle.visible)
        #expect(!step(&idle, "tick", 102.9))
        #expect(step(&idle, "tick", 103))
        #expect(!idle.visible)
        // Hides once.
        #expect(!step(&idle, "tick", 110))
        #expect(step(&idle, "interact", 111))
        #expect(idle.visible)
        // Interaction while visible restarts the wait without "showing" again.
        #expect(!step(&idle, "interact", 113))
        #expect(!step(&idle, "tick", 115.5))
        #expect(step(&idle, "tick", 116))
    }

    @Test func steadyInteractionKeepsItUp() {
        var idle = ChromeIdle(delay: 3, now: 0)
        for t in stride(from: 0.0, through: 30, by: 0.5) {
            _ = idle.interact(now: t)
            #expect(!step(&idle, "tick", t + 0.25))
        }
        #expect(idle.visible)
    }
}

// The digital zoom's vectors are mirrored in
// apps/windows/NvrView.Tiles.Tests/ZoomTests.cs (parity: zoom-fills-tile).
@Suite struct ZoomTests {
    private func close(_ a: CGPoint, _ b: CGPoint, _ e: CGFloat = 1e-9) -> Bool {
        abs(a.x - b.x) < e && abs(a.y - b.y) < e
    }

    private func close(_ a: CGRect, _ b: CGRect, _ e: CGFloat = 1e-6) -> Bool {
        abs(a.minX - b.minX) < e && abs(a.minY - b.minY) < e && abs(a.width - b.width) < e
            && abs(a.height - b.height) < e
    }

    private let center = CGPoint(x: 0.5, y: 0.5)
    /// A 16:9 picture fitted in a square tile: bars above and below.
    private let wideInTall = ZoomBase(picture: CGSize(width: 1600, height: 900), tile: CGSize(width: 400, height: 400), fill: false)
    /// A 9:16 picture fitted in a 16:9 tile: bars left and right.
    private let tallInWide = ZoomBase(picture: CGSize(width: 900, height: 1600), tile: CGSize(width: 800, height: 450), fill: false)
    /// A 16:9 picture filling a square tile: cropped left and right.
    private let filled = ZoomBase(picture: CGSize(width: 1600, height: 900), tile: CGSize(width: 300, height: 300), fill: true)

    @Test func theBaseIsThePictureAsAMultipleOfTheTile() {
        #expect(wideInTall == ZoomBase(width: 1, height: 0.5625))
        #expect(tallInWide == ZoomBase(width: 0.31640625, height: 1))
        #expect(abs(filled.width - 16.0 / 9) < 1e-12 && filled.height == 1)
        #expect(ZoomBase(picture: .zero, tile: CGSize(width: 10, height: 10), fill: false) == .whole)
    }

    @Test func oneXLooksAsUnzoomed() {
        #expect(!TileZoom.none.isZoomed)
        #expect(TileZoom.none.visible() == CGRect(x: 0, y: 0, width: 1, height: 1))
        // Letterboxed: the whole picture between the bars.
        #expect(close(TileZoom.none.destination(in: wideInTall), CGRect(x: 0, y: 0.21875, width: 1, height: 0.5625)))
        #expect(close(TileZoom.none.visible(in: wideInTall), CGRect(x: 0, y: 0, width: 1, height: 1)))
        // Filled: the whole tile, the middle of the picture.
        #expect(close(TileZoom.none.destination(in: filled), CGRect(x: 0, y: 0, width: 1, height: 1)))
        #expect(close(TileZoom.none.visible(in: filled), CGRect(x: 0.21875, y: 0, width: 0.5625, height: 1)))
    }

    @Test func zoomingGrowsThePictureOverTheBars() {
        // 1.5x: wider than the tile (cropped), still shorter (smaller bars).
        let z = TileZoom(scale: 1.5, center: center)
        #expect(close(z.destination(in: wideInTall), CGRect(x: 0, y: 0.078125, width: 1, height: 0.84375)))
        #expect(close(z.visible(in: wideInTall), CGRect(x: 1.0 / 6, y: 0, width: 2.0 / 3, height: 1)))
        // 2x: past filling the height, the whole tile, cropped on both axes.
        let z2 = TileZoom(scale: 2, center: center)
        #expect(close(z2.destination(in: wideInTall), CGRect(x: 0, y: 0, width: 1, height: 1)))
        #expect(close(z2.visible(in: wideInTall), CGRect(x: 0.25, y: 0.0625 / 1.125, width: 0.5, height: 1 / 1.125)))
    }

    @Test func theBarsGoAtTheFillThreshold() {
        // The picture fills the height at 1 / 0.5625 = 1.777...x.
        let below = TileZoom(scale: 1.77, center: center).destination(in: wideInTall)
        #expect(close(below, CGRect(x: 0, y: 0.0021875, width: 1, height: 0.995625)))
        let past = TileZoom(scale: 1.78, center: center)
        #expect(close(past.destination(in: wideInTall), CGRect(x: 0, y: 0, width: 1, height: 1)))
        #expect(close(past.visible(in: wideInTall), CGRect(x: 0.5 - 0.5 / 1.78, y: 0.5 - 0.5 / 1.00125, width: 1 / 1.78, height: 1 / 1.00125)))
    }

    @Test func aTallPictureGrowsSideways() {
        let z = TileZoom(scale: 2, center: center)
        #expect(close(z.destination(in: tallInWide), CGRect(x: 0.18359375, y: 0, width: 0.6328125, height: 1)))
        #expect(close(z.visible(in: tallInWide), CGRect(x: 0, y: 0.25, width: 1, height: 0.5)))
        let z4 = TileZoom(scale: 4, center: center)
        #expect(close(z4.destination(in: tallInWide), CGRect(x: 0, y: 0, width: 1, height: 1)))
        #expect(close(z4.visible(in: tallInWide), CGRect(x: 0.5 - 0.5 / 1.265625, y: 0.375, width: 1 / 1.265625, height: 0.25)))
    }

    @Test func scaleIsClampedOneToEight() {
        let top = TileZoom.none.zoomed(by: 100, atUnit: center, in: wideInTall)
        #expect(top.scale == TileZoom.maxScale)
        #expect(close(top.visible(in: wideInTall), CGRect(x: 0.4375, y: 0.5 - 1.0 / 9, width: 0.125, height: 2.0 / 9)))
        #expect(TileZoom.none.zoomed(by: 0.1, atUnit: center, in: wideInTall) == .none)
        // Zooming back out to 1x forgets the center.
        let z = TileZoom(scale: 3, center: CGPoint(x: 0.9, y: 0.1))
        #expect(z.zoomed(to: 1, keeping: CGPoint(x: 0.9, y: 0.1), atUnit: center, in: wideInTall) == .none)
    }

    @Test func zoomKeepsThePointUnderThePointer() {
        // A picture the tile's shape: fixed on both axes.
        let u = CGPoint(x: 0.25, y: 0.8)
        let q = TileZoom.none.picturePoint(atUnit: u)
        let z = TileZoom.none.zoomed(by: 2, atUnit: u)
        #expect(z.scale == 2)
        #expect(close(z.picturePoint(atUnit: u), q))
        // Letterboxed, from 2x (filling the tile) on: fixed on both axes.
        let u1 = CGPoint(x: 0.25, y: 0.4)
        let z1 = TileZoom.none.zoomed(by: 2, atUnit: u1, in: wideInTall)
        // Across the picture's width (it filled that axis at 1x) the point stays.
        #expect(abs(z1.picturePoint(atUnit: u1, in: wideInTall).x - 0.25) < 1e-9)
        let u2 = CGPoint(x: 0.6, y: 0.4)
        let q2 = z1.picturePoint(atUnit: u2, in: wideInTall)
        let z2 = z1.zoomed(by: 1.5, atUnit: u2, in: wideInTall)
        #expect(abs(z2.scale - 3) < 1e-12)
        #expect(close(z2.picturePoint(atUnit: u2, in: wideInTall), q2))
    }

    @Test func zoomNearAnEdgeStaysInsideThePicture() {
        // At the very corner the picture is pushed back to cover the tile.
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 1, y: 1))
        #expect(close(z.visible(), CGRect(x: 0.5, y: 0.5, width: 0.5, height: 0.5)))
        let odd = TileZoom(scale: 2, center: CGPoint(x: -3, y: 7)).clamped()
        #expect(close(odd.visible(), CGRect(x: 0, y: 0.5, width: 0.5, height: 0.5)))
        // An axis the picture does not fill yet stays centered.
        let short = TileZoom(scale: 1.5, center: CGPoint(x: 0.9, y: 0.9)).clamped(in: wideInTall)
        #expect(close(short.center, CGPoint(x: 1 - 1 / 3.0, y: 0.5)))
    }

    @Test func panningFollowsTheDragAndIsClamped() {
        let z = TileZoom(scale: 4, center: center)
        // Dragging right by half the tile moves the picture right: the
        // region moves left by half its own width (1/8 of the picture).
        #expect(close(z.panned(byUnit: CGVector(dx: 0.5, dy: 0)).center, CGPoint(x: 0.375, y: 0.5)))
        // Far beyond the edge it stops at the picture's edge.
        let far = z.panned(byUnit: CGVector(dx: 100, dy: -100), in: wideInTall)
        #expect(close(far.visible(in: wideInTall), CGRect(x: 0, y: 1 - 1 / 2.25, width: 0.25, height: 1 / 2.25)))
        #expect(close(far.destination(in: wideInTall), CGRect(x: 0, y: 0, width: 1, height: 1)))
        // Across an axis the picture does not fill, nothing moves.
        let short = TileZoom(scale: 1.5, center: center)
        let moved = short.panned(byUnit: CGVector(dx: 0, dy: 0.3), in: wideInTall)
        #expect(close(moved.destination(in: wideInTall), short.destination(in: wideInTall)))
        // At 1x there is nothing to pan.
        #expect(TileZoom.none.panned(byUnit: CGVector(dx: 0.3, dy: 0.3)) == .none)
    }

    @Test func fillZoomsFromTheCropAndPansToThePictureEdges() {
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 0, y: 0), in: filled)
        #expect(close(z.visible(in: filled), CGRect(x: 0.21875, y: 0, width: 0.28125, height: 0.5)))
        #expect(close(z.destination(in: filled), CGRect(x: 0, y: 0, width: 1, height: 1)))
        // Panned far right: the picture's left edge, beyond the 1x crop.
        let left = z.panned(byUnit: CGVector(dx: 100, dy: 0), in: filled)
        #expect(close(left.visible(in: filled), CGRect(x: 0, y: 0, width: 0.28125, height: 0.5)))
    }

    @Test func theRegionIsKeptInPictureCoordinatesAcrossResolutions() {
        // The same zoom on the sub-stream and on the main stream samples the
        // same part of the picture, now with more pixels.
        let z = TileZoom.none.zoomed(by: 3, atUnit: CGPoint(x: 0.7, y: 0.3))
        let rect = CGRect(x: 0, y: 0, width: 320, height: 180)
        let sub = TilePlacement.place(rect: rect, scaleX: 1, scaleY: 1, picture: CGSize(width: 640, height: 360), fill: false, zoom: z)
        let main = TilePlacement.place(rect: rect, scaleX: 1, scaleY: 1, picture: CGSize(width: 2560, height: 1440), fill: false, zoom: z)
        #expect(sub.destination == main.destination)
        if let s = sub.source, let m = main.source {
            #expect(close(CGRect(x: s.minX * 4, y: s.minY * 4, width: s.width * 4, height: s.height * 4), m))
        } else {
            Issue.record("no source")
        }
    }

    @Test func easingReachesTheTarget() {
        var s = 1.0
        var steps = 0
        while s != 2, steps < 1000 {
            s = TileZoom.eased(from: s, to: 2, dt: 1.0 / 120)
            #expect(s <= 2)
            steps += 1
        }
        #expect(s == 2)
        // About a quarter second at 120 Hz, not one jump and not forever.
        #expect(steps > 5 && steps < 120)
    }
}

@Suite struct TilePlacementTests {
    @Test func zoomGrowsTheDestinationOverTheBars() {
        // 4:3 tile, 16:9 picture: bars above and below at 1x.
        let rect = CGRect(x: 10, y: 20, width: 400, height: 300)
        let picture = CGSize(width: 1280, height: 720)
        let plain = TilePlacement.place(rect: rect, scaleX: 2, scaleY: 2, picture: picture, fill: false)
        #expect(plain.destination == CGRect(x: 20, y: 115, width: 800, height: 450))
        #expect(plain.source == CGRect(origin: .zero, size: picture))
        let small = TilePlacement.place(
            rect: rect, scaleX: 2, scaleY: 2, picture: picture, fill: false, zoom: TileZoom(scale: 1.2, center: CGPoint(x: 0.5, y: 0.5)))
        #expect(small.destination.minX == 20 && small.destination.width == 800)
        #expect(abs(small.destination.minY - 70) < 1e-9 && abs(small.destination.height - 540) < 1e-9)
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 0.5, y: 0.5))
        let zoomed = TilePlacement.place(rect: rect, scaleX: 2, scaleY: 2, picture: picture, fill: false, zoom: z)
        #expect(zoomed.destination == CGRect(x: 20, y: 40, width: 800, height: 600))
        #expect(zoomed.source == CGRect(x: 320, y: 120, width: 640, height: 480))
        // The zoom map's picture rectangle, in points, follows.
        #expect(TilePlacement.shownRect(rect: rect, picture: picture, fill: false, zoom: z) == rect)
    }

    @Test func theSourceMatchesTheZoomWithACodedSizeOffset() {
        let rect = CGRect(x: 0, y: 0, width: 320, height: 180)
        let picture = CGSize(width: 1920, height: 1080)
        let z = TileZoom(scale: 4, center: CGPoint(x: 0.125, y: 0.875))
        let p = TilePlacement.place(
            rect: rect, scaleX: 1, scaleY: 1, picture: picture, offset: CGPoint(x: 0, y: 8), fill: false, zoom: z)
        let v = z.visible()
        #expect(
            p.source
                == CGRect(
                    x: v.minX * picture.width, y: v.minY * picture.height + 8, width: v.width * picture.width,
                    height: v.height * picture.height))
    }

    @Test func noPictureDrawsTheCell() {
        let p = TilePlacement.place(
            rect: CGRect(x: 0, y: 0, width: 100, height: 50), scaleX: 2, scaleY: 2, picture: .zero, fill: false)
        #expect(p.destination == CGRect(x: 0, y: 0, width: 200, height: 100))
        #expect(p.source == nil)
    }

    @Test func unitPointsAreInTheTile() {
        let tile = CGRect(x: 0, y: 0, width: 400, height: 400)
        #expect(
            TilePlacement.shownRect(rect: tile, picture: CGSize(width: 1600, height: 900), fill: false)
                == CGRect(x: 0, y: 87, width: 400, height: 225))
        #expect(TilePlacement.unit(CGPoint(x: 200, y: 10), in: tile) == CGPoint(x: 0.5, y: 0.025))
        #expect(TilePlacement.unit(CGPoint(x: 100, y: 500), in: tile) == CGPoint(x: 0.25, y: 1))
    }
}
