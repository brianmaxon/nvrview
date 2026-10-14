// swift-tools-version: 6.2
import Foundation
import PackageDescription

// The C interface (docs/FFI.md) reaches Swift as the module `NVRViewCore`.
//
// By default that module is a header-only C target holding an exact copy of
// docs/nvrview.h (a test keeps the copy identical), so everything compiles
// and tests run without the Rust library. Only `NVRCoreC` (the real
// implementation of `CoreAPI`) calls the C functions, and nothing links it
// unless the real core is enabled.
//
// To plug in the real library, drop NVRViewCore.xcframework (the Rust static
// library, its header and a module map naming the module `NVRViewCore`) next
// to this file and build with NVRVIEW_REAL_CORE=1 in the environment. That
// swaps the C target for `.binaryTarget(name: "NVRViewCore", path:
// "NVRViewCore.xcframework")`, links `NVRCoreC` into the app, and defines
// `NVRVIEW_REAL_CORE` for it. No Swift code changes: every file already says
// `import NVRViewCore`.
let realCore = ProcessInfo.processInfo.environment["NVRVIEW_REAL_CORE"] != nil

let cInterface: Target =
    realCore
    ? .binaryTarget(name: "NVRViewCore", path: "NVRViewCore.xcframework")
    : .target(name: "NVRViewCore")

// The app is the library NVRViewApp plus a tiny executable. The self-test
// (NVRViewSelfTest) is a separate module, linked only when
// NVRVIEW_SELFTEST=1: the shipped app has none, and the self-test build
// shares NVRViewApp's compiled module with it (same scratch path), so only
// the self-test module and the executable's link differ.
let selfTest = ProcessInfo.processInfo.environment["NVRVIEW_SELFTEST"] != nil

// The app's feature modules (Sources/<Module>), each depending only on the
// ones below it: AppBase first, then the features, then NVRViewApp on top.
// They share one access rule: `package` for what another module uses.
let appModules: [(name: String, dependencies: [Target.Dependency])] = [
    ("AppBase", ["NVRCore", "VideoKit"]),
    // Updates: checking, downloading, release notes, and their settings page.
    ("AppUpdates", ["AppBase"]),
    // Doorbell calls: the call sessions, microphone, speech, and transcripts.
    ("AppCalls", ["AppBase"]),
    // Capture: pictures and live recordings, and their settings page.
    ("AppCapture", ["AppBase"]),
    // Camera motion: the pan, tilt, zoom, lens, and preset model and controls.
    ("AppPTZ", ["AppBase"]),
    // Smart alerts on this computer: settings, the events window, evidence
    // and verification views, and the model behind them.
    // Recordings: browsing, playback, editing, and export.
    ("AppRecordings", ["AppBase", "AppCapture"]),
    ("AppAlerts", ["AppBase", "AppCapture", "AlertHost", "DetectorKit"]),
    // The stage: the tile grid, headers, insets, and the call window.
    ("AppStage", ["AppBase", "AppCalls", "AppCapture", "AppPTZ", "AppAlerts"]),
]

let appDependencies: [Target.Dependency] =
    ["VideoKit", "NVRCore", "FakeCore", "DetectorKit", "AlertHost"] + appModules.map { .target(name: $0.name) }
    + (realCore ? ["NVRCoreC"] : [])
let coreDefine: [SwiftSetting] = realCore ? [.define("NVRVIEW_REAL_CORE")] : []
// `-enable-testing` in release lets the self-test module use internals
// (`@testable import`) without every declaration being package or public.
let appSwiftSettings: [SwiftSetting] =
    coreDefine + [.unsafeFlags(["-enable-testing"], .when(configuration: .release))]

let package = Package(
    name: "NVRView",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "VideoKit", targets: ["VideoKit"]),
        .executable(name: "NVRView", targets: ["NVRView"]),
        .executable(name: "NVRViewDemo", targets: ["NVRViewDemo"]),
    ],
    targets: [
        cInterface,
        .target(name: "VideoKit"),
        // The on-device object detector (RT-DETR on Core ML): crops of a
        // decoded picture in, raw boxes out. Knows nothing of alerts or the core.
        .target(name: "DetectorKit"),
        // Smart alerts on this computer: answers the core's verification
        // requests with the detector, makes snapshots, fetches the model on
        // first use, builds the settings document with the Keychain's
        // credentials, and raises notifications. Calls no C function.
        .target(name: "AlertHost", dependencies: ["NVRCore", "VideoKit", "DetectorKit"]),
        // The bridge between the C core's actions and events and VideoKit.
        // Calls no C function, so it links without the library.
        .target(name: "NVRCore", dependencies: ["VideoKit", "NVRViewCore"]),
        // CCore: the real CoreAPI, calling the C functions. Needs the library
        // at link time; nothing depends on it unless NVRVIEW_REAL_CORE is set.
        .target(name: "NVRCoreC", dependencies: ["NVRCore", "NVRViewCore"]),
        // Development and test double of the core, plus the temporary stand-in
        // pipeline it uses. Never a dependency of the shipping app path; the
        // app links it only for `--fake`.
        .target(name: "FakeCore", dependencies: ["NVRCore", "VideoKit"]),
        // The composition layer: the app model, windows and views that wire
        // the feature modules together.
        .target(name: "NVRViewApp", dependencies: appDependencies, swiftSettings: appSwiftSettings),
        // The app's self-test (`--selftest`) and its mocks.
        .target(
            name: "NVRViewSelfTest", dependencies: appDependencies + ["NVRViewApp"],
            swiftSettings: coreDefine),
        .executableTarget(
            name: "NVRView",
            dependencies: ["NVRViewApp"] + (selfTest ? ["NVRViewSelfTest"] : []),
            swiftSettings: selfTest ? [.define("NVRVIEW_SELFTEST")] : []),
        .executableTarget(name: "NVRViewDemo", dependencies: ["VideoKit", "NVRCore", "FakeCore"]),
        .testTarget(name: "DetectorKitTests", dependencies: ["DetectorKit"]),
        .testTarget(name: "AlertHostTests", dependencies: ["AlertHost", "NVRCore", "DetectorKit", "VideoKit"]),
        .testTarget(name: "VideoKitTests",dependencies: ["VideoKit", "FakeCore"]),
        // With the real core, the tests also run it end to end (against a
        // spawned mock-nvr; see RealCoreAudioTests).
        .testTarget(
            name: "NVRCoreTests",
            dependencies: ["NVRCore", "FakeCore", "VideoKit", "NVRViewCore"] + (realCore ? ["NVRCoreC"] : []),
            swiftSettings: realCore ? [.define("NVRVIEW_REAL_CORE")] : []),
    ] + appModules.map {
        .target(name: $0.name, dependencies: $0.dependencies, swiftSettings: appSwiftSettings)
    },
    swiftLanguageModes: [.v6]
)
