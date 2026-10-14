import AppAlerts
import AppBase
import AppCalls
import AppCapture
import AppPTZ
import Foundation
import NVRCore
import VideoKit

/// What the stage (the tile grid, its headers, insets, and the call window)
/// needs from the app beyond the alert and capture hosts. The app model
/// conforms.
@MainActor
package protocol StageHost: AlertsHost {
    // Features the stage drives.
    var capture: CaptureController! { get }
    var calls: DoorbellCalls! { get }
    var ptz: PTZModel! { get }

    // What the stage shows.
    var alerts: AlertCenter { get }
    var detections: DetectionTracker { get }
    var showFrameRates: Bool { get }
    var indicatorCorner: ChromeCorner { get }
    var insetCorner: ChromeCorner { get }
    var insetGrid: InsetGridSetting { get }
    var callsOpenInWindow: Bool { get }
    var callWindowPlacement: CallWindowPlacement.Mode { get set }
    var popOutCall: (CameraRef) -> Void { get }
    var popInCall: (CameraRef) -> Void { get }

    // Cameras and devices.
    func deviceName(_ key: String) -> String
    func hasCameraAI(_ camera: CameraRef) -> Bool
    func channelInfo(_ camera: CameraRef) -> ChannelInfo?
    func runtime(_ key: String) -> DeviceRuntime?
    func veil(for camera: CameraRef) -> TileVeil?
    func retryOffered(for camera: CameraRef) -> Bool
    func retry(_ camera: CameraRef)
    func level(_ camera: CameraRef) -> AudioLevel?
    func switchNote(forTile tile: UInt64) -> String?
    func dismissAlert(_ camera: CameraRef)
    func toggleListening(_ camera: CameraRef, additive: Bool)
    func setCallPresentation(_ camera: CameraRef, _ presentation: CallPresentation?)

    // Observers.
    func subscribe(_ body: @escaping () -> Void) -> UUID
    func unsubscribe(_ token: UUID)
    func subscribeDetections(_ body: @escaping (DetectionEvent, CameraRef, Date) -> Void) -> UUID
    func unsubscribeDetections(_ token: UUID)
}
