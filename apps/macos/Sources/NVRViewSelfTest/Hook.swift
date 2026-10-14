import AppKit
@testable import NVRViewApp

/// What the self-test build of the app hands to `runApp(selfTest:)`.
@MainActor
public func selfTestHook() -> SelfTestHook {
    SelfTestHook(
        forbiddenLog: SelfTest.forbiddenLog,
        start: { app, areas, seconds in
            SelfTest(app: app as! AppDelegate, areas: areas).start(seconds: seconds)
        })
}
