import Foundation
import VideoKit

enum Fixtures {
    /// `fixtures/synthetic/` at the repository root, located relative to this file.
    static let directory: URL = {
        var url = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { url.deleteLastPathComponent() }  // file, VideoKitTests, Tests, macos, apps
        return url.appendingPathComponent("fixtures/synthetic")
    }()

    struct Clip {
        let name: String
        let codec: VideoCodec
        let data: Data
        let units: [AccessUnit]

        func frames(fps: Int64 = 20) -> [VideoFrame] {
            units.enumerated().map { index, unit in
                VideoFrame(
                    codec: codec, isKeyframe: unit.isKeyframe,
                    ptsMicros: Int64(index) * 1_000_000 / fps,
                    annexB: data.subdata(in: unit.range))
            }
        }
    }

    static func load(_ name: String) throws -> Clip {
        let codec: VideoCodec = name.hasSuffix(".h265") ? .h265 : .h264
        let data = try Data(contentsOf: directory.appendingPathComponent(name))
        return Clip(name: name, codec: codec, data: data, units: AccessUnitSplitter.split(data, codec: codec))
    }
}
