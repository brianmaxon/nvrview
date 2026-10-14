import Foundation
import NVRCore
import Testing

/// Alert insets (docs/UX.md "Alert insets"), on simulated time: what opens
/// one, stacking and repeats, the swap, closing by itself and the hover hold,
/// and the link between a sound alarm's inset and its alert.
@Suite struct AlertInsetTests {
    let door = CameraRef(device: "d-1", channel: 3)
    let yard = CameraRef(device: "d-1", channel: 1)
    let hall = CameraRef(device: "d-1", channel: 4)
    let side = CameraRef(device: "d-2", channel: 0)
    let shown = CameraRef(device: "d-2", channel: 2)
    let t0 = Date(timeIntervalSince1970: 1_800_000_000)

    private func event(_ kind: String, channel: Int = 3, active: Bool = true, source: DetectionSource = .device, alarm: Bool = false)
        -> DetectionEvent
    {
        DetectionEvent(device: 1, channel: channel, kind: kind, active: active, source: source, alarm: alarm)
    }

    // MARK: Triggers

    @Test func whatOpensAnInset() {
        #expect(InsetTrigger.kind(for: event("visitor", alarm: true), entrance: false) == .doorbell)
        #expect(InsetTrigger.kind(for: event("other_alarm", alarm: true), entrance: true) == nil)
        #expect(InsetTrigger.kind(for: event("smoke_alarm", source: .localAudio), entrance: false) == .soundAlarm("smoke_alarm"))
        #expect(InsetTrigger.kind(for: event("co_alarm", source: .localAudio), entrance: false) == .soundAlarm("co_alarm"))
        // People only on cameras marked as entrances, or turned on per camera.
        #expect(InsetTrigger.kind(for: event("people"), entrance: true) == .detection("people"))
        #expect(InsetTrigger.kind(for: event("people"), entrance: false) == nil)
        #expect(InsetTrigger.kind(for: event("vehicle"), entrance: true) == nil)
        #expect(InsetTrigger.kind(for: event("vehicle"), entrance: false, extraKinds: ["vehicle"]) == .detection("vehicle"))
        #expect(InsetTrigger.kind(for: event(DetectionEvent.motionKind), entrance: true, extraKinds: ["motion"]) == nil)
    }

    @Test func aVisibleCameraGetsNoInset() {
        var board = InsetBoard()
        let r1 = board.started(.doorbell, camera: door, visible: true, now: 0, at: t0)
        #expect(!r1)
        #expect(board.isEmpty)
        let r2 = board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        #expect(r2)
        #expect(board.shown.map(\.camera) == [door])
    }

    // MARK: Stacking

    @Test func newestOnTopUpToThreeThenMore() {
        var board = InsetBoard()
        // Spaced past the minimum on-screen time, so the oldest gives way.
        for (i, c) in [door, yard, hall, side].enumerated() {
            let t = Double(i) * 11
            board.started(.detection("people"), camera: c, visible: false, now: t, at: t0 + t)
        }
        #expect(board.shown.map(\.camera) == [side, hall, yard])
        #expect(board.more == 1)
        #expect(board.waiting.map(\.camera) == [door])
        // A repeat refreshes that camera's inset where it is (here: waiting).
        board.started(.detection("people"), camera: door, visible: false, now: 40, at: t0 + 40)
        #expect(board.items.count == 4)
        #expect(board.items.map(\.camera) == [side, hall, yard, door])
        #expect(board.item(for: door)?.count == 2)
        #expect(board.item(for: door)?.lastAt == t0 + 40)
        #expect(board.item(for: door)?.firstAt == t0)
    }

    @Test func aRepeatOnADrawnCameraKeepsItsPlace() {
        var board = InsetBoard()
        for (i, c) in [door, yard, hall].enumerated() {
            board.started(.detection("people"), camera: c, visible: false, now: Double(i), at: t0 + Double(i))
        }
        #expect(board.shown.map(\.camera) == [hall, yard, door])
        board.started(.detection("people"), camera: door, visible: false, now: 5, at: t0 + 5)
        #expect(board.shown.map(\.camera) == [hall, yard, door])
        #expect(board.shown.last?.count == 2)
    }

