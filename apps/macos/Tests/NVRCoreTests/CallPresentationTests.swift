import Foundation
import NVRCore
import Testing

// Calls in their own window and movable insets (docs/UX.md "Alert insets",
// "Doorbell calls"): the pure rules. The vectors are plain tables so the
// Windows app's tests (NvrView.Tiles.Tests) can mirror them exactly; keep the
// two copies in step.

@Suite struct CallPresentationTests {
    let door = CameraRef(device: "d-1", channel: 3)
    let yard = CameraRef(device: "d-1", channel: 1)
    let t0 = Date(timeIntervalSince1970: 1_700_000_000)

    static func near(_ a: Double, _ b: Double) -> Bool { abs(a - b) < 1e-9 }

    // MARK: Snap, one axis

    /// (position, size, stage) -> fraction. Margin 12, snap 24.
    static let fractionVectors: [(position: Double, size: Double, stage: Double, fraction: Double)] = [
        // Free space 100 (424 - 2 * 12 - 300).
        (12, 300, 424, 0),  // at the start margin
        (36, 300, 424, 0),  // 24 from the start: snaps
        (37, 300, 424, 0.25),  // 25 from the start: free
        (62, 300, 424, 0.5),
        (87, 300, 424, 0.75),  // 25 from the end: free
        (88, 300, 424, 1),  // 24 from the end: snaps
        (112, 300, 424, 1),  // at the end margin
        (-50, 300, 424, 0),  // dragged past the start
        (500, 300, 424, 1),  // dragged past the end
        // Free space 40: both ends within reach; the nearer wins, the start on a tie.
        (32, 300, 364, 0),
        (33, 300, 364, 1),
        // No free space.
        (12, 300, 300, 0),
        (100, 300, 300, 0),
    ]

    @Test func eachAxisSnapsToItsEdgeWithin24Points() {
        for v in Self.fractionVectors {
            let f = InsetPlacement.fraction(v.position, size: v.size, stage: v.stage)
            #expect(Self.near(f, v.fraction), "position \(v.position) size \(v.size) stage \(v.stage): \(f)")
        }
    }

    /// (origin, column, stage) -> anchor.
    static let anchorVectors:
        [(
            origin: (x: Double, y: Double), column: (width: Double, height: Double),
            stage: (width: Double, height: Double), anchor: (x: Double, y: Double)
        )] = [
            // Free space 676 x 376.
            ((181, 106), (300, 200), (1000, 600), (0.25, 0.25)),
            ((350, 200), (300, 200), (1000, 600), (0.5, 0.5)),
            ((688, 30), (300, 200), (1000, 600), (1, 0)),
            ((350, 400), (300, 200), (1000, 600), (0.5, 1)),
            ((0, 0), (300, 200), (1000, 600), (0, 0)),
        ]

    @Test func aDraggedColumnBecomesAFractionOfTheFreeSpace() {
        for v in Self.anchorVectors {
            let a = InsetPlacement.anchor(origin: v.origin, column: v.column, stage: v.stage)
            #expect(Self.near(a.x, v.anchor.x) && Self.near(a.y, v.anchor.y), "origin \(v.origin): \(a)")
        }
        // Away from the edges the origin comes back.
        let back = InsetPlacement.origin(
            anchor: InsetAnchor(x: 0.25, y: 0.25), column: (300, 200), stage: (1000, 600))
        #expect(back.x == 181 && back.y == 106)
        // A resize keeps the fraction: the same anchor on a larger stage.
        let wide = InsetPlacement.origin(anchor: InsetAnchor(x: 1, y: 0.5), column: (300, 200), stage: (2000, 1000))
        #expect(wide.x == 2000 - 12 - 300 && wide.y == 12 + 0.5 * (1000 - 24 - 200))
    }

