import AppKit
import NVRCore

// The Events window's Scene Memory pane (docs/UX.md "Events window"): per
// camera, the newest kept picture with what the alert filters learned over
// it. The core sends generic insight layers (FFI minor 17); this model picks
// the camera, the heatmap layer, and the overlay's opacity, and resets a
// camera's memory after a confirmation.

/// Which pane of the Events window is shown.
package enum EventsWindowPane: String, CaseIterable, Identifiable {
    case events = "Events"
    case sceneMemory = "Scene Memory"
    package var id: String { rawValue }
}

@MainActor
@Observable
package final class SceneMemoryModel {
    @ObservationIgnored package let app: any AlertsHost
    package var pane: EventsWindowPane = .events {
        didSet { if pane != oldValue, pane == .sceneMemory { reload() } }
    }
    /// The cameras the filters learned about, with their classes.
    package private(set) var cameras: [InsightCameraList.Entry] = []
    /// The core cannot say (an older core, or smart alerts not set up).
    package private(set) var unavailable = false
    package private(set) var selected: AlertCamera?
    package private(set) var insights: AlertInsights?
    /// The kept picture the layers are drawn on.
    package private(set) var picture: NSImage?
    /// The heatmap layer shown (the class picker).
    package var layerID: String?
    /// The heatmap's opacity over the picture.
    package var opacity = 0.6
    /// Draw the box layers (the last alerted boxes).
    package var showBoxes = true
    /// Waiting for the reset's confirmation.
    package var confirmingReset = false
    /// What the last action did, in words.
    package var note: String?

    package init(app: any AlertsHost) {
        self.app = app
    }

    /// Reads the camera list again, and the shown camera's layers.
    package func reload() {
        guard let list = app.alertsModel.insightCameras() else {
            unavailable = true
            cameras = []
            insights = nil
            picture = nil
            return
        }
        unavailable = false
        cameras = list.cameras.sorted { cameraName($0.camera) < cameraName($1.camera) }
        let keep = selected.flatMap { s in cameras.contains { $0.camera == s } ? s : nil }
        select(keep ?? cameras.first?.camera)
    }

    package func select(_ camera: AlertCamera?) {
        selected = camera
        note = nil
        guard let camera, let i = app.alertsModel.insights(camera: camera) else {
            insights = nil
            picture = nil
            return
        }
        insights = i
        picture = i.picture.flatMap { app.alertsModel.evidenceFrame(trigger: $0.triggerID, index: $0.frame) }
        if layerID == nil || !i.gridLayers.contains(where: { $0.id == layerID }) {
            layerID = i.gridLayers.first?.id
        }
    }

    /// The heatmap layer shown.
    package var layer: InsightLayer? { insights?.gridLayers.first { $0.id == layerID } }

    package func cameraName(_ c: AlertCamera) -> String { app.cameraRef(c).map(app.name(of:)) ?? "Camera \(c.channel + 1)" }

    /// Forgets the shown camera's memory (after the confirmation).
    package func reset() {
        confirmingReset = false
        guard let camera = selected else { return }
        let ok = app.alertsModel.resetInsights(camera: camera)
        let name = cameraName(camera)
        reload()
        note = ok ? "Scene memory of \(name) reset." : "The scene memory could not be reset."
    }
}
