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

let appDependencies: [Target.Dependency] =
    ["VideoKit", "NVRCore", "FakeCore"] + (realCore ? ["NVRCoreC"] : [])

let package = Package(
    name: "NVRView",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "VideoKit", targets: ["VideoKit"]),
        .executable(name: "NVRViewDemo", targets: ["NVRViewDemo"]),
    ],
    targets: [
        cInterface,
        .target(name: "VideoKit"),
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
        .executableTarget(name: "NVRViewDemo", dependencies: ["VideoKit", "NVRCore", "FakeCore"]),
        .testTarget(name: "VideoKitTests", dependencies: ["VideoKit", "FakeCore"]),
        .testTarget(name: "NVRCoreTests", dependencies: ["NVRCore", "FakeCore", "VideoKit", "NVRViewCore"]),
    ],
    swiftLanguageModes: [.v6]
)
