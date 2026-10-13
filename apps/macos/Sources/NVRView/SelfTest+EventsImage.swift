import AppKit
import FakeCore
import Foundation
import NVRCore
import UniformTypeIdentifiers

// Kept pictures out of the Events window (docs/UX.md "Events"): the list's
// picture icon and "With pictures" filter, the large viewer, Save As, Copy,
// Open in Preview, the drag out, and Show in Finder, on the fake core's
// evidence store. Part of SelfTest (SelfTest+Alerts.swift runs it after the
// event log's steps).

private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// Step `alerts-events-image`; `entry` is a decision of the event log.
    func eventsImageStep(_ events: SmartAlertEventsModel, entry: SmartAlertLogEntry) async {
        // parity: alerts-events-image
        let id = "alerts-events-image"
        guard let model = app.model, let fake = model.fake else { return }
        func fail(_ text: String) { problems.append("\(id): \(text)") }
        let width = 320, height = 240
        let manager = FileManager.default
        let work = manager.temporaryDirectory.appendingPathComponent("nvrview-events-image-\(UUID().uuidString)")
        try? manager.createDirectory(at: work, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: work) }
        let savedTemp = events.tempFolder, savedOpener = events.opener
        defer {
            events.tempFolder = savedTemp
            events.opener = savedOpener
            events.now = { Date() }
            events.withPictures = false
            model.coreDataDirectory = nil
        }

        // Two kept frames, boxes from the camera's AI claim and our detector on the first.
        let nowMs = Int(Date().timeIntervalSince1970 * 1000)
        let doc = """
            {"methods":{"camera_ai":{"claimed":"person"},
              "detector":{"frames":{"0":{"boxes":[{"label":"person","score":0.9,"box":{"x":0.4,"y":0.3,"w":0.2,"h":0.4}}]}}}}}
            """
        let jpeg = Self.sampleJPEG(width: width, height: height)
        for frame in 0..<2 {
            let json = #"{"trigger_id":\#(entry.triggerID),"frame":\#(frame),"at_ms":\#(nowMs + frame),"doc":\#(doc)}"#
            do { try fake.alertEvidencePut(json: json, data: jpeg) } catch { return fail("the fake core refused a frame: \(error)") }
        }
        events.reload()
        events.select(entry.id)
        guard let subject = events.subject else { return fail("the Events window loaded no picture") }

        // The list's icon: only while frames are kept; the filter follows it.
        let other = events.entries.first { $0.triggerID != entry.triggerID && events.evidenceSets[$0.triggerID] == nil }
        let badge = events.badge(entry)
        if badge?.frames != 2 || badge?.tooltip.hasPrefix("2 pictures, deleted in ") != true {
            fail("the row's icon was \(String(describing: badge))")
        }
        if let other, events.badge(other) != nil { fail("a row without pictures has an icon") }
        events.withPictures = true
        let filtered = events.entries.map(\.triggerID)
        events.withPictures = false
        if filtered != [entry.triggerID] { fail("the With pictures filter showed \(filtered)") }
        events.now = { Date().addingTimeInterval(3 * 3600) }
        if events.badge(entry) != nil { fail("an expired set still has its icon") }
        events.now = { Date() }

        // Enlarge: the viewer opens on the shown frame; arrows step, Esc closes.
        events.openViewer()
        guard let viewer = events.viewer, viewer.window?.isVisible == true else { return fail("the viewer did not open") }
        viewer.window?.orderBack(nil)
        let opened = events.frame
        viewer.handle(.next)
        let stepped = events.frame
        viewer.handle(.previous)
        let back = events.frame
        viewer.handle(.close)
        if opened != 0 || stepped != 1 || back != 0 { fail("the viewer's frame went \(opened), \(stepped), \(back)") }
        if events.viewer != nil { fail("Esc did not close the viewer") }
        // The pane's picture is the frame as drawn at the viewer's zoom: whole at 1x.
        let base = ZoomBase(picture: subject.image.size, tile: CGSize(width: 800, height: 500), fill: false)
        let whole = EvidenceViewerView.pictureFrame(.none, base: base, in: CGSize(width: 800, height: 500))
        let fit = EvidenceOverlay.pictureRect(picture: subject.image.size, in: CGSize(width: 800, height: 500))
        if abs(whole.width - fit.width) > 0.5 || abs(whole.height - fit.height) > 0.5 {
            fail("the viewer's 1x picture \(whole) is not the fitted one \(fit)")
        }
        let zoomed = EvidenceViewerView.pictureFrame(
            TileZoom.none.zoomed(by: 2, atUnit: CGPoint(x: 0.5, y: 0.5), in: base), base: base, in: CGSize(width: 800, height: 500))
        if abs(zoomed.width - 2 * fit.width) > 1 { fail("zoomed 2x the picture is \(zoomed.width) wide, not \(2 * fit.width)") }

        // Save As: the frame's size, with the boxes or without.
        let withBoxes = work.appendingPathComponent("boxes.png"), plain = work.appendingPathComponent("plain.png")
        guard events.writePicture(to: withBoxes, withBoxes: true), events.writePicture(to: plain, withBoxes: false) else {
            return fail("a picture was not written")
        }
        guard let boxed = NSBitmapImageRep(data: (try? Data(contentsOf: withBoxes)) ?? Data()),
            let clean = NSBitmapImageRep(data: (try? Data(contentsOf: plain)) ?? Data())
        else { return fail("a written picture cannot be read") }
        if boxed.pixelsWide != width || boxed.pixelsHigh != height || clean.pixelsWide != width || clean.pixelsHigh != height {
            fail("sizes \(boxed.pixelsWide)x\(boxed.pixelsHigh), \(clean.pixelsWide)x\(clean.pixelsHigh), wanted \(width)x\(height)")
        }
        // The detector's box (second method: cyan) has its left edge at x = 0.4.
        let edgeX = Int(0.4 * Double(width)) + 1, edgeY = height / 2
        let a = boxed.colorAt(x: edgeX, y: edgeY)?.usingColorSpace(.sRGB), b = clean.colorAt(x: edgeX, y: edgeY)?.usingColorSpace(.sRGB)
        if let a, let b, abs(a.blueComponent - b.blueComponent) + abs(a.redComponent - b.redComponent) < 0.2 {
            fail("the box is not in the picture with boxes")
        }
        if (try? Data(contentsOf: withBoxes)) == (try? Data(contentsOf: plain)) { fail("with and without boxes are the same file") }

        // Copy: the picture as shown (the boxes are on) is on the clipboard.
        let board = NSPasteboard(name: NSPasteboard.Name("nvrview.selftest.evidence"))
        defer { board.releaseGlobally() }
        let copied = events.copyPicture(to: board)
        if !copied || board.data(forType: .png) != (try? Data(contentsOf: withBoxes)) {
            fail("the clipboard did not get the picture as shown")
        }

        // Open in Preview: a temporary file of the same picture, named like Save Picture.
        events.tempFolder = work.appendingPathComponent("temp")
        var previewed: URL?
        events.opener = { previewed = $0 }
        events.openInPreview()
        let name = subject.camera
        if previewed.map({ !$0.lastPathComponent.hasSuffix(" \(name).png") || (try? Data(contentsOf: $0)) != (try? Data(contentsOf: withBoxes)) }) != false {
            fail("Open in Preview wrote \(previewed?.lastPathComponent ?? "nothing")")
        }

        // Drag: the promised file is made on request and is the same picture.
        if let provider = events.dragProvider() {
            let dropped: (name: String, data: Data)? = await withCheckedContinuation { continuation in
                _ = provider.loadFileRepresentation(forTypeIdentifier: UTType.png.identifier) { url, _ in
                    continuation.resume(returning: url.flatMap { u in (try? Data(contentsOf: u)).map { (u.lastPathComponent, $0) } })
                }
            }
            if dropped?.data != (try? Data(contentsOf: withBoxes)) { fail("the dragged file is not the picture") }
            if dropped?.name.hasSuffix(" \(name).png") != true { fail("the dragged file is named \(dropped?.name ?? "nothing")") }
        } else {
            fail("no drag provider")
        }

        // Show in Finder: only for a kept (pinned) set whose folder is there.
        let folder = work.appendingPathComponent("core/alerts/evidence/pinned/\(entry.triggerID)")
        try? manager.createDirectory(at: folder, withIntermediateDirectories: true)
        model.coreDataDirectory = work.appendingPathComponent("core")
        let titles = { events.pictureMenu(window: { nil }).items.map(\.title) }
        if events.pinnedFolder != nil || titles().contains("Show in Finder") { fail("Show in Finder is offered for a set that is not kept") }
        events.keepEvidence()
        if events.pinnedFolder?.path != folder.path || !titles().contains("Show in Finder") {
            fail("Show in Finder is not offered for a kept set: \(titles())")
        }
        if events.badge(entry)?.tooltip.contains("kept") != true { fail("the kept set's tooltip is \(events.badge(entry)?.tooltip ?? "none")") }
        print("\(id): icon, filter, viewer, save \(width)x\(height), copy, preview, drag and Show in Finder checked")
    }

    /// A plain JPEG for a kept frame.
    static func sampleJPEG(width: Int, height: Int) -> Data {
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 3,
            hasAlpha: false, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        if let context = NSGraphicsContext(bitmapImageRep: rep) {
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = context
            NSColor(calibratedWhite: 0.35, alpha: 1).setFill()
            NSRect(x: 0, y: 0, width: width, height: height).fill()
            NSGraphicsContext.restoreGraphicsState()
        }
        return rep.representation(using: .jpeg, properties: [:]) ?? Data()
    }
}
