import AppKit
import NVRCore
import SwiftUI

// The Scene Memory pane of the Events window (docs/UX.md "Events window"):
// a camera list, and for the chosen camera its newest kept picture with the
// chosen heatmap layer over it (a color scale with a legend, an opacity
// slider, a class picker), the box layers, the scalar layers (the thresholds
// in force), each layer's description and provenance, and Reset. Any layer
// of a known kind is drawn the same way, so a new technique in the core needs
// no new code here.

/// The Events window's content: the event log or the scene memory.
package struct SmartAlertEventsTabs: View {
    @Bindable package var events: SmartAlertEventsModel
    @Bindable package var scene: SceneMemoryModel

    package var body: some View {
        VStack(spacing: 0) {
            Picker("", selection: $scene.pane) {
                ForEach(EventsWindowPane.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden().fixedSize()
            .padding(.vertical, 6)
            Divider()
            switch scene.pane {
            case .events: SmartAlertEventsView(events: events)
            case .sceneMemory: SceneMemoryView(scene: scene)
            }
        }
    }
}

package struct SceneMemoryView: View {
    @Bindable package var scene: SceneMemoryModel

    package var body: some View {
        HStack(spacing: 0) {
            cameraList.frame(width: 220)
            Divider()
            detail.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var cameraList: some View {
        Group {
            if scene.unavailable {
                Text("The scene memory is not available (smart alerts are off, or this core cannot show it).")
                    .foregroundStyle(.secondary).padding()
            } else if scene.cameras.isEmpty {
                Text("Nothing learned yet. Each check's boxes teach the memory; it shows here once a camera has some.")
                    .foregroundStyle(.secondary).padding()
            } else {
                List(selection: Binding(get: { scene.selected }, set: { scene.select($0) })) {
                    ForEach(scene.cameras, id: \.camera) { c in
                        VStack(alignment: .leading) {
                            Text(scene.cameraName(c.camera))
                            Text(c.classes.joined(separator: ", ")).font(.caption).foregroundStyle(.secondary)
                        }
                        .tag(c.camera)
                    }
                }
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
    }

    @ViewBuilder
    private var detail: some View {
        if let insights = scene.insights, let camera = scene.selected {
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(scene.cameraName(camera)).font(.headline)
                        Text(insights.night ? "night thresholds now" : "day thresholds now")
                            .font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button("Reset Scene Memory…") { scene.confirmingReset = true }
                            .help("Forgets what the alert filters learned about this camera. Nothing is sent to the camera.")
                    }
                    SceneMemoryPicture(scene: scene, insights: insights).frame(minHeight: 240, idealHeight: 360)
                    controls(insights)
                    if let note = scene.note { Text(note).font(.caption).foregroundStyle(.secondary) }
                    ForEach(insights.scalarLayers) { scalars($0) }
                    ForEach(insights.layers.filter { if case .other = $0.data { true } else { false } }) { l in
                        about(l, prefix: "Not drawn by this version: ")
                    }
                }
                .padding(12)
            }
            .confirmationDialog(
                "Reset the scene memory of \(scene.cameraName(camera))?", isPresented: $scene.confirmingReset
            ) {
                Button("Reset", role: .destructive) { scene.reset() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Things that stay in place will alert again until the memory has learned them.")
            }
        } else {
            Text(scene.cameras.isEmpty ? "" : "Choose a camera.").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder
    private func controls(_ insights: AlertInsights) -> some View {
        let grids = insights.gridLayers
        if grids.isEmpty {
            Text("No heatmap for this camera yet.").foregroundStyle(.secondary)
        } else {
            HStack(spacing: 16) {
                Picker("Layer", selection: $scene.layerID) {
                    ForEach(grids) { Text($0.title).tag(String?.some($0.id)) }
                }
                .fixedSize()
                HStack {
                    Text("Opacity")
                    Slider(value: $scene.opacity, in: 0...1).frame(width: 140)
                        .accessibilityLabel("Heatmap opacity")
                }
                if !insights.boxLayers.isEmpty {
                    Toggle("Last alerted boxes", isOn: $scene.showBoxes)
                }
            }
            if let layer = scene.layer { legend(layer) }
        }
    }

    /// The color scale with its range and the layer's threshold.
    private func legend(_ layer: InsightLayer) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(String(format: "%.2g", layer.range.lowerBound)).font(.caption.monospacedDigit())
                LinearGradient(
                    colors: InsightColorScale.stops.map { Color(.sRGB, red: $0.r, green: $0.g, blue: $0.b) },
                    startPoint: .leading, endPoint: .trailing
                )
                .frame(width: 220, height: 10).clipShape(RoundedRectangle(cornerRadius: 2))
                .overlay(alignment: .leading) {
                    if let t = layer.grid?.threshold {
                        let span = layer.range.upperBound - layer.range.lowerBound
                        Rectangle().fill(Color.white).frame(width: 2, height: 14)
                            .offset(x: 220 * (t - layer.range.lowerBound) / max(span, 1e-9) - 1)
                    }
                }
                Text(String(format: "%.2g", layer.range.upperBound)).font(.caption.monospacedDigit())
                Text(layer.units).font(.caption).foregroundStyle(.secondary)
                if let t = layer.grid?.threshold {
                    Text("hot at \(String(format: "%.2f", t)) (white mark)").font(.caption).foregroundStyle(.secondary)
                }
            }
            about(layer, prefix: "")
        }
    }

    private func scalars(_ layer: InsightLayer) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(layer.title).font(.subheadline.bold())
            if case .scalar(let items) = layer.data {
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 2) {
                    ForEach(items) { i in
                        GridRow {
                            Text(i.label)
                            Text(i.text).font(.body.monospacedDigit())
                        }
                    }
                }
            }
            about(layer, prefix: "")
        }
    }

    private func about(_ layer: InsightLayer, prefix: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            if !layer.description.isEmpty {
                Text(prefix + layer.description).font(.caption).fixedSize(horizontal: false, vertical: true)
            }
            if !layer.provenance.isEmpty {
                Text("Method: " + layer.provenance).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// The kept picture with the chosen heatmap layer and the box layers over it.
package struct SceneMemoryPicture: View {
    package let scene: SceneMemoryModel
    package let insights: AlertInsights

    package init(scene: SceneMemoryModel, insights: AlertInsights) {
        self.scene = scene
        self.insights = insights
    }

    package var body: some View {
        GeometryReader { geo in
            let size = scene.picture?.size ?? CGSize(width: 16, height: 9)
            let rect = EvidenceOverlay.pictureRect(picture: size, in: geo.size)
            ZStack(alignment: .topLeading) {
                if let picture = scene.picture {
                    Image(nsImage: picture).resizable().aspectRatio(contentMode: .fit)
                        .frame(width: geo.size.width, height: geo.size.height)
                } else {
                    Rectangle().fill(Color.black.opacity(0.85)).frame(width: rect.width, height: rect.height)
                        .overlay { Text("No kept picture of this camera").foregroundStyle(.secondary) }
                        .offset(x: rect.minX, y: rect.minY)
                }
                if let layer = scene.layer, let grid = layer.grid, let image = InsightColorScale.image(grid, range: layer.range) {
                    Image(decorative: image, scale: 1).resizable().interpolation(.none)
                        .frame(width: rect.width, height: rect.height)
                        .opacity(scene.opacity)
                        .offset(x: rect.minX, y: rect.minY)
                        .accessibilityLabel("\(layer.title) heatmap")
                }
                if scene.showBoxes {
                    ForEach(insights.boxLayers) { l in
                        if case .boxes(let boxes, _) = l.data {
                            ForEach(Array(boxes.enumerated()), id: \.offset) { _, b in
                                Rectangle().strokeBorder(Color.white, style: StrokeStyle(lineWidth: 2, dash: [5, 3]))
                                    .frame(width: b.w * rect.width, height: b.h * rect.height)
                                    .overlay(alignment: .topLeading) {
                                        Text("\(b.className) \(Int((b.score * 100).rounded()))%")
                                            .font(.caption2.bold()).padding(.horizontal, 3)
                                            .background(Color.white).foregroundStyle(.black).offset(y: -14)
                                    }
                                    .offset(x: rect.minX + b.x * rect.width, y: rect.minY + b.y * rect.height)
                            }
                        }
                    }
                }
            }
        }
    }
}
