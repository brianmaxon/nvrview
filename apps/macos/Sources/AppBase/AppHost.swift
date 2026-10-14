import Foundation
import NVRCore
import VideoKit

/// What the feature modules (calls, recording, alerts, camera motion) need
/// from the app, and nothing more. The app model conforms, so the features
/// never name it: the dependency runs one way, from the app down to them.
/// Main actor only, like the model.
@MainActor
package protocol AppHost: AnyObject {
    // The core and the clock.
    var clock: any ViewerClock { get }
    var bridge: CoreBridge { get }
    var leases: TileLeases { get }

    // Change notification and the event log.
    func changed()
    func logEvent(_ text: String, transcriptFollows: Bool)

    // Cameras.
    func name(of camera: CameraRef) -> String
    func cameraOptions(_ camera: CameraRef) -> CameraOptions
    func setAllowTalk(_ camera: CameraRef, _ on: Bool)
    func setEntrance(_ camera: CameraRef, _ on: Bool)
    var devices: [DeviceRuntime] { get }
    var allCameras: [CameraRef] { get }
    func listedChannels(_ device: DeviceRuntime) -> [ChannelInfo]
    func cameras(device key: String, channel c: ChannelInfo) -> [CameraRef]
    func cameraRef(_ c: AlertCamera) -> CameraRef?
    func alertCamera(_ c: CameraRef) -> AlertCamera?
    func isFisheye(_ camera: CameraRef) -> Bool
    func fisheyeLens(_ camera: CameraRef, picture: DecodedPicture?) -> FisheyeLens?
    func setFisheyeMount(_ camera: CameraRef, _ mount: FisheyeMount)

    // Audio.
    var audible: Set<CameraRef> { get }
    func setAudible(_ new: Set<CameraRef>)

    // Calls and their recordings.
    var recordCalls: Bool { get }
    var saveCallTranscripts: Bool { get }
    var transcripts: TranscriptStore { get }
    func callPresentation(for camera: CameraRef) -> CallPresentation
    func startCallRecording(_ camera: CameraRef)
    func reservedCallStem(_ camera: CameraRef) -> String?
    func callEnded(_ camera: CameraRef)
}

extension AppHost {
    package func logEvent(_ text: String) { logEvent(text, transcriptFollows: false) }
}
