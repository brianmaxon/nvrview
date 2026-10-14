import AppKit
import NVRCore
import AppPTZ

// Camera motion on the stage (docs/UX.md "Pan, tilt, and zoom", "Capture and
// camera keys"): the pad and lens popovers under the header buttons and the
// Camera menu's "Pan and Tilt…", the presets menu, and the Option keys.
// These are the user's own controls and need no enable; a move is held by a
// control (or a key), renewed by the model while it is held, and stopped on
// release, window close, tile close and focus loss.

extension StageView {
    /// Shows the problems the model reports as a line over the tile (the
    /// popover has its own line).
    package func installPTZ() {
        ptzObserver = model.ptz.addObserver { [weak self] change in
            guard let self, case .problem(let camera, let text) = change else { return }
            MainActor.assumeIsolated {
                guard frames.leaves.contains(where: { $0.camera == camera }),
                    ptzPopoverController?.camera != camera
                else { return }
                showToast("\(model.name(of: camera)): \(text)")
            }
        }
    }

    /// Opens the pad or the lens popover for the camera at `path`, under
    /// `view` (the header button) or the tile's header.
    package func showPTZ(_ mode: PTZPopoverController.Mode, camera: CameraRef, path: NodePath, from view: NSView?) {
        closePTZPopover()
        let controller = PTZPopoverController(
            camera: camera, mode: mode, name: model.name(of: camera), ptz: model.ptz)
        let popover = NSPopover()
        popover.behavior = .semitransient
        popover.contentViewController = controller
        popover.delegate = self
        ptzPopover = popover
        ptzPopoverController = controller
        guard let anchor = view ?? chromes[path] else { return }
        let edge: NSRectEdge = anchor.isFlipped ? .maxY : .minY
        let rect = view == nil ? CGRect(x: anchor.bounds.midX - 20, y: anchor.bounds.minY, width: 40, height: 30) : anchor.bounds
        popover.show(relativeTo: rect, of: anchor, preferredEdge: edge)
    }

    /// Closes the popover, and with it anything it holds.
    package func closePTZPopover() {
        guard let popover = ptzPopover else { return }
        popover.close()
        endPTZPopover()
    }

    fileprivate func endPTZPopover() {
        ptzPopoverController?.closed()
        ptzPopover = nil
        ptzPopoverController = nil
    }

    /// The presets menu of the camera at `path`, once its list is read.
    package func showPresets(camera: CameraRef, path: NodePath, from view: NSView?) {
        model.ptz.withInfo(camera) { [weak self] info in
            guard let self else { return }
            guard let info else {
                showToast("\(model.name(of: camera)): the presets could not be read")
                return
            }
            let menu = presetsMenu(info: info, camera: camera, ptz: model.ptz)
            popUp(menu, under: view, path: path)
        }
    }

    /// The right-click and Camera menus' Presets item, from the list read so
    /// far (reading it when it has not been).
    package func presetsMenuItem(camera: CameraRef) -> NSMenuItem {
        let item = NSMenuItem(title: "Presets", action: nil, keyEquivalent: "")
        if let info = model.ptz.info(for: camera) {
            item.submenu = presetsMenu(info: info, camera: camera, ptz: model.ptz)
        } else {
            let sub = NSMenu(title: "Presets")
            let reading = NSMenuItem(title: "Reading the presets…", action: nil, keyEquivalent: "")
            reading.isEnabled = false
            sub.addItem(reading)
            sub.autoenablesItems = false
            item.submenu = sub
            model.ptz.load(camera)
        }
        return item
    }

    // MARK: Keys (docs/UX.md "Capture and camera keys")

    /// What the selected (or focused) tile's camera can do with the keys.
    package var selectedPTZCapability: (panTilt: Bool, lens: Bool) {
        guard let path = selectedPath, let camera = state.tree[path]?.camera,
            let tile = headerTile(camera, path: path)
        else { return (false, false) }
        return (tile.panTilt, tile.lens)
    }

    /// A camera key for the selected tile's camera. False when the tile has
    /// no such control (the key goes on).
    package func handlePTZKey(_ key: PTZKey) -> Bool {
        guard let path = selectedPath, let camera = state.tree[path]?.camera,
            let tile = headerTile(camera, path: path)
        else { return false }
        noteInteraction()
        switch key {
        case .move(let direction):
            guard tile.panTilt else { return false }
            model.ptz.keyDown(direction, camera: camera)
        case .zoom(let direction):
            guard tile.lens else { return false }
            model.ptz.withInfo(camera) { [weak self] info in
                guard let self else { return }
                if info == nil { showToast("\(model.name(of: camera)): the lens could not be read") }
                model.ptz.zoomStep(direction, camera: camera)
            }
        case .preset(let n):
            guard tile.presets else { return false }
            model.ptz.withInfo(camera) { [weak self] info in
                guard let self, info != nil else { return }
                if !model.ptz.recallNth(n, camera: camera) {
                    showToast("\(model.name(of: camera)): there is no preset \(n)")
                }
            }
        }
        return true
    }

    /// An arrow key came up.
    package func handlePTZKeyUp(_ direction: PTZDirection) { model.ptz.keyUp(direction) }
}

extension StageView: NSPopoverDelegate {
    package func popoverDidClose(_ notification: Notification) { endPTZPopover() }
}
