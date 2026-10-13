import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Pictures and recordings made from tiles, and the real core's recordings read back: the self-test's "capture" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// Runs `body` once the capture board says `done` (the fake core's
    /// queued `recording` events are sent first, on each poll).
    func whenCapture(
        _ timeout: Double, _ done: @escaping @MainActor (RecordingBoard) -> Bool,
        _ body: @escaping @MainActor () -> Void
    ) {
        guard let model = app.model else { return body() }
        when({
            model.fake?.completeCaptureWork()
            return done(model.capture.board)
        }, timeout: timeout, body)
    }

    /// R records the selected tile's camera from the next keyframe: the
    /// pill ("● REC") on its tile stays when the header fades and at any
    /// width, the header's Record says Stop, the toolbar and the Camera
    /// menu list it, and the header's Stop ends it ("Recording saved").
    /// Then a start under the free-space guard says why, and a device's
    /// third main-stream recording offers the sub-stream instead. Both
    /// cores (the fake writes nothing; the real one writes into the
    /// self-test's folder).
    func recordStep(_ path: NodePath, _ camera: CameraRef) {
        guard let model = app.model, let window, let chrome = window.stage.chromes[path] else { return pictureStep() }
        guard runs("capture") else { return insetStep() }
        let stage = window.stage
        // parity: capture-encoder-priming
        do {
            let (reported, measured) = try AACCaptureEncoder.measurePriming()
            print("capture: AAC priming reported \(reported), impulse back at \(measured.map { $0 - 4096 } ?? -1)")
            if measured.map({ abs($0 - 4096 - Int(reported)) > 48 }) ?? true {
                problems.append("capture: the encoder's priming \(reported) is not where the audio comes back")
            }
        } catch {
            problems.append("capture: the AAC round trip failed (\(error))")
        }
        if model.capture.encoder == nil || !model.capture.configured {
            problems.append("capture: the encoder was not installed or the core not configured")
        }
        // parity: capture-record-tile
        window.window?.makeFirstResponder(stage)
        window.state.selected = path
        press("r", keyCode: 15)
        let started = model.capture.board.live(of: camera)
        whenCapture(20, { $0.live(of: camera)?.state == "recording" }) { [self] in
            stage.refreshChrome()
            let entry = model.capture.board.live(of: camera)
            let tile = stage.headerTile(camera, path: path)
            let label = tile.flatMap { TileHeaderLayout.menu(for: $0).first { $0.item == .record }?.label }
            print(
                "capture: R started \(started.map { ($0.file as NSString).lastPathComponent } ?? "nothing"), "
                    + "state \(entry?.state ?? "none"), Record says \(label ?? "?")")
            if started == nil { problems.append("capture: R did not start a recording (\(stage.toastText ?? "no message"))") }
            if entry?.state != "recording" { problems.append("capture: the recording did not reach its first keyframe") }
            if label != "Stop Recording" { problems.append("capture: the header's Record did not become Stop") }
            if let f = started?.file, !f.hasPrefix(model.recordingsFolder.path) {
                problems.append("capture: the recording is not in the recordings folder")
            }
            // parity: capture-record-indicator
            let pill = chrome.pillText
            chrome.fadeOut()
            let stays = chrome.pillText != nil
            chrome.showChrome()
            let narrow = tile.map {
                TileHeaderLayout.compute(width: 100, titleWidth: 200, tile: $0, previous: nil).visible.contains(.recordingPill)
            }
            let toolbar = window.recordingItemTitle
            let menu = NSMenu()
            RecordingsMenu.shared.fill(menu)
            let listed = menu.items.filter { $0.action == #selector(ViewerWindowController.stopRecordingItem(_:)) }.count
            print(
                "capture: pill \(pill ?? "none"), stays when the header fades \(stays), at 100 pt \(narrow ?? false), "
                    + "toolbar \(toolbar ?? "none"), Camera menu lists \(listed)")
            if pill?.hasPrefix("● REC") != true || !stays { problems.append("capture: the tile's pill did not show or did not stay") }
            if narrow != true { problems.append("capture: a narrow header dropped the recording pill") }
            if toolbar != "● 1 recording" { problems.append("capture: the toolbar item said \(toolbar ?? "nothing")") }
            if listed != 1 { problems.append("capture: the Recordings submenu listed \(listed) recordings") }
            chrome.pressHeader(.record)
            whenCapture(15, { $0.running.isEmpty }) { [self] in
                stage.refreshChrome()
                let ended = model.capture.board.ended.last
                let toast = stage.toastText
                print("capture: Stop ended it \(ended?.state ?? "no") (\(ended?.reason ?? "-")), message \(toast ?? "none")")
                if ended?.state != "finished" || ended?.reason != "stopped" { problems.append("capture: Stop did not finish the recording") }
                if toast != "Recording saved" { problems.append("capture: the end said \(toast ?? "nothing")") }
                if chrome.pillText != nil { problems.append("capture: the pill stayed after the recording ended") }
                advance(ToastView.actionDuration + 0.1)
                recordLimitsStep(path, camera)
            }
        }
    }

    /// The free-space guard and the main-stream limit, as messages.
    func recordLimitsStep(_ path: NodePath, _ camera: CameraRef) {
        guard let model = app.model, let window else { return pictureStep() }
        let stage = window.stage
        // parity: capture-record-disk-low
        model.capture.minFreeOverride = 1 << 62
        model.fake?.setCaptureFreeBytes(1)
        stage.performHeaderItem(.record, at: path, from: nil)
        let refused = stage.toastText
        let none = model.capture.board.running.isEmpty
        model.capture.minFreeOverride = nil
        model.fake?.setCaptureFreeBytes(.max)
        advance(ToastView.actionDuration + 0.1)
        print("capture: low disk says \(refused ?? "nothing"), nothing started \(none)")
        if refused?.contains("free disk space") != true || !none { problems.append("capture: low disk did not refuse with its reason") }
        // parity: capture-record-main-limit
        let cameras = stage.frames.leaves.compactMap(\.camera).filter { $0.device == camera.device && $0.lens == 0 }
        guard cameras.count >= 3 else {
            print("capture: fewer than three cameras of one device, the main-stream limit is not checked")
            return pictureStep()
        }
        model.capture.startLive(cameras[0], quality: .main)
        model.capture.startLive(cameras[1], quality: .main)
        model.capture.startLive(cameras[2], quality: .main)
        let message = stage.toastText ?? "nothing"
        let action = stage.toast.actionTitle
        if action == "Record Sub-stream" { stage.toast.pressAction() }
        let third = model.capture.board.live(of: cameras[2])
        print("capture: a third main stream says \(message), offers \(action ?? "nothing"), then \(third?.tier ?? "nothing")")
        if !message.contains("two main streams") || action != "Record Sub-stream" || third?.tier != "sub" {
            problems.append("capture: the main-stream limit did not offer the sub-stream")
        }
        whenCapture(20, { b in b.list.count == 3 && b.list.allSatisfy { $0.state == "recording" } }) { [self] in
            window.stopAllRecordings(nil)
            whenCapture(15, { $0.running.isEmpty }) { [self] in
                if !model.capture.board.running.isEmpty { problems.append("capture: Stop All left recordings running") }
                advance(ToastView.actionDuration + 0.1)
                pictureStep()
            }
        }
    }

    // MARK: Save picture (docs/CAPTURE.md "Save picture")

    /// A throwaway pictures folder for the model to save into.
    func freshPictureFolder(_ model: AppModel) -> URL {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("nvrview-selftest-pictures-\(UUID().uuidString)", isDirectory: true)
        model.picturesFolder = folder
        model.pictureFormat = .png
        model.shareSafeNames = false
        return folder
    }

    /// Runs `action` (a key or a menu item), waits for the save it starts to
    /// finish, and gives the record (nil if none came).
    func saving(
        _ stage: StageView, _ action: () -> Void, then body: @escaping @MainActor (PictureSaveRecord?) -> Void
    ) {
        let before = stage.pictureSaves
        action()
        when({ stage.pictureSaves > before }, timeout: 8) {
            body(stage.pictureSaves > before ? stage.lastPictureSave : nil)
        }
    }

    /// Checks one finished save: a file with the naming rule's shape and
    /// extension, its size, its dimensions (`size`, within a pixel, when
    /// given), and, for a copy of the decoded picture, its pixels against a
    /// CPU conversion of the buffer it was made from. Returns the image.
    @discardableResult
    func checkSaved(
        _ record: PictureSaveRecord?, _ what: String, ext: String, nameEnds: String, size: CGSize?, tolerance: Int
    ) -> RenderedImage? {
        guard let record else {
            problems.append("\(what): no picture was saved")
            return nil
        }
        guard case .success(let saved) = record.result else {
            problems.append("\(what): the save failed: \(record.result)")
            return nil
        }
        let name = saved.url.lastPathComponent
        let shape = /\d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2}\.\d{3} .+/
        let fileSize = (try? FileManager.default.attributesOfItem(atPath: saved.url.path)[.size] as? Int) ?? nil
        guard let data = try? Data(contentsOf: saved.url), let image = PictureEncoder.decode(data) else {
            problems.append("\(what): \(name) is not an image file")
            return nil
        }
        let sized = size.map { abs(CGFloat(image.width) - $0.width) <= 1 && abs(CGFloat(image.height) - $0.height) <= 1 } ?? true
        let named = name.wholeMatch(of: shape) != nil && name.hasSuffix(nameEnds + "." + ext)
        let toast = window?.stage.toastText
        print("\(what): \(name), \(image.width)x\(image.height), \(data.count) bytes, toast \(toast ?? "none")")
        if !named { problems.append("\(what): the file name \(name) does not follow the rule") }
        if fileSize != data.count || data.count < 100 { problems.append("\(what): the file has \(fileSize ?? -1) bytes") }
        if !sized { problems.append("\(what): \(image.width)x\(image.height), expected \(size.map { "\($0)" } ?? "?")") }
        if toast != "Picture saved" || window?.stage.toast.actionTitle != "Show in Finder" {
            problems.append("\(what): the confirmation toast said \(toast ?? "nothing")")
        }
        if !record.dewarped {
            // A JPEG is compared with the same encoder's JPEG of the decoded
            // picture: its losses (far off on a sharp color edge, so they
            // vary with the frame saved) cancel out.
            let buffer = record.picture.pixelBuffer
            let (checked, matched) =
                ext == "jpg"
                ? PictureCheck.compareEncoded(
                    image, buffer: buffer, color: record.color, region: record.region, encoding: PictureFormat.jpeg.encoding,
                    tolerance: tolerance) ?? (0, 0)
                : PictureCheck.compare(image, buffer: buffer, color: record.color, region: record.region, tolerance: tolerance)
            print("\(what): \(matched) of \(checked) sampled pixels match the decoded picture")
            if checked < 20 || matched * 100 < checked * 95 {
                problems.append("\(what): only \(matched) of \(checked) pixels match the decoded picture")
            }
        }
        return image
    }

    /// P saves the tile's full picture as PNG, a share-safe JPEG names the
    /// camera Camera A, and Save Zoomed Area saves the visible region of a
    /// zoomed tile at the source's resolution; each file is read back and its
    /// pixels compared with the decoded buffer.
    func pictureStep() {
        guard let model = app.model, let window else { return insetStep() }
        let stage = window.stage
        guard let leaf = stage.frames.leaves.first(where: { $0.camera.map { !model.isFisheye($0) } == true }),
            let camera = leaf.camera
        else {
            problems.append("no plain tile to save a picture from")
            return insetStep()
        }
        let path = leaf.path
        let folder = freshPictureFolder(model)
        let name = PictureNaming.sanitize(model.name(of: camera))
        when({ model.leases.tile(for: camera)?.latestPicture() != nil }, timeout: 3) { [self] in
            // parity: capture-picture-tile
            window.window?.makeFirstResponder(stage)
            window.state.selected = path
            let size = stage.pictureSizes[camera]
            saving(stage, { press("p", keyCode: 35) }) { [self] record in
                checkSaved(record, "picture", ext: "png", nameEnds: " " + name, size: size, tolerance: 3)
                // Share-safe names and JPEG.
                model.shareSafeNames = true
                model.pictureFormat = .jpeg
                saving(stage, { press("p", keyCode: 35) }) { [self] record in
                    checkSaved(record, "picture (share-safe JPEG)", ext: "jpg", nameEnds: " Camera A", size: size, tolerance: 8)
                    model.shareSafeNames = false
                    model.pictureFormat = .png
                    zoomedPictureStep(path, camera, folder: folder)
                }
            }
        }
    }

    func zoomedPictureStep(_ path: NodePath, _ camera: CameraRef, folder: URL) {
        guard let model = app.model, let window else { return insetStep() }
        let stage = window.stage
        guard let leaf = stage.frames.leaves.first(where: { $0.path == path }) else { return insetStep() }
        // parity: capture-picture-zoomed
        stage.zoom(at: CGPoint(x: leaf.rect.midX, y: leaf.rect.midY), by: 2, eased: true)
        advance(1)
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
        let want = stage.renderedSource(of: path)
        let full = stage.pictureSizes[camera]
        let name = PictureNaming.sanitize(model.name(of: camera))
        saving(stage, { stage.performOnSelected(.saveZoomedArea) }) { [self] record in
            if let want, let full {
                checkSaved(record, "zoomed area", ext: "png", nameEnds: " " + name, size: want.size, tolerance: 3)
                if let r = record?.region, abs(r.minX - want.minX) > 1 || abs(r.minY - want.minY) > 1 {
                    problems.append("zoomed area: the region \(r) is not the one shown, \(want)")
                }
                if want.width >= full.width || want.height >= full.height {
                    problems.append("zoomed area: the region \(want.size) is not smaller than the picture \(full)")
                }
            } else {
                problems.append("zoomed area: the tile drew no source rectangle")
            }
            stage.resetZoom(at: path)
            advance(1)
            try? FileManager.default.removeItem(at: folder)
            insetStep()
        }
    }

    /// A fisheye tile's saved view is the dewarped picture at the source's
    /// resolution (its pixels match the fixture's colors where the reference
    /// mapping says), and Save Original Fisheye Picture is the circle as
    /// received.
    func fisheyePictureStep(_ path: NodePath, _ camera: CameraRef) {
        guard let model = app.model, let window else { return checkLog() }
        let stage = window.stage
        let folder = freshPictureFolder(model)
        let name = PictureNaming.sanitize(model.name(of: camera))
        stage.setFisheyeMode(.panorama, at: path)
        stage.grid.prepare?(stage.grid.renderer, stage.grid.drawableSize)
        window.window?.makeFirstResponder(stage)
        window.state.selected = path
        // parity: capture-picture-fisheye
        saving(stage, { stage.performOnSelected(.savePicture) }) { [self] record in
            let image = checkSaved(record, "fisheye view", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3)
            if record?.dewarped != true { problems.append("fisheye view: the saved picture was not dewarped") }
            if let image, let record,
                let dewarp = stage.fisheyeDewarp(
                    camera: camera, path: path, picture: record.picture,
                    tileSize: CGSize(width: image.width, height: image.height))
            {
                let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
                let (checked, matched) = Self.fisheyeMatches(dewarp, in: rect) { image.rgb(x: $0, y: $1) }
                print("fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                if checked < 20 || Double(matched) < 0.9 * Double(checked) {
                    problems.append("fisheye view: \(matched) of \(checked) saved pixels match the fixture")
                }
            }
            saving(stage, { stage.performOnSelected(.saveOriginalFisheye) }) { [self] original in
                let circle = checkSaved(original, "original fisheye", ext: "png", nameEnds: " " + name, size: nil, tolerance: 3)
                if original?.dewarped != false { problems.append("original fisheye: the saved picture was dewarped") }
                if let circle, let image, circle.width != image.width || circle.height != image.height {
                    problems.append("original fisheye: \(circle.width)x\(circle.height), not the source's size \(image.width)x\(image.height)")
                }
                try? FileManager.default.removeItem(at: folder)
                checkLog()
            }
        }
    }

    // MARK: Alert insets and doorbell calls

    func captureRealStep() {
        guard let model = app.model, let window,
            let device = model.devices.first(where: { $0.link.state == .connected && !$0.channels.isEmpty }),
            ["127.0.0.1", "localhost", "::1"].contains(device.config.host),
            let camera = window.stage.frames.leaves.compactMap(\.camera).first(where: { $0.device == device.key }),
            model.leases.tile(for: camera) != nil
        else { return checkLog() }
        // parity: capture-record-real-core
        model.capture.startLive(camera)
        guard let id = model.capture.board.live(of: camera)?.id else {
            problems.append("capture (real core): the live recording did not start (\(model.capture.lastRefusal ?? "?"))")
            return checkLog()
        }
        whenCapture(30, { ($0.running[id]?.durationMs ?? 0) >= Self.realRecordingMs }) { [self] in
            model.capture.stop(id)
            whenCapture(20, { $0.running[id] == nil }) { [self] in
                guard let end = model.capture.board.ended.last(where: { $0.recording == id }) else {
                    problems.append("capture (real core): the live recording did not end")
                    return checkLog()
                }
                inspectRecording(end) { [self] r in
                    if let r {
                        let aac = r.audio.first
                        print(
                            "capture (real core): live \((end.file as NSString).lastPathComponent), \(r.videoTracks) video, "
                                + "\(r.audio.count) audio (\(end.audio)), \(String(format: "%.2f", r.duration)) s for "
                                + "\(end.durationMs) ms, keyframe start \(r.startsOnKeyframe), AAC decoded \(aac?.decodedFrames ?? 0) frames")
                        if r.videoTracks != 1 || !r.startsOnKeyframe { problems.append("capture (real core): the live file's video is wrong") }
                        if abs(r.duration - Double(end.durationMs) / 1000) > 0.5 {
                            problems.append("capture (real core): the file lasts \(r.duration) s, the events said \(end.durationMs) ms")
                        }
                        if end.audio == "passthrough" && (r.audio.count != 1 || (aac?.decodedFrames ?? 0) == 0) {
                            problems.append("capture (real core): the camera's AAC is not in the file or does not decode")
                        }
                    }
                    captureRealCallStep(camera)
                }
            }
        }
    }

    func captureRealCallStep(_ camera: CameraRef) {
        guard let model = app.model else { return checkLog() }
        // parity: capture-call-record-real-core
        model.calls.startConversation(camera.firstLens, at: Date())
        model.capture.startCall(camera.firstLens)
        guard let id = model.capture.callRecording(camera)?.id else {
            problems.append("capture (real core): the call recording did not start (\(model.capture.lastRefusal ?? "?"))")
            model.calls.end(camera.firstLens, reason: "closed")
            return checkLog()
        }
        whenCapture(30, { ($0.running[id]?.durationMs ?? 0) >= Self.realRecordingMs }) { [self] in
            // The recording ends with the call.
            model.calls.end(camera.firstLens, reason: "closed")
            whenCapture(20, { $0.running[id] == nil }) { [self] in
                guard let end = model.capture.board.ended.last(where: { $0.recording == id }) else {
                    problems.append("capture (real core): the call recording did not end with the call")
                    return checkLog()
                }
                inspectRecording(end) { [self] r in
                    if let r {
                        let enabled = r.audio.map(\.enabled)
                        let decoded = r.audio.map(\.decodedFrames)
                        print(
                            "capture (real core): call \((end.file as NSString).lastPathComponent) ended \(end.state) "
                                + "(\(end.reason ?? "-")), tracks \(end.tracks), \(r.videoTracks) video, audio enabled \(enabled), "
                                + "decoded \(decoded) frames, peaks \(r.audio.map { String(format: "%.2f", $0.peak) })")
                        if end.state != "finished" || end.reason != "stopped" {
                            problems.append("capture (real core): the call recording ended \(end.state) (\(end.reason ?? "-"))")
                        }
                        if r.videoTracks != 1 || enabled != [true, false, false] {
                            problems.append("capture (real core): the call file's tracks are \(r.videoTracks) video, audio \(enabled)")
                        }
                        if decoded.count != 3 || decoded.contains(0) {
                            problems.append("capture (real core): a call track did not decode (\(decoded))")
                        }
                        if !r.startsOnKeyframe { problems.append("capture (real core): the call file does not start on a keyframe") }
                    }
                    checkLog()
                }
            }
        }
    }

    /// Reads a finished recording back (nil, and a problem, if it cannot).
    func inspectRecording(_ end: RecordingEvent, _ body: @escaping @MainActor (RecordingFileCheck?) -> Void) {
        let url = URL(fileURLWithPath: end.file)
        Task { @MainActor [self] in
            do {
                body(try await RecordingFileCheck.inspect(url))
            } catch {
                problems.append("capture (real core): \(url.lastPathComponent) could not be read (\(error))")
                body(nil)
            }
        }
    }
}