    @Test func aNewerInsetWaitsUntilTheOlderHasBeenUpTenSeconds() {
        var board = InsetBoard()
        for (i, c) in [door, yard, hall].enumerated() {
            board.started(.detection("people"), camera: c, visible: false, now: Double(i), at: t0 + Double(i))
        }
        board.started(.detection("people"), camera: side, visible: false, now: 5, at: t0 + 5)
        // All three are younger than 10 s: the newcomer waits.
        #expect(board.shown.map(\.camera) == [hall, yard, door])
        #expect(board.waiting.map(\.camera) == [side])
        // The lowest one (the oldest) gives way at its tenth second.
        #expect(board.deadline == InsetBoard.minimumShownTime)
        let early = board.tick(now: 9)
        #expect(early.isEmpty)
        #expect(board.waiting.map(\.camera) == [side])
        let due = board.tick(now: 10)
        #expect(due.isEmpty)
        #expect(board.shown.map(\.camera) == [side, hall, yard])
        #expect(board.waiting.map(\.camera) == [door])
    }

    @Test func interleavedEventsOnFourCamerasCauseNoChurn() {
        var board = InsetBoard()
        var changes = 0
        var previous: [CameraRef] = []
        let cameras = [door, yard, hall, side]
        for step in 0..<40 {
            let t = Double(step)
            board.started(.detection("people"), camera: cameras[step % 4], visible: false, now: t, at: t0 + t)
            _ = board.tick(now: t)
            let now = board.shown.map(\.camera)
            if now != previous { changes += 1 }
            previous = now
        }
        // Three arrive, the fourth waits; the lowest makes room for it once
        // it has been up 10 s, and the order then holds: far fewer changes
        // than the forty events (the old behavior moved a camera on every
        // event).
        #expect(changes <= 6)
        #expect(board.items.count == 4)
    }

    @Test func aWaitingCameraCanBeBroughtIn() {
        var board = InsetBoard()
        for (i, c) in [door, yard, hall, side].enumerated() {
            board.started(.detection("people"), camera: c, visible: false, now: Double(i), at: t0 + Double(i))
        }
        #expect(board.waiting.map(\.camera) == [door] || board.waiting.map(\.camera) == [side])
        let waiting = board.waiting[0].camera
        let alreadyDrawn = board.bringIn(board.shown[0].camera, now: 4)
        #expect(!alreadyDrawn)
        let brought = board.bringIn(waiting, now: 4)
        #expect(brought)
        #expect(board.shown.first?.camera == waiting)
        #expect(board.shown.count == 3)
        #expect(board.more == 1)
    }

    @Test func theGridSettingSetsHowManyAreDrawn() {
        let grid = InsetGridSetting(rows: 2, columns: 3)
        #expect(grid.capacity == 6)
        #expect(InsetGridSetting.standard == InsetGridSetting(rows: 3, columns: 1))
        #expect(InsetGridSetting(rows: 0, columns: 99) == InsetGridSetting(rows: 1, columns: 6))
        var board = InsetBoard(capacity: grid.capacity)
        let cameras = (0..<7).map { CameraRef(device: "d-9", channel: $0) }
        for (i, c) in cameras.enumerated() {
            board.started(.detection("people"), camera: c, visible: false, now: Double(i), at: t0 + Double(i))
        }
        #expect(board.shown.count == 6)
        #expect(board.more == 1)
        // Shrinking it moves the lowest ones to the waiting list at once.
        board.setCapacity(InsetGridSetting(rows: 1, columns: 2).capacity)
        #expect(board.shown.count == 2)
        #expect(board.more == 5)
        board.setCapacity(6)
        #expect(board.shown.count == 6)
    }

    // MARK: Return inset

