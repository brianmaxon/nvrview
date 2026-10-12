import CoreGraphics
import Foundation
import VideoKit

/// The picture of a downloaded file at a time, for a still image
/// (FFI-RECORDINGS.md section 7a (c), "from a file"): decoded with
/// VideoToolbox from the keyframe at or before (`exact`: on to the picture
/// asked for), ready for `PictureRenderer` and `PictureEncoder`.
public enum StillSource {
    public static func fromFile(_ path: String, atMs: Int64, exact: Bool) async -> PictureRender? {
        guard let file = try? await RecordedFileSource.open(URL(fileURLWithPath: path)) else { return nil }
        defer { file.close() }
        file.show(atMs, keyframesOnly: !exact)
        guard let picture = file.latestPicture() else { return nil }
        let w = picture.width, h = picture.height
        return PictureRender(
            picture: picture, color: file.info.color, source: CGRect(x: 0, y: 0, width: w, height: h), width: w, height: h)
    }
}
