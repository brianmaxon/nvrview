import AppKit
import NVRCore
import AppBase

// The tile header's actions (docs/UX.md "Tile header", docs/CAPTURE.md "The
// header model"): one place that turns a `TileHeaderItem` into what it
// does, for the header's buttons, the "…" menu, the right-click menu, the
// keys and the Camera menu alike. Menus are built from
// `TileHeaderLayout.menu(for:)`, so they have the header's order and
// wording.

extension StageView {
    /// What the header model needs to know about the tile at `path`
    /// showing `camera`: the camera's capabilities (the `channels` event)
    /// and the tile's state.
    func headerTile(_ camera: CameraRef, path: NodePath) -> TileHeaderTile? {
        let info = model.channelInfo(camera)
        let ptz = info?.ptz(lens: camera.lens) ?? .none
        let call = model.calls.call(for: camera.firstLens)
        // A grid tile of a popped-out call offers Pop Out too: it brings
        // the call's window forward. In the call window, Record is the
        // call's recording.
        let where_: TileHeaderCall = fixedTile ? .window : (call == nil ? .noCall : .inset)
        return TileHeaderTile(
            audio: true, talk: info?.talk(lens: camera.lens) == true, doorbell: info?.doorbell == true,
            panTilt: ptz.panTilt, presets: ptz.presets, lens: ptz.zoomFocus, fisheye: model.isFisheye(camera),
            picture: model.veil(for: camera) == nil,
            recording: fixedTile ? model.capture.callRecording(camera) != nil : model.capture.isRecorded(camera),
            zoomed: isZoomed(path), panned: isPanned(path),
            alert: alertActive(camera), listening: model.audible.contains(camera), conversation: call != nil,
            call: where_)
    }

    /// The tile the keys and the Camera menu act on: the focused one, the
    /// selected one, or the only one.
    var selectedPath: NodePath? {
        if let f = state.focused { return f }
        if let s = state.selected, state.tree[s]?.isLeaf == true { return s }
        return frames.leaves.count == 1 ? frames.leaves.first?.path : nil
    }

    /// The selected tile's header model (Camera menu titles).
    var selectedHeaderTile: TileHeaderTile? {
        guard let path = selectedPath, let camera = state.tree[path]?.camera else { return nil }
        return headerTile(camera, path: path)
    }

    /// A key or a Camera menu item for the selected tile.
    func performOnSelected(_ item: TileHeaderItem) {
        guard let path = selectedPath else { return }
        performHeaderItem(item, at: path, from: nil)
    }

    /// ⇧⌘R in a viewer window: the selected tile's call recording while
    /// its camera's call is open, else as R.
    func recordCallOnSelected() {
        guard let path = selectedPath, let camera = state.tree[path]?.camera else { return }
        guard !fixedTile, model.calls.call(for: camera.firstLens) != nil else { return performOnSelected(.record) }
        noteInteraction()
        model.capture.toggleCall(camera)
        refreshChrome()
    }

    /// Does what `item` does for the tile at `path`; `view` is the button
    /// clicked, under which a menu pops up (nil from a key or a menu).
    func performHeaderItem(_ item: TileHeaderItem, at path: NodePath, from view: NSView?) {
        guard let camera = state.tree[path]?.camera, let tile = headerTile(camera, path: path) else { return }
        noteInteraction()
        if TileActionStub.isPending(item) {
            let label = TileHeaderLayout.menu(for: tile).first { $0.item == item }?.label ?? "This"
            showToast(TileActionStub.message(label))
            return
        }
        switch item {
        case .speaker:
            let flags = NSEvent.modifierFlags
            model.toggleListening(camera, additive: view != nil && (flags.contains(.option) || flags.contains(.command)))
        case .savePicture:
            savePicture(at: path, kind: .view, ask: false)
        case .savePictureAs:
            savePicture(at: path, kind: .view, ask: true)
        case .saveZoomedArea:
            savePicture(at: path, kind: .zoomedArea, ask: false)
        case .saveOriginalFisheye:
            savePicture(at: path, kind: .originalFisheye, ask: false)
        case .record:
            if fixedTile { model.capture.toggleCall(camera) } else { model.capture.toggleLive(camera) }
        case .recordMain:
            model.capture.startLive(camera, quality: .main)
        case .recordSub:
            model.capture.startLive(camera, quality: .sub)
        case .dismissAlert:
            dismissAlert(camera)
        case .conversation:
            if model.calls.call(for: camera.firstLens) == nil { startConversation(camera) } else { endConversation(camera) }
        case .resetZoom, .resetPosition:
            resetZoom(at: path)
        case .fillTile:
            setFill(!fills(atTile: path), atTile: path)
        case .pop:
            if fixedTile { model.popInCall(camera.firstLens) } else { model.popOutCall(camera.firstLens) }
            return
        case .allowTalkBack:
            model.setAllowTalk(camera, !model.cameraOptions(camera.firstLens).allowTalk)
        case .entrance:
            model.setEntrance(camera, !model.cameraOptions(camera).entrance)
        case .panTilt:
            showPTZ(.pad, camera: camera, path: path, from: view)
            return
        case .lens:
            showPTZ(.lens, camera: camera, path: path, from: view)
            return
        case .presets:
            showPresets(camera: camera, path: path, from: view)
            return
        case .quality, .fisheye, .callsOpenIn, .more:
            if let menu = menu(for: item, at: path) { popUp(menu, under: view, path: path) }
        default:
            break
        }
        refreshChrome()
    }

