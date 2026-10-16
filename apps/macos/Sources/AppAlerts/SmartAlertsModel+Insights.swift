import Foundation
import NVRCore

// What the smart-alert filters learned (docs/UX.md "Events window", "Scene
// Memory"; FFI minor 17): the cameras with a memory, one camera's insight
// layers, and the per-camera reset.

extension SmartAlertsModel {
    /// The cameras the filters learned about; nil when the core cannot say
    /// (an older core, or smart alerts not set up).
    package func insightCameras() -> InsightCameraList? {
        guard let text = try? bridge.smartAlertsAPI().alertInsightsGet(query: nil) else { return nil }
        return InsightCameraList.parse(text)
    }

    /// One camera's layers.
    package func insights(camera: AlertCamera) -> AlertInsights? {
        guard let text = try? bridge.smartAlertsAPI().alertInsightsGet(query: #"{"camera":\#(camera.json)}"#) else {
            return nil
        }
        return AlertInsights.parse(text)
    }

    /// Forgets what the filters learned about a camera (local data only).
    @discardableResult
    package func resetInsights(camera: AlertCamera) -> Bool {
        (try? bridge.smartAlertsAPI().alertInsightsReset(request: #"{"camera":\#(camera.json)}"#)) != nil
    }
}
