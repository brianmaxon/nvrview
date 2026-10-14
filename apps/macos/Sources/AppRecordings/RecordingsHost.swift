import AppBase
import AppCapture
import Foundation
import NVRCore
import VideoKit

/// What the recordings screens and player need from the app beyond the
/// capture settings: fisheye handling for the player's picture. The app model
/// conforms.
@MainActor
package protocol RecordingsHost: CaptureHost {
    func isFisheye(_ camera: CameraRef) -> Bool
    func fisheyeLens(_ camera: CameraRef, picture: DecodedPicture?) -> FisheyeLens?
    func setFisheyeMount(_ camera: CameraRef, _ mount: FisheyeMount)
}
