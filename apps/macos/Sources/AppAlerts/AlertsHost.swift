import AlertHost
import AppBase
import AppCapture
import Foundation
import NVRCore
import VideoKit

/// What the smart-alert model, settings, events window, and verification
/// views need from the app beyond `AppHost`. The app model conforms.
@MainActor
package protocol AlertsHost: CaptureHost {
    /// Where the core keeps its data (kept evidence lives under it).
    var coreDataDirectory: URL? { get }
    var pictureRenderer: PictureRenderer { get }
    /// The on-device alert host, once started.
    var smartAlerts: SmartAlertsController? { get }
    /// The smart-alerts model this app owns.
    var alertsModel: SmartAlertsModel! { get }
    /// Asks the app to configure the alert host again (settings changed).
    func smartAlertsNeedConfigure()
}
