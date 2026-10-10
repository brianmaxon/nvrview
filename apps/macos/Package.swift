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
        .executableTarget(name: "NVRViewDemo", dependencies: ["VideoKit"]),
        .testTarget(name: "VideoKitTests", dependencies: ["VideoKit"]),
    ],
    swiftLanguageModes: [.v6]
)
