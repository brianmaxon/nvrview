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

@Suite struct ZoomTests {
    private func close(_ a: CGPoint, _ b: CGPoint, _ e: CGFloat = 1e-9) -> Bool {
        abs(a.x - b.x) < e && abs(a.y - b.y) < e
    }

    private func close(_ a: CGRect, _ b: CGRect, _ e: CGFloat = 1e-6) -> Bool {
        abs(a.minX - b.minX) < e && abs(a.minY - b.minY) < e && abs(a.width - b.width) < e
            && abs(a.height - b.height) < e
    }

    @Test func oneXShowsTheWholeFrame() {
        #expect(TileZoom.none.visible() == TileZoom.wholeFrame)
        #expect(!TileZoom.none.isZoomed)
        let fit = CGRect(x: 0, y: 0, width: 640, height: 360)
        #expect(TileZoom.none.source(fit: fit, picture: fit.size) == fit)
    }

    @Test func zoomKeepsThePointUnderThePointer() {
        let u = CGPoint(x: 0.25, y: 0.8)
        let q = TileZoom.none.picturePoint(atUnit: u)
        let z = TileZoom.none.zoomed(by: 2, atUnit: u)
        #expect(z.scale == 2)
        #expect(close(z.picturePoint(atUnit: u), q))
        // Again, from a zoomed state, at another point.
        let u2 = CGPoint(x: 0.6, y: 0.4)
        let q2 = z.picturePoint(atUnit: u2)
        let z2 = z.zoomed(by: 1.5, atUnit: u2)
        #expect(close(z2.picturePoint(atUnit: u2), q2))
        #expect(abs(z2.scale - 3) < 1e-12)
    }

    @Test func zoomAtTheCenterIsCentered() {
        let z = TileZoom.none.zoomed(by: 4, atUnit: CGPoint(x: 0.5, y: 0.5))
        #expect(close(z.visible(), CGRect(x: 0.375, y: 0.375, width: 0.25, height: 0.25)))
    }

    @Test func scaleIsClampedOneToEight() {
        let u = CGPoint(x: 0.5, y: 0.5)
        #expect(TileZoom.none.zoomed(by: 100, atUnit: u).scale == TileZoom.maxScale)
        #expect(TileZoom.none.zoomed(by: 0.1, atUnit: u) == .none)
        let z = TileZoom(scale: 3, center: CGPoint(x: 0.9, y: 0.1))
        // Zooming back out to 1x forgets the center.
        #expect(z.zoomed(to: 1, keeping: CGPoint(x: 0.9, y: 0.1), atUnit: u) == .none)
    }

    @Test func zoomNearAnEdgeStaysInsideThePicture() {
        // At the very corner the region is pushed back inside.
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 1, y: 1))
        #expect(close(z.visible(), CGRect(x: 0.5, y: 0.5, width: 0.5, height: 0.5)))
        let odd = TileZoom(scale: 2, center: CGPoint(x: -3, y: 7)).clamped()
        #expect(close(odd.visible(), CGRect(x: 0, y: 0.5, width: 0.5, height: 0.5)))
    }

    @Test func panningFollowsTheDragAndIsClamped() {
        let z = TileZoom(scale: 4, center: CGPoint(x: 0.5, y: 0.5))
        // Dragging right by half the shown width moves the picture right:
        // the region moves left by half its own width (1/8 of the picture).
        let p = z.panned(byUnit: CGVector(dx: 0.5, dy: 0))
        #expect(close(p.center, CGPoint(x: 0.375, y: 0.5)))
        // Far beyond the edge it stops at the edge.
        let far = z.panned(byUnit: CGVector(dx: 100, dy: -100))
        #expect(close(far.visible(), CGRect(x: 0, y: 0.75, width: 0.25, height: 0.25)))
        // At 1x there is nothing to pan.
        #expect(TileZoom.none.panned(byUnit: CGVector(dx: 0.3, dy: 0.3)) == .none)
    }

    @Test func theRegionIsKeptInPictureCoordinatesAcrossResolutions() {
        // The same zoom on the sub-stream and on the main stream samples the
        // same part of the picture, now with more pixels.
        let z = TileZoom.none.zoomed(by: 3, atUnit: CGPoint(x: 0.7, y: 0.3))
        let sub = CGSize(width: 640, height: 360), main = CGSize(width: 2560, height: 1440)
        let s = z.source(fit: CGRect(origin: .zero, size: sub), picture: sub)
        let m = z.source(fit: CGRect(origin: .zero, size: main), picture: main)
        #expect(close(TileZoom.normalized(s, picture: sub), TileZoom.normalized(m, picture: main)))
        #expect(abs(m.width - 4 * s.width) < 1e-9)
    }

    @Test func fillCropsBeforeZooming() {
        // A 16:9 picture filling a square tile shows its middle square at 1x;
        // zooming stays inside that crop.
        let picture = CGSize(width: 1600, height: 900)
        let fit = PictureFit.fit(picture: picture, in: CGRect(x: 0, y: 0, width: 300, height: 300), fill: true).source
        #expect(fit == CGRect(x: 350, y: 0, width: 900, height: 900))
        let frame = TileZoom.normalized(fit, picture: picture)
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 0, y: 0), in: frame)
        #expect(close(z.source(fit: fit, picture: picture), CGRect(x: 350, y: 0, width: 450, height: 450)))
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
    @Test func zoomChangesOnlyTheSource() {
        let rect = CGRect(x: 10, y: 20, width: 400, height: 300)
        let picture = CGSize(width: 1280, height: 720)
        let plain = TilePlacement.place(rect: rect, scaleX: 2, scaleY: 2, picture: picture, fill: false)
        let z = TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 0.5, y: 0.5))
        let zoomed = TilePlacement.place(rect: rect, scaleX: 2, scaleY: 2, picture: picture, fill: false, zoom: z)
        #expect(zoomed.destination == plain.destination)
        #expect(plain.source == CGRect(origin: .zero, size: picture))
        #expect(zoomed.source == CGRect(x: 320, y: 180, width: 640, height: 360))
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

    @Test func unitPointsAreClampedToTheShownPicture() {
        let shown = TilePlacement.shownRect(
            rect: CGRect(x: 0, y: 0, width: 400, height: 400), picture: CGSize(width: 1600, height: 900), fill: false)
        #expect(shown == CGRect(x: 0, y: 87, width: 400, height: 225))
        #expect(TilePlacement.unit(CGPoint(x: 200, y: 10), in: shown) == CGPoint(x: 0.5, y: 0))
        #expect(TilePlacement.unit(CGPoint(x: 100, y: 87 + 225), in: shown) == CGPoint(x: 0.25, y: 1))
    }
}
