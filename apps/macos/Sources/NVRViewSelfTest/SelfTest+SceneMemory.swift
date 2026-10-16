@testable import NVRViewApp
import AppAlerts
import AppKit
import FakeCore
import Foundation
import NVRCore
import SwiftUI

// The Events window's Scene Memory pane (docs/UX.md "Events window") on the
// fake core's scripted insight layers: the camera list, the kept picture
// with the heatmap drawn over it in the scale's colors, the layer picker
// (a second, made-up layer id shows any grid layer is drawn the same way),
// the opacity, the thresholds, and Reset. Part of SelfTest
// (SelfTest+Alerts.swift runs it after the kept-pictures step).

private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// Step `alerts-scene-memory`; `entry` is a decision whose first kept
    /// frame (from `alerts-events-image`) is the picture.
    func sceneMemoryStep(entry: SmartAlertLogEntry) {
        // parity: alerts-scene-memory
        let id = "alerts-scene-memory"
        guard let model = app.model, let fake = model.fake, let scene = app.eventsWindow?.scene,
            let camera = entry.camera
        else { return }
        func fail(_ text: String) { problems.append("\(id): \(text)") }
        defer { scene.pane = .events }

        // Two grid layers of 8 by 4: the class memory hot in the top right
        // cell, and a made-up layer hot in the bottom left one.
        func grid(_ hot: Int) -> String { (0..<32).map { $0 == hot ? "1" : "0" }.joined(separator: ",") }
        let layers = """
            [{"id":"scene.person","title":"Scene memory: person","description":"Where person boxes have stayed.",
              "provenance":"A running average.","units":"warmth","range":[0,1],"kind":"grid","rows":4,"cols":8,
              "values":[\(grid(7))],"class":"person","threshold":0.7},
             {"id":"selftest.other","title":"Another method","description":"A layer the app has no code for.",
              "provenance":"Scripted.","units":"","range":[0,1],"kind":"grid","rows":4,"cols":8,"values":[\(grid(24))]},
             {"id":"repeat.last","title":"Last alerted boxes","description":"","provenance":"","units":"score","range":[0,1],
              "kind":"boxes","boxes":[{"class":"person","score":0.6,"box":{"x":0.1,"y":0.4,"w":0.2,"h":0.3}}]},
             {"id":"thresholds","title":"Thresholds","description":"","provenance":"","units":"","range":[0,1],"kind":"scalar",
              "items":[{"id":"floor.night","label":"Score floor at night","value":0.5,"units":"score"},
                       {"id":"parked.iou","label":"Parked overlap","value":0.8,"units":"IoU"}]}]
            """
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        fake.scriptInsights(camera: camera, classes: ["person"], layersJSON: layers, picture: (entry.triggerID, 0, nowMs))
        scene.pane = .sceneMemory
        guard scene.cameras.map(\.camera) == [camera], scene.selected == camera, let insights = scene.insights else {
            return fail("the pane listed \(scene.cameras.map(\.camera)) and opened \(String(describing: scene.selected))")
        }
        if scene.picture == nil { fail("the kept picture was not loaded") }
        if insights.gridLayers.map(\.id) != ["scene.person", "selftest.other"] || scene.layerID != "scene.person" {
            fail("layers \(insights.gridLayers.map(\.id)), shown \(scene.layerID ?? "none")")
        }
        if insights.scalarLayers.first.map({ if case .scalar(let i) = $0.data { i.count } else { 0 } }) != 2 {
            fail("the thresholds were not read")
        }

        // Drawn: the hot cell has the scale's top color at full opacity, and
        // the picture's own gray with the overlay off.
        let size = CGSize(width: 640, height: 480)
        func pixel(row: Int, col: Int) -> NSColor? {
            let r = ImageRenderer(content: SceneMemoryPicture(scene: scene, insights: insights).frame(width: size.width, height: size.height))
            r.scale = 1
            guard let cg = r.cgImage else { return nil }
            let rect = EvidenceOverlay.pictureRect(picture: scene.picture?.size ?? size, in: size)
            let x = Int(rect.minX + (Double(col) + 0.5) * rect.width / 8)
            let y = Int(rect.minY + (Double(row) + 0.5) * rect.height / 4)
            return NSBitmapImageRep(cgImage: cg).colorAt(x: x, y: y)?.usingColorSpace(.sRGB)
        }
        let top = InsightColorScale.stops[4]
        func isTop(_ c: NSColor?) -> Bool {
            guard let c else { return false }
            return abs(c.redComponent - top.r) < 0.08 && abs(c.greenComponent - top.g) < 0.08 && abs(c.blueComponent - top.b) < 0.08
        }
        scene.opacity = 1
        let hot = pixel(row: 0, col: 7), cold = pixel(row: 2, col: 4)
        if !isTop(hot) { fail("the hot cell is drawn \(String(describing: hot)), not the scale's top color") }
        if isTop(cold) { fail("a cold cell is drawn hot") }
        scene.opacity = 0
        if isTop(pixel(row: 0, col: 7)) { fail("the heatmap shows at opacity 0") }
        // The other layer: drawn the same way, hot elsewhere.
        scene.opacity = 1
        scene.layerID = "selftest.other"
        if !isTop(pixel(row: 3, col: 0)) || isTop(pixel(row: 0, col: 7)) { fail("the second layer is not drawn where it is hot") }

        // Reset (after the confirmation): forgotten.
        scene.confirmingReset = true
        scene.reset()
        if !scene.cameras.isEmpty || scene.note?.hasPrefix("Scene memory of ") != true || scene.confirmingReset {
            fail("after Reset: \(scene.cameras.count) cameras, note \(scene.note ?? "none")")
        }
        print("\(id): camera list, picture, heatmap colors and opacity, a second layer, thresholds and Reset checked")
    }
}
