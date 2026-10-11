import FakeCore
import Foundation
import NVRCore
import Testing

// The Retry button of a tile without a picture (docs/UX.md, "Tiles without a
// picture"): when it is offered, and what the call does. Time is the viewer
// clock, stepped by hand: nothing here waits.

@Suite struct TileRetryTests {
    let camera = CameraRef(device: "dev-a", channel: 1)

    func link(_ state: DeviceState?) -> DeviceLink { DeviceLink(state: state, since: 0) }

    @Test func appliesToMessagesARetryCanHelp() {
        #expect(TileRetry.applies(link: link(nil), status: nil, hasPicture: false))
        #expect(TileRetry.applies(link: link(.connecting), status: nil, hasPicture: false))
        #expect(TileRetry.applies(link: link(.backoff(attempt: 3, retryMilliseconds: 8000)), status: nil, hasPicture: false))
        #expect(TileRetry.applies(link: link(.connected), status: .waiting, hasPicture: false))
        #expect(TileRetry.applies(link: link(.connected), status: nil, hasPicture: false))
        #expect(TileRetry.applies(link: link(.connected), status: .recovering, hasPicture: false))
    }

    @Test func neverAppliesToALoginRefusalOrAnythingElseFinal() {
        let refused = DeviceState.loginFailed(status: 401, remainTimes: 2)
        #expect(!TileRetry.applies(link: link(refused), status: nil, hasPicture: false))
        #expect(!TileRetry.applies(link: link(.closed), status: nil, hasPicture: false))
        #expect(!TileRetry.applies(link: link(.other("x")), status: nil, hasPicture: false))
        #expect(!TileRetry.applies(link: link(.connected), status: .undecodable, hasPicture: false))
        #expect(!TileRetry.applies(link: link(.connected), status: .playing, hasPicture: true))
        // A picture on screen: no veil to put a button under.
        #expect(!TileRetry.applies(link: link(.connected), status: .waiting, hasPicture: true))
    }

    @Test func theButtonAppearsAfterTheDelayOnTheViewerClock() {
        let clock = ManualViewerClock(now: 1000)
        var watch = TileRetry.Watch()
        let offer1 = watch.offered(camera, applies: true, now: clock.now)
        #expect(!offer1)
        clock.advance(by: TileRetry.offerAfter - 0.5)
        let offer2 = watch.offered(camera, applies: true, now: clock.now)
        #expect(!offer2)
        clock.advance(by: 0.5)
        let offer3 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer3)
        clock.advance(by: 600)
        let offer4 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer4)
    }

    @Test func aPictureOrAFinalStateResetsTheDelay() {
        let clock = ManualViewerClock(now: 0)
        var watch = TileRetry.Watch()
        _ = watch.offered(camera, applies: true, now: clock.now)
        clock.advance(by: 40)
        let offer5 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer5)
        // A picture arrives for a moment.
        let offer6 = watch.offered(camera, applies: false, now: clock.now)
        #expect(!offer6)
        clock.advance(by: 1)
        let offer7 = watch.offered(camera, applies: true, now: clock.now)
        #expect(!offer7)
        clock.advance(by: TileRetry.offerAfter)
        let offer8 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer8)
    }

    @Test func retryingStartsTheDelayAgainAndOtherTilesAreIndependent() {
        let other = CameraRef(device: "dev-a", channel: 2)
        let clock = ManualViewerClock(now: 0)
        var watch = TileRetry.Watch()
        _ = watch.offered(camera, applies: true, now: clock.now)
        clock.advance(by: 20)
        _ = watch.offered(other, applies: true, now: clock.now)
        clock.advance(by: 15)
        let offer9 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer9)
        let offer10 = watch.offered(other, applies: true, now: clock.now)
        #expect(!offer10)
        watch.restart(camera, now: clock.now)
        let offer11 = watch.offered(camera, applies: true, now: clock.now)
        #expect(!offer11)
        clock.advance(by: TileRetry.offerAfter)
        let offer12 = watch.offered(camera, applies: true, now: clock.now)
        #expect(offer12)
        let offer13 = watch.offered(other, applies: true, now: clock.now)
        #expect(offer13)
        watch.keep([camera])
        let offer14 = watch.offered(other, applies: true, now: clock.now)
        #expect(!offer14)
    }

    @Test func theBridgeRetriesThroughTheCoreAndRefusesUnknownTiles() throws {
        let created = Box<FakeCore?>(nil)
        var options = FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")])
        options.pacing = .manual
        let bridge = try CoreBridge(factory: FakeCore.factory(options: options) { created.set($0) })
        defer { bridge.shutdown() }
        let fake = created.get()!
        let device = try bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "x")
        let tile = try bridge.openTile(device: device, channel: 0, tier: .sub)
        try bridge.retryTile(tile)
        #expect(fake.retriedTiles == [tile.id])
        bridge.closeTile(tile)
        #expect(throws: CoreStatus.unknownID) { try bridge.retryTile(tile) }
        #expect(fake.violations.isEmpty, "\(fake.violations)")
    }
}
