import AppBase
import Foundation
import NVRCore

/// Where recordings and saved pictures go unless the user chose a folder.
package enum CaptureDefaults {
    package static var recordingsFolder: URL {
        FileManager.default.urls(for: .moviesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("NVRView", isDirectory: true)
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Movies/NVRView", isDirectory: true)
    }

    package static var picturesFolder: URL {
        FileManager.default.urls(for: .picturesDirectory, in: .userDomainMask).first?
            .appendingPathComponent("NVRView", isDirectory: true)
            ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Pictures/NVRView", isDirectory: true)
    }
}

/// The capture settings the app keeps (in the user defaults) and the capture
/// controller and its settings page read. The app model conforms.
@MainActor
package protocol CaptureHost: AppHost {
    var picturesFolder: URL { get set }
    var pictureFormat: PictureFormat { get set }
    var exportQuality: ExportQuality { get set }
    var shareSafeNames: Bool { get set }
    var recordingsFolder: URL { get set }
    var recordFrom: RecordQuality { get set }
    var maxRecordingMinutes: Int { get set }
    var maxRecordingGB: Double { get set }
    var devices: [DeviceRuntime] { get }
    func activeCall(for camera: CameraRef) -> DoorbellCall?
}
