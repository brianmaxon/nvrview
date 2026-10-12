import AppKit
import NVRCore

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
        let presets = item("Presets", nil)
        presets.submenu = {
            let m = NSMenu(title: "Presets")
            m.delegate = PresetsMenuDelegate.shared
            m.autoenablesItems = false
            return m
        }()
        let recordings = item("Recordings", nil)
        recordings.submenu = {
            let m = NSMenu(title: "Recordings")
            let none = NSMenuItem(title: "No recordings", action: nil, keyEquivalent: "")
            none.isEnabled = false
            m.addItem(none)
            m.autoenablesItems = false
            return m
        }()
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

/// The Camera menu's Presets submenu: the selected tile's camera's list,
/// read when the menu opens (recall only).
@MainActor
final class PresetsMenuDelegate: NSObject, NSMenuDelegate {
    static let shared = PresetsMenuDelegate()

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        menu.autoenablesItems = false
        func note(_ text: String) {
            let i = NSMenuItem(title: text, action: nil, keyEquivalent: "")
            i.isEnabled = false
            menu.addItem(i)
        }
        guard let stage = (NSApp.keyWindow?.windowController as? ViewerWindowController)?.stage,
            let path = stage.selectedPath, let camera = stage.state.tree[path]?.camera,
            stage.selectedHeaderTile?.presets == true
        else { return note("No camera with presets selected") }
        guard let info = stage.model.ptz.info(for: camera) else {
            stage.model.ptz.load(camera)
            return note("Reading the presets…")
        }
        for item in presetItems(info: info, camera: camera, ptz: stage.model.ptz) { menu.addItem(item) }
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
    @objc func captureRecord(_ sender: Any?) { stage.performOnSelected(.record) }
    @objc func panTiltItem(_ sender: Any?) { stage.performOnSelected(.panTilt) }
    @objc func stopAllRecordings(_ sender: Any?) {}
    @objc func openPicturesFolder(_ sender: Any?) { stage.openPicturesFolder() }
    @objc func openRecordingsFolder(_ sender: Any?) { stage.showToast(TileActionStub.message("Open Recordings Folder")) }

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
        case #selector(conversationKey(_:)):
            item.title = tile?.conversation == true ? "End Conversation" : "Start Conversation"
            return tile != nil
        case #selector(panTiltItem(_:)):
            return tile?.panTilt == true
        case #selector(stopAllRecordings(_:)):
            return false
        case #selector(openPicturesFolder(_:)), #selector(openRecordingsFolder(_:)):
            return true
        default:
            return nil
        }
    }
}