    @Test func anchorsAreClampedAndDecodeClamped() throws {
        #expect(InsetAnchor(x: -1, y: 2) == InsetAnchor(x: 0, y: 1))
        #expect(InsetAnchor(x: .nan, y: .infinity) == InsetAnchor(x: 0, y: 0))
        #expect(InsetAnchor(corner: .topLeft) == InsetAnchor(x: 0, y: 0))
        #expect(InsetAnchor(corner: .topRight) == InsetAnchor(x: 1, y: 0))
        #expect(InsetAnchor(corner: .bottomLeft) == InsetAnchor(x: 0, y: 1))
        #expect(InsetAnchor(corner: .bottomRight) == InsetAnchor(x: 1, y: 1))
        let decoded = try JSONDecoder().decode(InsetAnchor.self, from: Data(#"{"x":1.5,"y":0.25}"#.utf8))
        #expect(decoded == InsetAnchor(x: 1, y: 0.25))
        let round = try JSONDecoder().decode(InsetAnchor.self, from: JSONEncoder().encode(InsetAnchor(x: 0.3, y: 0.7)))
        #expect(round == InsetAnchor(x: 0.3, y: 0.7))
        #expect(!InsetAnchor(x: 0, y: 0.5).growsUpward && InsetAnchor(x: 0, y: 0.51).growsUpward)
    }

    // MARK: The column

    /// Stage 1000 x 600, inset width 300: (anchor, heights) -> (x, y) per drawn inset, newest first.
    static let columnVectors: [(anchor: (x: Double, y: Double), heights: [Double], frames: [(x: Double, y: Double)])] =
        [
            // Corners: the same as the corner rule (two of three fit).
            ((0, 0), [200, 200, 200], [(12, 12), (12, 220)]),
            ((1, 0), [200, 200, 200], [(688, 12), (688, 220)]),
            ((0, 1), [200, 200, 200], [(12, 388), (12, 180)]),
            ((1, 1), [200, 200, 200], [(688, 388), (688, 180)]),
            // The middle grows downward.
            ((0.5, 0.5), [100, 100], [(350, 196), (350, 304)]),
            // Below the middle grows upward: the newest at the column's bottom.
            ((0.25, 0.75), [100, 150], [(181, 408.5), (181, 250.5)]),
            // Above the middle grows downward.
            ((0.25, 0.25), [100, 150], [(181, 91.5), (181, 199.5)]),
            // One inset taller than the stage is still drawn.
            ((0, 1), [700], [(12, -112)]),
            ((0, 0), [700, 100], [(12, 12)]),
            ((0, 0), [], []),
        ]

    @Test func theColumnSitsAtItsAnchorAndGrowsAwayFromTheNearerEdge() {
        for v in Self.columnVectors {
            let frames = InsetGeometry.frames(
                count: v.heights.count, width: 300, heights: v.heights, anchor: InsetAnchor(x: v.anchor.x, y: v.anchor.y),
                stage: (1000, 600))
            #expect(frames.count == v.frames.count, "anchor \(v.anchor) heights \(v.heights)")
            for (f, want) in zip(frames, v.frames) {
                #expect(Self.near(f.x, want.x) && Self.near(f.y, want.y), "anchor \(v.anchor): \(f) want \(want)")
                #expect(f.width == 300)
            }
        }
    }

    @Test func theCornerRuleIsTheAnchorRuleAtTheCorners() {
        let heights: [Double] = [120, 180, 90, 300]
        for corner in [ChromeCorner.topLeft, .topRight, .bottomLeft, .bottomRight] {
            for stage in [(1000.0, 600.0), (640.0, 360.0), (300.0, 200.0)] {
                let a = InsetGeometry.frames(count: 4, width: 220, heights: heights, corner: corner, stage: stage)
                let b = InsetGeometry.frames(
                    count: 4, width: 220, heights: heights, anchor: InsetAnchor(corner: corner), stage: stage)
                #expect(a.map(\.x) == b.map(\.x) && a.map(\.y) == b.map(\.y))
            }
        }
    }

    // MARK: Presentation

    /// (global "Open calls in their own window", per-doorbell override) -> presentation.
    static let resolveVectors: [(global: Bool, override: CallPresentation?, result: CallPresentation)] = [
        (false, nil, .inset),
        (true, nil, .window),
        (false, .window, .window),
        (true, .inset, .inset),
        (false, .inset, .inset),
        (true, .window, .window),
    ]

    @Test func theDoorbellsChoiceWinsOverTheGlobalDefault() {
        for v in Self.resolveVectors {
            #expect(CallPresentation.resolve(global: v.global, override: v.override) == v.result)
        }
    }

    @Test func aCallKeepsItsPresentationLatchAndDraft() {
        var call = DoorbellCall(camera: door, startedAt: t0, now: 10, audio: .audible(muted: []))
        #expect(call.presentation == .inset && !call.latched && call.draft.isEmpty)
        call = DoorbellCall(camera: door, startedAt: t0, now: 10, audio: .overlay, presentation: .window)
        #expect(call.presentation == .window)
        // A press joining the call keeps its presentation.
        call.pressed(now: 12)
        #expect(call.presentation == .window && call.presses == 2)
        // Keep talking and typing are activity.
        call.setLatched(true, now: 20)
        #expect(call.latched && call.lastActivity == 20)
        call.microphoneStopped()
        #expect(!call.latched)
        call.edited(draft: "One moment", now: 30)
        #expect(call.draft == "One moment" && call.lastActivity == 30)
        // Sent: the field empties, unless the user typed on meanwhile.
        call.edited(draft: "One moment please", now: 31)
        call.sent("One moment")
        #expect(call.draft == "One moment please")
        call.sent("One moment please")
        #expect(call.draft.isEmpty)
        #expect(!call.expired(now: 31 + DoorbellCall.idleTimeout - 1))
        #expect(call.expired(now: 31 + DoorbellCall.idleTimeout))
    }

    // MARK: Detached insets

    @Test func aCallInItsOwnWindowKeepsADetachedItem() {
        var board = InsetBoard()
        // Recorded even though the doorbell is visible.
        board.started(.doorbell, camera: door, visible: true, now: 0, at: t0, detached: true)
        #expect(board.shown.isEmpty && board.more == 0 && board.isEmpty)
        #expect(board.detachedCameras == [door])
        #expect(board.item(for: door)?.detached == true)
        // Not closed by the clock: it lives as long as the call.
        board.stopped(.doorbell, camera: door, now: 1)
        #expect(board.deadline == nil)
        #expect(board.tick(now: 100_000).isEmpty)
        // Another press keeps it detached.
        board.started(.doorbell, camera: door, visible: false, now: 5, at: t0 + 5)
        #expect(board.item(for: door)?.detached == true && board.item(for: door)?.count == 2)
        board.stopped(.doorbell, camera: door, now: 6)
        // Swap, Tab and Esc pass it by.
        board.swap(door, displaced: yard, now: 6)
        #expect(board.swap == nil)
        board.swapTop(displaced: yard, now: 6)
        #expect(board.swap == nil)
        let escaped = board.dismissTop()
        #expect(escaped == nil && board.detachedCameras == [door])
        // Pop In: drawn again on top, its minute starting now.
        let poppedIn = board.setDetached(door, false, now: 50)
        #expect(poppedIn)
        #expect(board.shown.map(\.camera) == [door] && board.item(for: door)?.usedAt == 50)
        #expect(board.deadline == 50 + InsetBoard.autoCloseDelay)
        let again = board.setDetached(door, false, now: 51)
        #expect(!again)
        // The call's end removes it either way.
        let poppedOut = board.setDetached(door, true, now: 60)
        #expect(poppedOut)
        let ended = board.dismiss(door)
        #expect(ended?.camera == door && board.items.isEmpty)
    }

    @Test func poppingOutTheSwappedInCallSwapsBack() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.swap(door, displaced: yard, now: 1)
        #expect(board.swap?.shown == door && board.items.first?.isReturn == true)
        let poppedOut = board.setDetached(door, true, now: 2)
        #expect(poppedOut)
        #expect(board.swap == nil && !board.items.contains(where: \.isReturn))
        #expect(board.isEmpty && board.detachedCameras == [door])
    }

    @Test func detachedItemsDoNotCountTowardMore() {
        var board = InsetBoard()
        let others = (0..<4).map { CameraRef(device: "d-2", channel: $0) }
        for (i, c) in others.enumerated() {
            board.started(.detection("person"), camera: c, visible: false, now: Double(i), at: t0)
        }
        board.started(.doorbell, camera: door, visible: false, now: 9, at: t0, detached: true)
        #expect(board.shown.map(\.camera) == Array(others.reversed().prefix(3)))
        #expect(board.more == 1)
        // Esc takes the top drawn inset, not the call.
        let escaped = board.dismissTop()
        #expect(escaped?.camera == others[3])
        #expect(board.detachedCameras == [door])
    }

    // MARK: Call window placement

    /// Visible frames, top-left origin: a screen with a 25 pt menu bar, and a second screen to its right.
    static let screenA = WindowRect(x: 0, y: 25, width: 1440, height: 875)
    static let screenB = WindowRect(x: 1440, y: 0, width: 1920, height: 1080)
    static let small = WindowRect(x: 0, y: 0, width: 400, height: 300)

    /// (mode, remembered, screens, active, index) -> frame.
    static let placementVectors:
        [(
            mode: CallWindowPlacement.Mode, remembered: WindowRect?, screens: [WindowRect], active: WindowRect?,
            index: Int, frame: WindowRect
        )] = [
            // Where I left it.
            (.remembered, WindowRect(x: 100, y: 100, width: 500, height: 420), [screenA, screenB], screenB, 0,
             WindowRect(x: 100, y: 100, width: 500, height: 420)),
            // Further windows cascade 24 pt right and down.
            (.remembered, WindowRect(x: 100, y: 100, width: 500, height: 420), [screenA, screenB], screenA, 2,
             WindowRect(x: 148, y: 148, width: 500, height: 420)),
            // ... kept inside the remembered frame's screen.
            (.remembered, WindowRect(x: 900, y: 450, width: 500, height: 420), [screenA], screenA, 2,
             WindowRect(x: 940, y: 480, width: 500, height: 420)),
            // The middle of the top edge on the second screen: still there.
            (.remembered, WindowRect(x: 1300, y: 20, width: 480, height: 410), [screenA, screenB], screenA, 0,
             WindowRect(x: 1300, y: 20, width: 480, height: 410)),
            // ... and with that screen gone: the top right of the active screen, the size kept.
            (.remembered, WindowRect(x: 1300, y: 20, width: 480, height: 410), [screenA], nil, 0,
             WindowRect(x: 948, y: 37, width: 480, height: 410)),
            (.remembered, WindowRect(x: 2000, y: 100, width: 500, height: 420), [screenA], screenA, 0,
             WindowRect(x: 928, y: 37, width: 500, height: 420)),
            // The title bar under the menu bar: not reachable, so not used.
            (.remembered, WindowRect(x: 100, y: 10, width: 500, height: 420), [screenA], screenA, 0,
             WindowRect(x: 928, y: 37, width: 500, height: 420)),
            // Top right of the active screen, whatever was remembered.
            (.activeScreen, WindowRect(x: 100, y: 100, width: 500, height: 420), [screenA, screenB], screenB, 0,
             WindowRect(x: 2848, y: 12, width: 500, height: 420)),
            // Nothing remembered: 480 wide; further windows 24 pt left and down.
            (.remembered, nil, [screenA], nil, 0, WindowRect(x: 948, y: 37, width: 480, height: 410)),
            (.activeScreen, nil, [screenA, screenB], screenB, 1, WindowRect(x: 2844, y: 36, width: 480, height: 410)),
            // A screen smaller than the window: the size is limited to it.
            (.activeScreen, nil, [small], small, 0, WindowRect(x: 12, y: 12, width: 376, height: 276)),
            // No screens at all.
            (.remembered, nil, [], nil, 1, WindowRect(x: 24, y: 24, width: 480, height: 410)),
        ]

    @Test func callWindowsOpenWhereLeftOrTopRightOfTheActiveScreen() {
        for (i, v) in Self.placementVectors.enumerated() {
            let frame = CallWindowPlacement.frame(
                mode: v.mode, remembered: v.remembered, screens: v.screens, active: v.active, index: v.index)
            #expect(frame == v.frame, "vector \(i): \(frame)")
        }
        #expect(CallWindowPlacement.Mode.remembered.title == "Where I left it")
        #expect(CallWindowPlacement.Mode.activeScreen.title == "Top right of the active screen")
    }

    @Test func rectanglesFlipBetweenTopLeftAndBottomLeftOrigins() {
        let r = WindowRect(x: 10, y: 20, width: 100, height: 50)
        #expect(r.flipped(height: 900) == WindowRect(x: 10, y: 830, width: 100, height: 50))
        #expect(r.flipped(height: 900).flipped(height: 900) == r)
        #expect(CallWindowPlacement.isOnScreen(WindowRect(x: 1200, y: 25, width: 480, height: 410), screens: [Self.screenA]))
        #expect(!CallWindowPlacement.isOnScreen(WindowRect(x: 1300, y: 25, width: 480, height: 410), screens: [Self.screenA]))
    }

    // MARK: The call window's stream

    /// (drawn width, sub-stream width, asked for main before) -> main.
    static let qualityVectors: [(drawn: Double, sub: Double?, wasMain: Bool, main: Bool)] = [
        (960, 640, false, true),
        (640, 640, false, false),
        (641, 640, false, true),
        (600, 640, true, true),
        (512, 640, true, true),
        (511, 640, true, false),
        (300, nil, false, false),
        (700, nil, false, true),
        (900, 0, false, true),
        (320, 352, false, false),
        (353, 352, false, true),
    ]

    @Test func callWindowsAskForTheMainStreamWhenWiderThanTheSubStream() {
        for (i, v) in Self.qualityVectors.enumerated() {
            #expect(
                CallWindowQuality.wantsMain(drawnWidth: v.drawn, subWidth: v.sub, wasMain: v.wasMain) == v.main,
                "vector \(i)")
        }
    }
}
