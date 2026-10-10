// swift-tools-version: 6.2
import PackageDescription

// vision-lab: a command-line bench for on-device detection on decoded video.
// It decodes an Annex-B H.264/H.265 file with VideoToolbox, samples frames at
// a configurable rate, runs Apple Vision and Core ML detectors on the decoded
// CVPixelBuffers, and reports latency, cost and (given local labels)
// precision and recall. See docs/VISION.md.
//
// Standalone on purpose: it shares no code with apps/macos, so the
// measurements do not move when the app does.
let package = Package(
    name: "VisionLab",
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "vision-lab", targets: ["vision-lab"])
    ],
    targets: [
        .target(name: "VisionLab"),
        .executableTarget(name: "vision-lab", dependencies: ["VisionLab"], path: "Sources/CLI"),
        .testTarget(name: "VisionLabTests", dependencies: ["VisionLab"]),
    ],
    swiftLanguageModes: [.v6]
)
