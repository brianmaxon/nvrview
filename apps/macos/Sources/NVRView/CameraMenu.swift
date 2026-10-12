import AppKit
import NVRCore

/// The Camera menu's Recordings submenu: the running recordings, each
/// with Stop, rebuilt each time it opens.
@MainActor
final class RecordingsMenu: NSObject, NSMenuDelegate {
    static let shared = RecordingsMenu()
    /// The model whose recordings it lists (set by the app).
    weak var model: AppModel?

    static func make() -> NSMenu {
        let m = NSMenu(title: "Recordings")
        m.delegate = shared
        shared.fill(m)
        return m
    }

    func menuNeedsUpdate(_ menu: NSMenu) { fill(menu) }

    /// The items for the recordings running now.
    func fill(_ menu: NSMenu) {
        menu.removeAllItems()
        let running = model?.capture.board.list ?? []
        guard let model, !running.isEmpty else {
            let none = NSMenuItem(title: "No recordings", action: nil, keyEquivalent: "")
            none.isEnabled = false
            menu.addItem(none)
            return
        }
        for r in running {
            let name = r.camera.map { model.name(of: $0) } ?? (r.file as NSString).lastPathComponent
            let what = r.isCall ? "Call · " : ""
            let item = NSMenuItem(
                title: "Stop \(what)\(name) (\(RecordingText.elapsed(ms: r.durationMs)))",
                action: #selector(ViewerWindowController.stopRecordingItem(_:)), keyEquivalent: "")
            item.tag = Int(truncatingIfNeeded: r.id)
            item.representedObject = NSNumber(value: r.id)
            menu.addItem(item)
        }
    }
}

/// The Camera menu (docs/UX.md "Capture and camera keys"): the selected
/// tile's capture and camera actions with their keys. A viewer window takes
/// the single keys (P, ⇧P, R, T) before the menus do, as for the View
/// menu's; ⇧⌘P and ⇧⌘R, hidden here, also work in a call window, where P
/// may be typed into the reply. Actions that are not built yet say so
/// (`TileActionStub`).
@MainActor
enum CameraMenu {
    static func make() -> NSMenuItem {
        func item(_ title: String, _ action: Selector?, _ key: String = "", _ mods: NSEvent.ModifierFlags = [])
            -> NSMenuItem
        {
            let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
            i.keyEquivalentModifierMask = mods
            return i
        }
        func hidden(_ i: NSMenuItem) -> NSMenuItem {
            i.isHidden = true
            i.allowsKeyEquivalentWhenHidden = true
            return i
        }
        func placeholder(_ title: String) -> NSMenu {
            let m = NSMenu(title: title)
            let none = NSMenuItem(title: "Coming soon", action: nil, keyEquivalent: "")
            none.isEnabled = false
            m.addItem(none)
            m.autoenablesItems = false
            return m
        }
        let presets = item("Presets", nil)
        presets.submenu = placeholder("Presets")
        let recordings = item("Recordings", nil)
        recordings.submenu = RecordingsMenu.make()
        let menu = NSMenu(title: "Camera")
        for i in [
            item("Save Picture", #selector(ViewerWindowController.savePictureKey(_:)), "p"),
            hidden(item("Save Picture", #selector(ViewerWindowController.captureSavePicture(_:)), "p", [.command, .shift])),
            item("Save Picture As…", #selector(ViewerWindowController.savePictureAsKey(_:)), "p", [.shift]),
            item("Start Recording", #selector(ViewerWindowController.recordKey(_:)), "r"),
            hidden(item("Record Call", #selector(ViewerWindowController.captureRecord(_:)), "r", [.command, .shift])),
            item("Start Conversation", #selector(ViewerWindowController.conversationKey(_:)), "t"),
            .separator(),
            item("Pan and Tilt…", #selector(ViewerWindowController.panTiltItem(_:))),
            presets,
            .separator(),
            recordings,
            item("Stop All Recordings", #selector(ViewerWindowController.stopAllRecordings(_:))),
            .separator(),
            item("Open Pictures Folder", #selector(ViewerWindowController.openPicturesFolder(_:))),
            item("Open Recordings Folder", #selector(ViewerWindowController.openRecordingsFolder(_:))),
        ] {
            menu.addItem(i)
        }
        let parent = NSMenuItem(title: "Camera", action: nil, keyEquivalent: "")
        parent.submenu = menu
        return parent
    }
}

extension ViewerWindowController {
    // Pressed single keys never reach these (the window takes them first).
    @objc func savePictureKey(_ sender: Any?) { perform(.savePicture) }
    @objc func savePictureAsKey(_ sender: Any?) { perform(.savePictureAs) }
    @objc func recordKey(_ sender: Any?) { perform(.toggleRecording) }
    @objc func conversationKey(_ sender: Any?) { perform(.toggleConversation) }
    /// ⇧⌘P and ⇧⌘R: accepted in viewer windows too, for the selected tile.
    @objc func captureSavePicture(_ sender: Any?) { stage.performOnSelected(.savePicture) }
    @objc func captureRecord(_ sender: Any?) { stage.recordCallOnSelected() }
    @objc func panTiltItem(_ sender: Any?) { stage.performOnSelected(.panTilt) }
    @objc func stopAllRecordings(_ sender: Any?) { model.capture.stopAll() }
    @objc func openPicturesFolder(_ sender: Any?) { stage.openPicturesFolder() }
    @objc func openRecordingsFolder(_ sender: Any?) { model.capture.openFolder() }

    /// A recording's Stop in the Recordings submenu.
    @objc func stopRecordingItem(_ sender: Any?) {
        guard let id = ((sender as? NSMenuItem)?.representedObject as? NSNumber)?.uint64Value else { return }
        model.capture.stop(id)
    }

    /// The Camera menu's items for the selected tile (nil: not one of them).
    func validateCameraItem(_ item: NSMenuItem) -> Bool? {
        let tile = stage.selectedHeaderTile
        switch item.action {
        case #selector(savePictureKey(_:)), #selector(savePictureAsKey(_:)), #selector(captureSavePicture(_:)):
            return tile?.picture == true
        case #selector(recordKey(_:)):
            item.title = tile?.recording == true ? "Stop Recording" : "Start Recording"
            return tile != nil
        case #selector(captureRecord(_:)):
            return tile != nil
        case #selector(stopRecordingItem(_:)):
            return true
        case #selector(conversationKey(_:)):
            item.title = tile?.conversation == true ? "End Conversation" : "Start Conversation"
            return tile != nil
        case #selector(panTiltItem(_:)):
            return tile?.panTilt == true
        case #selector(stopAllRecordings(_:)):
            return !model.capture.board.running.isEmpty
        case #selector(openPicturesFolder(_:)), #selector(openRecordingsFolder(_:)):
            return true
        default:
            return nil
        }
    }
}
