// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "NVRView",
    platforms: [.macOS(.v26)],
    products: [
        .library(name: "VideoKit", targets: ["VideoKit"]),
        .executable(name: "NVRViewDemo", targets: ["NVRViewDemo"]),
    ],
    targets: [
        .target(name: "VideoKit"),
        // Temporary: drives VideoKit until the Rust video pipeline does (M2).
        // Tests and the demo only; never a dependency of the app.
        .target(name: "PipelineStandIn", dependencies: ["VideoKit"]),
        .executableTarget(name: "NVRViewDemo", dependencies: ["VideoKit", "PipelineStandIn"]),
        .testTarget(name: "VideoKitTests", dependencies: ["VideoKit", "PipelineStandIn"]),
    ],
    swiftLanguageModes: [.v6]
)