    /// The menu a header button opens: Quality, Fisheye View, Calls open
    /// in, or "…" (the right-click menu without what the header shows).
    func menu(for item: TileHeaderItem, at path: NodePath) -> NSMenu? {
        guard let camera = state.tree[path]?.camera, let tile = headerTile(camera, path: path) else { return nil }
        if item == .more {
            let shown = Set(chromes[path]?.headerLayout?.visible ?? [])
            let menu = NSMenu()
            for entry in TileHeaderLayout.menu(for: tile) where !shown.contains(entry.item) {
                menu.addItem(menuItem(entry, camera: camera, path: path))
            }
            return menu
        }
        guard let entry = TileHeaderLayout.menu(for: tile).first(where: { $0.item == item }) else { return nil }
        return menuItem(entry, camera: camera, path: path).submenu
    }

    /// The "…" menu of the tile at `path` (self-test).
    func moreMenu(_ path: NodePath) -> NSMenu? { menu(for: .more, at: path) }

    func popUp(_ menu: NSMenu, under view: NSView?, path: NodePath) {
        if let view {
            menu.popUp(positioning: nil, at: NSPoint(x: 0, y: view.isFlipped ? view.bounds.height + 2 : -2), in: view)
        } else if let chrome = chromes[path] {
            menu.popUp(positioning: nil, at: NSPoint(x: chrome.bounds.width - 40, y: 30), in: chrome)
        }
    }

    /// One header menu entry as a menu item: its label, its key hint, and
    /// a submenu where it has one.
    func menuItem(_ entry: TileHeaderMenuEntry, camera: CameraRef, path: NodePath) -> NSMenuItem {
        let item: NSMenuItem
        switch entry.item {
        case .quality:
            item = qualityMenuItem(camera: camera, path: path)
        case .fisheye:
            item = fisheyeMenuItem(path: path)
            // The mount belongs with the view (it is per camera).
            if let sub = item.submenu, let mount = mountMenuItem(camera: camera).submenu {
                sub.addItem(.separator())
                sub.addItem(NSMenuItem.sectionHeader(title: "Mount"))
                for i in mount.items {
                    mount.removeItem(i)
                    sub.addItem(i)
                }
            }
        case .callsOpenIn:
            item = callsOpenInMenuItem(camera)
        case .presets:
            item = presetsMenuItem(camera: camera)
        default:
            let what = entry.item
            item = ClosureMenuItem(title: entry.label) { [weak self] in self?.performHeaderItem(what, at: path, from: nil) }
        }
        item.title = entry.label
        MenuKeyHint.apply(entry.macShortcut, to: item)
        switch entry.item {
        case .fillTile:
            item.state = fills(atTile: path) ? .on : .off
            item.toolTip = "Crop this tile's picture to fill it; drag the picture to choose the part shown"
        case .resetPosition:
            item.toolTip = "Show the middle of the filled picture again"
        case .entrance:
            item.state = model.cameraOptions(camera).entrance ? .on : .off
            item.toolTip = "A person on this camera opens an alert inset when it is not on screen"
        case .allowTalkBack:
            item.state = model.cameraOptions(camera.firstLens).allowTalk ? .on : .off
            item.toolTip = "Let Talk and typed replies speak through this camera (asked again for each camera)"
        case .conversation where model.calls.call(for: camera.firstLens) == nil:
            item.toolTip = "Open the call: captions of what the camera hears, Talk, and typed replies spoken through its speaker"
        default:
            if TileActionStub.isPending(entry.item) { item.toolTip = "Coming soon" }
        }
        return item
    }

    /// Calls open in ▸ Default, Window, Inset (per doorbell).
    private func callsOpenInMenuItem(_ camera: CameraRef) -> NSMenuItem {
        let current = model.cameraOptions(camera.firstLens).callPresentation
        let sub = NSMenu()
        let choices: [(String, CallPresentation?)] = [
            ("Default (\(model.callsOpenInWindow ? "window" : "inset"))", nil), ("Window", .window), ("Inset", .inset),
        ]
        for (title, value) in choices {
            let i = ClosureMenuItem(title: title) { [weak self] in self?.model.setCallPresentation(camera, value) }
            i.state = value == current ? .on : .off
            sub.addItem(i)
        }
        let item = NSMenuItem(title: "Calls open in", action: nil, keyEquivalent: "")
        item.submenu = sub
        return item
    }

    // MARK: The short message over the stage

    func installToast() {
        addSubview(toast)
    }

    /// Shows `text` over the stage for a few seconds (on the stage's clock).
    func showToast(_ text: String, action: (title: String, handler: () -> Void)? = nil) {
        toast.show(text, action: action)
        toast.place(in: bounds)
        addSubview(toast, positioned: .above, relativeTo: nil)
        toastHideAt = clock.now + (action == nil ? ToastView.duration : ToastView.actionDuration)
    }

    /// What the message says now, nil when none shows (self-test).
    var toastText: String? { toast.text }

    func tickToast(now: Double) {
        guard let at = toastHideAt, now >= at else { return }
        toastHideAt = nil
        toast.isHidden = true
    }
}