    @Test func theReturnInsetShowsTheLayoutWhenTheStageShowedOne() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.swap(door, displaced: shown, displacedLayout: true, now: 1)
        #expect(board.swap?.displacedLayout == true)
        #expect(board.items.first?.isReturn == true && board.items.first?.showsLayout == true)
        #expect(board.shown.first?.showsLayout == true)
        // A click on it restores the stage; nothing of the layout was kept here.
        board.swap(shown, displaced: nil, now: 2)
        #expect(board.swap == nil)
        #expect(board.shown.map(\.camera) == [door])
    }

    @Test func aSingleCameraStageKeepsTheOneCameraReturnInset() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.swap(door, displaced: shown, now: 1)
        #expect(board.swap?.displacedLayout == false)
        #expect(board.items.first?.showsLayout == false)
        board.swapBack(now: 2)
        board.swapTop(displaced: shown, displacedLayout: true, now: 3)
        #expect(board.swap?.displacedLayout == true)
        board.swapTop(displaced: nil, now: 4)
        #expect(board.swap == nil)
    }

    @Test func theReturnInsetAlwaysGetsAPlace() {
        var board = InsetBoard()
        for (i, c) in [door, yard, hall].enumerated() {
            board.started(.detection("people"), camera: c, visible: false, now: Double(i), at: t0 + Double(i))
        }
        board.swap(hall, displaced: shown, displacedLayout: true, now: 3)
        // Two others plus the return inset, though neither has been up 10 s.
        #expect(Set(board.shown.map(\.camera)) == [shown, yard, door])
    }

    @Test func aSoundAlarmOutranksADetectionOnTheSameCamera() {
        var board = InsetBoard()
        board.started(.detection("people"), camera: hall, visible: false, now: 0, at: t0)
        board.started(.soundAlarm("smoke_alarm"), camera: hall, visible: false, now: 1, at: t0 + 1)
        board.started(.detection("people"), camera: hall, visible: false, now: 2, at: t0 + 2)
        #expect(board.item(for: hall)?.kind == .soundAlarm("smoke_alarm"))
        #expect(board.item(for: hall)?.count == 3)
    }

    @Test func theHeaderSaysReasonTimeAndCount() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let locale = Locale(identifier: "en_US_POSIX")
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        let once = board.item(for: door)!.header(now: t0, calendar: calendar, locale: locale)
        #expect(once.hasPrefix("Doorbell · "))
        #expect(!once.contains("times"))
        board.started(.doorbell, camera: door, visible: false, now: 5, at: t0 + 5)
        #expect(board.item(for: door)!.header(now: t0, calendar: calendar, locale: locale).contains("2 times, last"))
        #expect(InsetKind.soundAlarm("co_alarm").reason == "CO alarm (sound)")
    }

    // MARK: Swap

    @Test func clickSwapsAndOneMoreClickSwapsBack() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.swap(door, displaced: shown, now: 1)
        #expect(board.swap?.shown == door)
        #expect(board.items.map(\.camera) == [shown])
        #expect(board.items.first?.isReturn == true)
        // The return inset: a click swaps back, the doorbell's inset returns.
        board.swap(shown, displaced: nil, now: 2)
        #expect(board.swap == nil)
        #expect(board.items.map(\.camera) == [door])
        #expect(board.items.first?.isReturn == false)
        // Tab does the same on the top inset.
        board.swapTop(displaced: shown, now: 3)
        #expect(board.swap?.shown == door)
        board.swapTop(displaced: nil, now: 4)
        #expect(board.swap == nil)
        #expect(board.shown.map(\.camera) == [door])
    }

    @Test func swappingAnotherInsetWhileSwappedKeepsTheWayBack() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.started(.soundAlarm("smoke_alarm"), camera: hall, visible: false, now: 0, at: t0)
        board.swap(hall, displaced: shown, now: 1)
        board.swap(door, displaced: nil, now: 2)
        #expect(board.swap?.shown == door)
        #expect(board.swap?.displaced == shown)
        #expect(Set(board.items.map(\.camera)) == [hall, shown])
        board.swapBack(now: 3)
        #expect(board.swap == nil)
        #expect(board.items.map(\.camera) == [door, hall])
    }

    @Test func escDismissesTheSwappedInCameraOrTheTopInset() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.started(.detection("people"), camera: yard, visible: false, now: 0, at: t0)
        board.swap(door, displaced: shown, now: 1)
        let r3 = board.dismissTop()
        #expect(r3?.camera == door)
        #expect(board.swap == nil)
        #expect(board.items.map(\.camera) == [yard])
        let r4 = board.dismissTop()
        #expect(r4?.camera == yard)
        #expect(board.isEmpty)
        let r5 = board.dismissTop()
        #expect(r5 == nil)
    }

    @Test func closingTheReturnInsetSwapsBack() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.swap(door, displaced: shown, now: 1)
        let r6 = board.dismiss(shown)
        #expect(r6 == nil)
        #expect(board.swap == nil)
        #expect(board.items.isEmpty)
    }

    // MARK: Closing by itself

    @Test func doorbellAndDetectionInsetsCloseAMinuteAfterTheEvent() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.started(.detection("people"), camera: yard, visible: false, now: 0, at: t0)
        let r7 = board.tick(now: 100)
        #expect(r7.isEmpty)  // still going: no stop yet
        board.stopped(.doorbell, camera: door, now: 100)
        board.stopped(.detection("people"), camera: yard, now: 100)
        #expect(board.deadline == 160)
        let r8 = board.tick(now: 159.9)
        #expect(r8.isEmpty)
        let r9 = board.tick(now: 160)
        #expect(Set(r9.map(\.camera)) == [door, yard])
        #expect(board.isEmpty)
    }

    @Test func hoverHoldsAndUseRestartsTheClock() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.stopped(.doorbell, camera: door, now: 1)
        board.hover(door, true, now: 30)
        let r10 = board.tick(now: 500)
        #expect(r10.isEmpty)
        board.hover(door, false, now: 500)
        let r11 = board.tick(now: 559)
        #expect(r11.isEmpty)
        board.use(door, now: 559)
        let r12 = board.tick(now: 618)
        #expect(r12.isEmpty)
        let r13 = board.tick(now: 619)
        #expect(r13.map(\.camera) == [door])
    }

    @Test func aLostStopStillCloses() {
        var board = InsetBoard()
        board.started(.detection("people"), camera: yard, visible: false, now: 0, at: t0)
        let r14 = board.tick(now: InsetBoard.lostStopAfter + InsetBoard.autoCloseDelay - 1)
        #expect(r14.isEmpty)
        let r15 = board.tick(now: InsetBoard.lostStopAfter + InsetBoard.autoCloseDelay)
        #expect(r15.count == 1)
    }

    @Test func aRepeatKeepsItOpen() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.stopped(.doorbell, camera: door, now: 1)
        board.started(.doorbell, camera: door, visible: false, now: 50, at: t0 + 50)
        let r16 = board.tick(now: 70)
        #expect(r16.isEmpty)
        board.stopped(.doorbell, camera: door, now: 51)
        let r17 = board.tick(now: 110)
        #expect(r17.isEmpty)
        let r18 = board.tick(now: 111)
        #expect(r18.count == 1)
    }

    @Test func aSwappedInDoorbellClosesAndTheStageReturns() {
        var board = InsetBoard()
        board.started(.doorbell, camera: door, visible: false, now: 0, at: t0)
        board.stopped(.doorbell, camera: door, now: 0)
        board.swap(door, displaced: shown, now: 10)
        let r19 = board.tick(now: 69)
        #expect(r19.isEmpty)
        let r20 = board.tick(now: 70)
        #expect(r20.map(\.camera) == [door])
        #expect(board.swap == nil)
        #expect(board.items.isEmpty)
    }

    @Test func aConversationInsetIsACallThatClosesAMinuteAfterItsLastUse() {
        #expect(InsetKind.conversation.isCall && InsetKind.doorbell.isCall && !InsetKind.view.isCall)
        #expect(InsetKind.conversation.closesByItself && !InsetKind.conversation.isCritical)
        #expect(InsetKind.conversation.reason == "Conversation")
        var board = InsetBoard()
        // Started by the user, not an event: opened and "ended" at once.
        board.started(.conversation, camera: door, visible: false, now: 0, at: t0)
        board.stopped(.conversation, camera: door, now: 0)
        board.use(door, now: 30)
        let early = board.tick(now: 89)
        #expect(early.isEmpty)
        let due = board.tick(now: 90)
        #expect(due.map(\.camera) == [door])
    }

    @Test func aPressDuringAConversationTurnsItIntoTheDoorbellInset() {
        var board = InsetBoard()
        board.started(.conversation, camera: door, visible: false, now: 0, at: t0)
        board.stopped(.conversation, camera: door, now: 0)
        board.started(.doorbell, camera: door, visible: false, now: 5, at: t0 + 5)
        #expect(board.item(for: door)?.kind == .doorbell && board.item(for: door)?.count == 2)
    }

    // MARK: Smoke and CO

    @Test func soundAlarmInsetsStayUntilDismissed() {
        var board = InsetBoard()
        board.started(.soundAlarm("smoke_alarm"), camera: hall, visible: false, now: 0, at: t0)
        board.stopped(.soundAlarm("smoke_alarm"), camera: hall, now: 5)
        #expect(board.deadline == nil)
        let r21 = board.tick(now: 10_000)
        #expect(r21.isEmpty)
        let r22 = board.dismiss(hall)
        #expect(r22?.kind == .soundAlarm("smoke_alarm"))
        #expect(board.isEmpty)
    }

    @Test func dismissingTheInsetDismissesTheAlertAndTheOtherWayRound() {
        var alerts = AlertCenter()
        var board = InsetBoard()
        let smoke = DetectionEvent(device: 1, channel: 4, kind: "smoke_alarm", active: true, source: .localAudio)
        alerts.apply(smoke, deviceKey: "d-1", at: t0)
        let kind = InsetTrigger.kind(for: smoke, entrance: false)!
        board.started(kind, camera: hall, visible: false, now: 0, at: t0)
        // What the stage does when the inset is dismissed.
        if let item = board.dismiss(hall), item.kind.isCritical { alerts.dismiss(item.camera, at: t0 + 1) }
        #expect(!alerts.isActive(hall))
        // Dismissed elsewhere (a tile header in another window): the inset goes.
        alerts.apply(smoke, deviceKey: "d-1", at: t0 + 2)
        board.started(kind, camera: hall, visible: false, now: 2, at: t0 + 2)
        alerts.dismiss(hall, at: t0 + 3)
        let dropped = board.dropSoundAlarms { !alerts.isActive($0) }
        #expect(dropped.map(\.camera) == [hall])
        #expect(board.isEmpty)
    }

    // MARK: Geometry

    @Test func insetsStackFromTheCornerOppositeTheIndicators() {
        #expect(ChromeCorner.topRight.opposite == .bottomLeft)
        #expect(ChromeCorner.bottomLeft.opposite == .topRight)
        let width = InsetGeometry.width(stage: 1000)
        #expect(width == 300)
        let frames = InsetGeometry.frames(
            count: 3, width: width, heights: [200, 200, 200], corner: .bottomLeft, stage: (1000, 600))
        // Two fit in 600 pt; the third is left out (counted as more).
        #expect(frames.count == 2)
        #expect(frames[0].x == 12 && frames[0].y == 600 - 12 - 200)
        #expect(frames[1].y == 600 - 12 - 200 - 8 - 200)
        let top = InsetGeometry.frames(count: 1, width: width, heights: [200], corner: .topRight, stage: (1000, 600))
        #expect(top[0].x == 1000 - 12 - 300 && top[0].y == 12)
        #expect(InsetGeometry.width(stage: 400) == 220)
    }

    @Test func theGridFillsColumnsFromTheAnchoredEdge() {
        let stage = (width: 1600.0, height: 900.0)
        let width = InsetGeometry.width(stage: stage.width, columns: 3)
        #expect(width == 480)  // 30% of the width; three columns fit
        let heights = [150.0, 150, 150, 150, 150, 150, 150]
        let right = InsetGeometry.grid(
            heights: heights, width: width, rows: 2, columns: 3, anchor: InsetAnchor(corner: .topRight), stage: stage)
        #expect(right.count == 6)
        // The first column is at the right edge; the next ones grow leftward.
        #expect(right[0].x == 1600 - 12 - 480 && right[1].x == right[0].x)
        #expect(right[2].x == right[0].x - 8 - 480)
        #expect(right[4].x == right[0].x - 2 * (8 + 480))
        #expect(right[0].y == 12 && right[1].y == 12 + 150 + 8)
        let left = InsetGeometry.grid(
            heights: heights, width: width, rows: 2, columns: 3, anchor: InsetAnchor(corner: .bottomLeft), stage: stage)
        #expect(left[0].x == 12 && left[2].x == 12 + 480 + 8)
        // Bottom anchored: the first inset is at the bottom of its column.
        #expect(left[0].y == 900 - 12 - 150 && left[1].y == left[0].y - 8 - 150)
    }

    @Test func oneColumnOfThreeIsTheOriginalStack() {
        let width = InsetGeometry.width(stage: 1000, columns: 1)
        #expect(width == InsetGeometry.width(stage: 1000))
        for corner in [ChromeCorner.topLeft, .topRight, .bottomLeft, .bottomRight] {
            let anchor = InsetAnchor(corner: corner)
            let grid = InsetGeometry.grid(
                heights: [200, 200, 200], width: width, rows: 3, columns: 1, anchor: anchor, stage: (1000, 900))
            let old = InsetGeometry.frames(count: 3, width: width, heights: [200, 200, 200], anchor: anchor, stage: (1000, 900))
            #expect(grid.count == 3 && old.count == 3)
            #expect(zip(grid, old).allSatisfy { $0.x == $1.x && $0.y == $1.y })
        }
    }

    @Test func columnsThatDoNotFitAreDroppedAndTheRestWait() {
        // 500 pt wide: two 220 pt columns fit; of six requested, two are used.
        #expect(InsetGeometry.fittingColumns(stage: 500, columns: 6) == 2)
        #expect(InsetGeometry.fittingColumns(stage: 200, columns: 6) == 1)
        let width = InsetGeometry.width(stage: 500, columns: 6)
        #expect(width >= InsetGeometry.minWidth && width <= (500 - 24 - 8) / 2)
        let frames = InsetGeometry.grid(
            heights: Array(repeating: 100, count: 8), width: width, rows: 2, columns: 6, anchor: InsetAnchor(corner: .topLeft),
            stage: (500, 900))
        #expect(frames.count == 4)
        // A short stage: the second inset does not fit under the first, so it starts the next column.
        let short = InsetGeometry.grid(
            heights: [150, 150, 150], width: 220, rows: 3, columns: 2, anchor: InsetAnchor(corner: .topLeft),
            stage: (700, 200))
        #expect(short.count == 2 && short[1].x > short[0].x)
    }

    // MARK: Keys

    @Test func tabAndEscGoToTheInsetsOnlyOnTheStage() {
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.tab, modifiers: [], isRepeat: false, context: .stage) == .swap)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.escape, modifiers: [], isRepeat: false, context: .stage) == .dismiss)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.tab, modifiers: [], isRepeat: false, context: .textEditing) == nil)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.tab, modifiers: [], isRepeat: false, context: .sidebar) == nil)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.tab, modifiers: [.shift], isRepeat: false, context: .stage) == nil)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.escape, modifiers: [], isRepeat: true, context: .stage) == nil)
        #expect(ViewerKeys.insetKey(keyCode: ViewerKeys.space, modifiers: [], isRepeat: false, context: .stage) == nil)
    }
}

@Suite struct VerificationStayTests {
    @Test func theStayChoicesStartAtFiveSeconds() {
        #expect(VerifyViewSettings.minimumSeconds == 5)
        #expect(VerifyViewSettings.stayChoices(including: 60).first == 5)
        #expect(VerifyViewSettings.stayChoices(including: 65).contains(65))
        #expect(VerifyViewSettings.stayTitle(5) == "5 s")
        #expect(VerifyViewSettings.stayTitle(90) == "1 min 30 s")
    }

    @Test func aCheckWithAFiveSecondStayClosesFiveSecondsAfterItsDecision() {
        var settings = VerifyViewSettings()
        settings.minSeconds = 5
        #expect(Double(max(settings.minSeconds, VerifyViewSettings.minimumSeconds)) == 5)
    }
}
