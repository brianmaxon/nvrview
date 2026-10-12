import Foundation
import Testing

@testable import NVRCore

@Suite("Camera motion")
struct PTZTests {
    @Test func infoParses() {
        let json = #"{"pan_tilt":true,"zoom":{"min":0,"max":28,"cur":3},"focus":null,"presets":[{"id":0,"name":"Door"}],"speeds":[1,64]}"#
        let info = PTZInfo.parse(json)
        #expect(info?.panTilt == true)
        #expect(info?.zoom == PTZRange(min: 0, max: 28, cur: 3))
        #expect(info?.focus == nil)
        #expect(info?.presets == [PTZPreset(id: 0, name: "Door")])
        #expect(info?.maxSpeed == 64)
    }

    @Test func heldMoveRenewsWellInsideTheCoresLimit() {
        var hold = PTZHold()
        #expect(hold.press(.left, speed: 20, now: 0) == .move(.left, speed: 20))
        #expect(hold.tick(now: 0.1) == nil)
        #expect(hold.tick(now: 0.25) == .move(.left, speed: 20))
        #expect(hold.tick(now: 0.4) == nil)
        #expect(PTZHold.renewInterval < 0.5)
        #expect(hold.release() == .stop)
        #expect(hold.release() == nil)
        #expect(hold.tick(now: 5) == nil)
    }

    @Test func lensStaysInRange() {
        let r = PTZRange(min: 0, max: 28, cur: 27)
        #expect(r.stepped(from: 27, by: 1) == 28)
        #expect(r.stepped(from: 1, by: -1) == 0)
        #expect(r.clamped(99) == 28)
        #expect(r.position(at: 2) == 28)
    }

    @Test func lensDragSendsFivePerSecondAndTheLast() {
        var t = PTZLensThrottle()
        #expect(t.moved(to: 1, now: 0) == 1)
        #expect(t.moved(to: 2, now: 0.05) == nil)
        #expect(t.moved(to: 3, now: 0.1) == nil)
        #expect(t.tick(now: 0.2) == 3)
        #expect(t.moved(to: 4, now: 0.25) == nil)
        #expect(t.end() == 4)
        #expect(t.end() == nil)
    }

    @Test func optionKeysReadByPosition() {
        func key(_ code: UInt16, _ mods: KeyModifiers = [.option], rep: Bool = false, _ ctx: KeyContext = .stage) -> PTZKey? {
            ViewerKeys.ptzKey(keyCode: code, modifiers: mods, isRepeat: rep, context: ctx)
        }
        #expect(key(ViewerKeys.leftArrow) == .move(.left))
        #expect(key(ViewerKeys.downArrow, rep: true) == .move(.down))
        #expect(key(24) == .zoom(1))
        #expect(key(27) == .zoom(-1))
        #expect(key(18) == .preset(1))
        #expect(key(25) == .preset(9))
        #expect(key(25, rep: true) == nil)
        #expect(key(ViewerKeys.leftArrow, []) == nil)
        #expect(key(ViewerKeys.leftArrow, [.option, .shift]) == nil)
        #expect(key(ViewerKeys.leftArrow, [.option], .textEditing) == nil)
    }

    @Test func eventsExplainRefusals() {
        let e = PTZEvent(tile: 1, action: "move", state: .refused, reason: "rate_limited")
        #expect(e.problem != nil)
        #expect(PTZEvent(tile: 1, action: "move", state: .sent).problem == nil)
        let parsed = CoreEvent.parse(Data(#"{"type":"ptz","tile":7,"action":"lens","state":"sent","position":28}"#.utf8))
        #expect(parsed == .ptz(PTZEvent(tile: 7, action: "lens", state: .sent, position: 28)))
    }
}
