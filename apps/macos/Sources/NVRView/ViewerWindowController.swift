import AppKit
import NVRCore
import Observation
import SwiftUI
import VideoKit

/// What the sidebar of one window needs to know about that window.
@MainActor
@Observable
final class SidebarState {
    var current: WindowContent?
    var editing = false
    var hasSelection = false
}

/// One viewer window: the sidebar (devices, layouts, groups) and the stage
/// showing one layout, group or scratch arrangement. Every window has its
/// own layout and may show any camera, including ones other windows show
/// (the leases stream each camera once).
@MainActor
final class ViewerWindowController: NSWindowController, NSWindowDelegate, StageDelegate, NSMenuItemValidation,
    NSToolbarDelegate
{
    let model: AppModel
    let state: WindowState
    let stage: StageView
    let sidebar = SidebarState()
    var onClose: (ViewerWindowController) -> Void = { _ in }
    /// What the window shows or where it is changed (to remember it).
    var onChange: () -> Void = {}
    /// The frame before entering full screen, while in full screen.
    private var windowedFrame: NSRect?

    static let minSize = CGSize(width: 640, height: 400)

    private var held: [CameraRef: Int] = [:]
    /// Set while this window shows an unedited All Cameras grid.
    private var allCamerasWatch: AllCamerasWatch?
    private var token: UUID?
    private var timer: Timer?
    private let editBar: EditBar
    private let editItem = NSToolbarItem.Identifier("edit")
    private let fullScreenItem = NSToolbarItem.Identifier("fullscreen")
    /// "● N recording" while recordings run (docs/CAPTURE.md "Indicators").
    private let recordingItem = NSToolbarItem.Identifier("recording")
    private weak var editButton: NSButton?
    private weak var recordingToolbarItem: NSToolbarItem?
    private let split = ViewerSplitViewController()
    private var sidebarItem: NSSplitViewItem!
    private var sidebarHost: NSView?
    /// The user's choice for the sidebar, saved with the window. Full
    /// screen closes and reopens the sidebar on its own; that never changes
    /// this.
    private var sidebarCollapsedPreference = false
    /// Set while this controller itself collapses or expands the sidebar.
    private var changingSidebar = false
    private var sidebarObservation: NSKeyValueObservation?
    /// The edge reveal over the stage while the sidebar is collapsed.
    private let overlay: SidebarOverlay
    private let overlayHost: NSHostingView<SidebarView>

    init(model: AppModel, content: WindowContent, renderer: GridRenderer) {
        self.model = model
        state = WindowState(content: content, name: "", tree: .empty)
        stage = StageView(model: model, state: state, renderer: renderer)
        editBar = EditBar()
        overlayHost = NSHostingView(rootView: SidebarView(model: model, window: nil, state: sidebar))
        overlayHost.sizingOptions = []
        overlay = SidebarOverlay(content: overlayHost)

        let window = ViewerWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1180, height: 700),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        window.minSize = Self.minSize
        window.collectionBehavior = [.fullScreenPrimary, .managed]
        window.titlebarSeparatorStyle = .automatic
        window.tabbingMode = .disallowed
        window.acceptsMouseMovedEvents = true
        super.init(window: window)
        window.delegate = self
        window.onEvent = { [weak self] event in self?.windowEvent(event) ?? false }

        let sidebarHost = NSHostingController(rootView: SidebarView(model: model, window: self, state: sidebar))
        self.sidebarHost = sidebarHost.view
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebarHost)
        self.sidebarItem = sidebarItem
        sidebarItem.minimumThickness = 200
        sidebarItem.maximumThickness = 320
        sidebarItem.canCollapse = true
        split.addSplitViewItem(sidebarItem)
        let stageVC = StageContainerController(stage: stage, editBar: editBar, overlay: overlay)
        split.addSplitViewItem(NSSplitViewItem(viewController: stageVC))
        window.contentViewController = split
        window.setContentSize(NSSize(width: 1180, height: 700))

        let toolbar = NSToolbar(identifier: "viewer")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar
        window.toolbarStyle = .unified

        overlayHost.rootView = SidebarView(model: model, window: self, state: sidebar)
        overlay.onPin = { [weak self] in self?.dockSidebar() }
        // The edge reveal's dwell and hide delay count on the viewer's clock;
        // a manual one (the self-test) is moved by whoever calls `tick`.
        let clock = model.clock
        overlay.clock = { clock.now }
        overlay.armsTimer = !(clock is ManualViewerClock)
        overlay.isEditing = { [weak self] in self?.overlayIsEditing ?? false }
        sidebarObservation = sidebarItem.observe(\.isCollapsed, options: [.new]) { [weak self] _, _ in
            MainActor.assumeIsolated { self?.sidebarCollapsedChanged() }
        }
        stage.delegate = self
        stage.onInsetAnchorChange = { [weak self] in self?.onChange() }
        stage.onViewsChanged = { [weak self] in self?.onChange() }
        editBar.controller = self
        token = model.subscribe { [weak self] in self?.modelChanged() }
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            // Countdowns and statuses that change without an event, and the
            // stage's time step (the chrome hides about 3 s after the last
            // input), also while the window gets no display refreshes.
            MainActor.assumeIsolated {
                self?.stage.refreshChrome()
                self?.stage.tick()
                self?.overlay.tick()
            }
        }
        show(content)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // MARK: Content

    /// Shows a layout, group or scratch arrangement in this window.
    func show(_ content: WindowContent) {
        stage.cancelFocus()
        state.content = content
        state.selected = nil
        switch content {
        case .layout(let id):
            guard let l = model.layout(id) else { return show(Self.allCameras(model)) }
            state.name = l.name
            state.tree = l.root
        case .group(let id):
            guard let g = model.group(id) else { return show(Self.allCameras(model)) }
            state.name = g.name
            state.tree = LayoutNode.balanced(g.cameras)
        case .scratch(let name, let root):
            state.name = name
            state.tree = root
        }
        // An All Cameras grid follows the devices until it is edited.
        if case .scratch(AllCameras.name, _) = content {
            allCamerasWatch = AllCamerasWatch(shown: state.tree.cameraList.compactMap(\.self))
        } else {
            allCamerasWatch = nil
        }
        sidebar.current = content
        stage.reload()
        syncLeases()
        updateTitle()
        stageDidChangeState(stage)
        onChange()
    }

    /// Every online camera of every connected device (up to the layout
    /// limit) as an even grid, unsaved.
    static func allCameras(_ model: AppModel) -> WindowContent {
        .scratch(name: AllCameras.name, root: LayoutNode.balanced(model.onlineCameras))
    }

    /// The window's cameras, leased so each is streamed once however many
    /// tiles and windows show it.
    private func syncLeases() {
        var wanted: [CameraRef: Int] = [:]
        for c in state.tree.cameraList.compactMap(\.self) { wanted[c, default: 0] += 1 }
        model.leases.update(from: held, to: wanted)
        held = wanted
    }

    private func updateTitle() {
        guard let window else { return }
        window.title = state.name
        let count = state.tree.cameras.count
        var subtitle = "\(count) camera\(count == 1 ? "" : "s") · sub-streams, full quality on focus"
        switch state.content {
        case .layout:
            if !model.layoutsStored { subtitle += " · not stored (this core has no layouts store)" }
        case .group: subtitle += " · group"
        case .scratch: subtitle += " · unsaved"
        }
        window.subtitle = subtitle
        editBar.update(state: state, model: model)
    }

    private func modelChanged() {
        updateRecordingItem()
        switch state.content {
        case .layout(let id):
            if let l = model.layout(id) {
                // Another window (or app) changed it: follow, unless this
                // window is the one that just stored the same tree, or a
                // drag here is still changing it (the model notifies on
                // every statistics event; following the stored tree then
                // snapped a divider back mid-drag). The drag stores its
                // result when it ends.
                if !stage.isInteracting, l.root.withTileIDs != state.tree || l.name != state.name {
                    state.tree = l.root
                    state.name = l.name
                    stage.reload()
                    syncLeases()
                    updateTitle()
                }
            } else {
                // Deleted elsewhere: keep what is on screen, unsaved.
                state.rename(to: .scratch(name: state.name, root: state.tree))
                sidebar.current = state.content
                updateTitle()
            }
        case .scratch:
            // An unedited All Cameras window follows devices connecting and
            // channels going on and offline. Rebuilding keeps the cameras
            // that stay (leases are shared per camera, so their streams do
            // not restart). Not while a tile is focused: it retries on the
            // next change.
            if var watch = allCamerasWatch, !state.editing, !stage.isFocused {
                if watch.refresh(to: model.onlineCameras) {
                    show(Self.allCameras(model))  // makes a fresh watch
                } else {
                    allCamerasWatch = watch
                }
            }
        case .group:
            break
        }
        stage.refreshChrome()
    }

    // MARK: StageDelegate

    func stage(_ stage: StageView, didEdit tree: LayoutNode) {
        allCamerasWatch?.markEdited()
        syncLeases()
        switch state.content {
        case .layout(let id):
            var l = model.layout(id) ?? SavedLayout(id: id, name: state.name, root: tree)
            l.root = tree
            l.name = state.name
            model.saveLayout(l)
        case .group, .scratch:
            state.rename(to: .scratch(name: state.name, root: tree))
            sidebar.current = state.content
            onChange()
        }
        updateTitle()
    }

    func stageDidChangeState(_ stage: StageView) {
        sidebar.hasSelection = state.selected != nil
        sidebar.editing = state.editing
        editBar.update(state: state, model: model)
    }

    // MARK: Sidebar actions

    /// A camera picked in the sidebar: into the selected tile (or the only
    /// one), else into the first empty tile, else shown alone.
    func cameraPicked(_ camera: CameraRef) {
        if let path = state.selected ?? (state.tree.leafCount == 1 ? [] : nil) {
            stage.assign(camera, at: path)
        } else if let empty = state.tree.leaves.first(where: { $0.camera == nil })?.path {
            state.selected = empty
            stage.assign(camera, at: empty)
        } else {
            show(.scratch(name: model.name(of: camera), root: .leaf(camera)))
        }
    }

    /// Asks for a name in a sheet on this window.
    func askName(_ title: String, initial: String, completion: @escaping (String) -> Void) {
        guard let window else { return }
        let alert = NSAlert()
        alert.messageText = title
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        let field = NSTextField(string: initial)
        field.frame = NSRect(x: 0, y: 0, width: 260, height: 24)
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        alert.beginSheetModal(for: window) { response in
            let name = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if response == .alertFirstButtonReturn, !name.isEmpty { completion(name) }
        }
    }

    // MARK: Actions

    @objc func toggleEditing(_ sender: Any?) {
        if stage.isFocused { stage.unfocus() }
        state.editing.toggle()
        if state.editing { allCamerasWatch?.markEdited() }
        if state.editing, state.selected == nil { state.selected = state.tree.leaves.first?.path }
        stage.reload()
        editButton?.title = state.editing ? "Done" : "Edit Layout"
        stageDidChangeState(stage)
        updateTitle()
    }

    @objc func splitSideBySide(_ sender: Any?) { stage.split(.row) }
    @objc func splitStacked(_ sender: Any?) { stage.split(.col) }
    @objc func removeTile(_ sender: Any?) { stage.removeSelected() }
    @objc func toggleFocus(_ sender: Any?) { stage.toggleFocus() }

    // MARK: Keyboard (docs/UX.md "Keyboard")

    /// Every event of this window passes here first. Interactions keep the
    /// chrome up; single-key shortcuts are taken before the focused view or
    /// the menus see them, except while text is being edited (and the
    /// sidebar keeps its own arrows, Space and Return). Returns true when
    /// the event was used up.
    private func windowEvent(_ event: NSEvent) -> Bool {
        switch event.type {
        case .mouseMoved, .leftMouseDown, .rightMouseDown, .otherMouseDown, .leftMouseDragged, .scrollWheel,
            .magnify, .keyDown:
            stage.noteInteraction()
        default:
            break
        }
        // Tab and Esc go to the alert insets first, when there are any.
        if event.type == .keyDown, let key = insetKey(for: event), stage.handleInsetKey(key) { return true }
        guard event.type == .keyDown, let shortcut = shortcut(for: event) else { return false }
        perform(shortcut)
        return true
    }

    /// Where the keyboard focus is, for the shortcuts.
    var keyContext: KeyContext {
        guard let responder = window?.firstResponder else { return .stage }
        // The field editor of a text field, or any text view.
        if responder is NSText { return .textEditing }
        if let view = responder as? NSView {
            if let sidebarHost, view.isDescendant(of: sidebarHost) { return .sidebar }
            if view.isDescendant(of: overlayHost) { return .sidebar }
        }
        return .stage
    }

    func shortcut(for event: NSEvent) -> ViewerShortcut? {
        var modifiers: KeyModifiers = []
        let flags = event.modifierFlags
        if flags.contains(.shift) { modifiers.insert(.shift) }
        if flags.contains(.control) { modifiers.insert(.control) }
        if flags.contains(.option) { modifiers.insert(.option) }
        if flags.contains(.command) { modifiers.insert(.command) }
        return ViewerKeys.shortcut(
            keyCode: event.keyCode, characters: event.charactersIgnoringModifiers, modifiers: modifiers,
            isRepeat: event.isARepeat, context: keyContext)
    }

    func insetKey(for event: NSEvent) -> InsetKey? {
        let flags = event.modifierFlags
        var modifiers: KeyModifiers = []
        if flags.contains(.shift) { modifiers.insert(.shift) }
        if flags.contains(.control) { modifiers.insert(.control) }
        if flags.contains(.option) { modifiers.insert(.option) }
        if flags.contains(.command) { modifiers.insert(.command) }
        return ViewerKeys.insetKey(keyCode: event.keyCode, modifiers: modifiers, isRepeat: event.isARepeat, context: keyContext)
    }

    /// A text field in the overlay is being edited.
    private var overlayIsEditing: Bool {
        guard let text = window?.firstResponder as? NSText, let view = text.delegate as? NSView else { return false }
        return view.isDescendant(of: overlayHost)
    }

    /// The overlay's pin: docks the sidebar (the stage resizes as usual).
    private func dockSidebar() {
        overlay.dismissNow()
        sidebarItem.animator().isCollapsed = false
    }

    private func sidebarCollapsedChanged() {
        let collapsed = sidebarItem.isCollapsed
        overlay.setCollapsed(collapsed)
        guard !changingSidebar else { return }
        sidebarCollapsedPreference = collapsed
        onChange()
    }

    var sidebarOverlay: SidebarOverlay { overlay }

    /// The self-test collapses and docks the sidebar like the user would
    /// (without the animation, so it need not wait for it).
    func setSidebarCollapsedForTest(_ collapsed: Bool) {
        sidebarItem.isCollapsed = collapsed
        window?.contentView?.layoutSubtreeIfNeeded()
    }

    /// The split view's arranged views (sidebar, stage) in window
    /// coordinates, for the self-test to see that nothing moved.
    var splitFramesForTest: [CGRect] {
        split.splitView.arrangedSubviews.map { $0.convert($0.bounds, to: nil) }
    }

    /// Whether AppKit would peek the collapsed sidebar now (nil where AppKit
    /// has no such question to ask). See `ViewerSplitViewController`.
    var systemSidebarPeekAllowedForTest: Bool? {
        ViewerSplitViewController.systemPeekAllowed(split.splitView)
    }

    /// Rests the pointer, as far as AppKit's own collapsed-sidebar hover is
    /// concerned, `x` points from the window's leading edge (`inside`), or
    /// takes it away. Returns false when this AppKit has no such hover.
    @discardableResult
    func hoverCollapsedSidebarEdgeForTest(x: CGFloat, inside: Bool) -> Bool {
        ViewerSplitViewController.hoverCollapsedEdge(split.splitView, x: x, inside: inside)
    }

    /// Applies the saved choice when a window is reopened.
    func restoreSidebar(collapsed: Bool) {
        sidebarCollapsedPreference = collapsed
        if sidebarItem.isCollapsed != collapsed {
            changingSidebar = true
            sidebarItem.isCollapsed = collapsed
            changingSidebar = false
        }
        overlay.setCollapsed(sidebarItem.isCollapsed)
    }

    /// Collapses or expands the sidebar without changing the saved choice.
    private func setSidebarTransiently(collapsed: Bool) {
        guard sidebarItem.isCollapsed != collapsed else { return }
        changingSidebar = true
        sidebarItem.isCollapsed = collapsed
        changingSidebar = false
    }

    func perform(_ shortcut: ViewerShortcut) {
        switch shortcut {
        case .toggleFullScreen: window?.toggleFullScreen(nil)
        case .toggleSidebar: split.toggleSidebar(nil)
        case .move(let direction): stage.moveSelection(direction)
        case .toggleFocus: stage.toggleFocus()
        case .resetZoom: stage.resetSelectedZoom()
        case .toggleListening: toggleListening(nil)
        case .confirm: stage.confirm()
        case .savePicture: stage.performOnSelected(.savePicture)
        case .savePictureAs: stage.performOnSelected(.savePictureAs)
        case .toggleRecording: stage.performOnSelected(.record)
        case .toggleConversation: stage.performOnSelected(.conversation)
        }
    }

    // Menu items listing the single keys (main.swift). Pressed keys never
    // reach these: the window takes them first (see `windowEvent`).
    @objc func fullScreenKey(_ sender: Any?) { perform(.toggleFullScreen) }
    @objc func sidebarKey(_ sender: Any?) { perform(.toggleSidebar) }
    @objc func resetZoomKey(_ sender: Any?) { perform(.resetZoom) }
    @objc func listenKey(_ sender: Any?) { perform(.toggleListening) }
    @objc func selectLeft(_ sender: Any?) { perform(.move(.left)) }
    @objc func selectRight(_ sender: Any?) { perform(.move(.right)) }
    @objc func selectUp(_ sender: Any?) { perform(.move(.up)) }
    @objc func selectDown(_ sender: Any?) { perform(.move(.down)) }

    @objc func toggleListening(_ sender: Any?) {
        guard let camera = stage.selectedCamera else { return }
        model.toggleListening(camera, additive: false)
    }

    @objc func stopListening(_ sender: Any?) { model.stopListening() }

    /// Fill Tiles: every tile of the window fills, or fits (a tile's own
    /// Fill Tile choice is cleared). Mixed in the menu while tiles differ.
    @objc func toggleFill(_ sender: Any?) {
        stage.setWindowFill(!state.fill)
        stage.reload()
    }

    @objc func saveAsLayout(_ sender: Any?) {
        askName("Save as Layout", initial: model.uniqueLayoutName(state.name)) { [weak self] name in
            guard let self, let id = model.createLayout(name: name, root: state.tree) else { return }
            // The tiles keep their views in the saved layout.
            state.views.rename(layout: state.viewsKey, to: TileKey.layoutKey(layout: id))
            show(.layout(id: id))
        }
    }

    @objc func newLayout(_ sender: Any?) {
        askName("New Layout", initial: model.uniqueLayoutName("Layout")) { [weak self] name in
            guard let self else { return }
            let first = state.tree.cameraList.compactMap(\.self)
            guard let id = model.createLayout(name: name, root: LayoutNode.Preset.twoByTwo.tree(cameras: first)) else {
                return
            }
            show(.layout(id: id))
            if !state.editing { toggleEditing(nil) }
        }
    }

    /// A new layout in a template's shape, filled with the online cameras in
    /// All Cameras order (empty tiles when there are fewer), opened for editing.
    func newLayout(from template: LayoutTemplate) {
        askName("New Layout", initial: model.uniqueLayoutName("Layout")) { [weak self] name in
            guard let self else { return }
            let root = template.tree(cameras: model.onlineCameras)
            guard let id = model.createLayout(name: name, root: root) else { return }
            show(.layout(id: id))
            if !state.editing { toggleEditing(nil) }
        }
    }

    @objc func newLayoutFromTemplate(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let template = LayoutTemplate(rawValue: raw) else { return }
        newLayout(from: template)
    }

    func applyPreset(_ preset: LayoutNode.Preset) {
        let cameras = state.tree.cameraList.compactMap(\.self)
        state.selected = nil
        stage.edit { _ in preset.tree(cameras: cameras) }
    }

    func rename(to name: String) {
        guard !name.isEmpty, name != state.name else { return }
        state.name = name
        switch state.content {
        case .layout(let id): model.renameLayout(id, to: name)
        case .group, .scratch:
            state.rename(to: .scratch(name: name, root: state.tree))
            onChange()
        }
        updateTitle()
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        // A single key that reached the menus from another window (Settings,
        // say) is not for this one.
        if item.keyEquivalentModifierMask.isDisjoint(with: [.command, .control, .option]), !item.keyEquivalent.isEmpty,
            NSApp.currentEvent?.type == .keyDown, NSApp.keyWindow !== window
        {
            return false
        }
        if let camera = validateCameraItem(item) { return camera }
        switch item.action {
        case #selector(toggleEditing(_:)):
            item.title = state.editing ? "Done Editing" : "Edit Layout"
            return true
        case #selector(splitSideBySide(_:)), #selector(splitStacked(_:)):
            return state.selected != nil || state.tree.leafCount == 1
        case #selector(removeTile(_:)):
            return state.selected != nil
        case #selector(toggleFocus(_:)):
            item.title = stage.isFocused ? "Leave Focus" : "Focus Tile"
            return stage.isFocused || state.selected != nil || state.tree.leafCount == 1
        case #selector(toggleListening(_:)):
            let camera = stage.selectedCamera
            item.state = camera.map { model.audible.contains($0) } == true ? .on : .off
            return camera != nil
        case #selector(stopListening(_:)):
            return !model.audible.isEmpty
        case #selector(toggleFill(_:)):
            item.state = stage.fillMixed ? .mixed : state.fill ? .on : .off
            return true
        case #selector(fullScreenKey(_:)):
            item.title = window?.styleMask.contains(.fullScreen) == true ? "Exit Full Screen" : "Enter Full Screen"
            return true
        case #selector(sidebarKey(_:)):
            item.title = sidebarItem.isCollapsed ? "Show Sidebar" : "Hide Sidebar"
            return true
        case #selector(resetZoomKey(_:)):
            return stage.selectedPath.map { stage.zoom(atTile: $0).isZoomed } == true
        case #selector(selectLeft(_:)), #selector(selectRight(_:)), #selector(selectUp(_:)), #selector(selectDown(_:)):
            return state.tree.leafCount > 1
        default:
            return true
        }
    }

    // MARK: Window

    /// What to reopen on the next launch.
    var savedWindow: SavedWindow? {
        guard let window else { return nil }
        let content: SavedWindow.Content
        switch state.content {
        case .layout(let id): content = .layout(id: id)
        case .group(let id): content = .group(id: id)
        case .scratch(let name, let root): content = .scratch(name: name, root: root)
        }
        let fullScreen = window.styleMask.contains(.fullScreen)
        return SavedWindow(
            content: content, frame: (fullScreen ? windowedFrame : nil) ?? window.frame, fullScreen: fullScreen,
            sidebarCollapsed: sidebarCollapsedPreference, insetAnchor: stage.insetAnchor, fill: state.views.fill,
            tileViews: state.views.savedEntries)
    }

    /// Puts the alert insets where the user left them (a restored window).
    func restoreInsetAnchor(_ anchor: InsetAnchor?) { stage.insetAnchor = anchor }

    /// Puts back the window's Fill Tiles and each tile's zoom, crop and Fill
    /// choice (a restored window). A file from before views were per tile
    /// has them per camera (`legacy`): each tile showing one of those
    /// cameras gets its view, and the window is saved in the new form.
    func restoreViews(_ views: TileViews<TileKey>, legacy: [LegacyTileView] = []) {
        var views = views
        if !legacy.isEmpty, views.count == 0 {
            views.migrate(legacy, layout: state.viewsKey, tree: state.tree)
        }
        views.prune(layout: state.viewsKey, tree: state.tree)
        state.views = views
        stage.reload()
        if !legacy.isEmpty { onChange() }
    }

    func windowDidMove(_ notification: Notification) { onChange() }
    func windowDidEndLiveResize(_ notification: Notification) { onChange() }
    // Full screen (docs/UX.md "Full screen chrome"): the system's own
    // transition; the sidebar closes on the way in (the stage is resized
    // by the transition anyway) and reopens on the way out; the toolbar
    // and menu bar hide and slide back in at the top edge; the stage's
    // overlays and the pointer hide after a few idle seconds. Nothing
    // resizes the video while in full screen except the user's own S.
    func windowWillEnterFullScreen(_ notification: Notification) {
        windowedFrame = window?.frame
        overlay.dismissNow()
        setSidebarTransiently(collapsed: true)
        stage.fullScreen = true
    }

    func windowDidEnterFullScreen(_ notification: Notification) { onChange() }

    func windowWillExitFullScreen(_ notification: Notification) {
        stage.fullScreen = false
        overlay.dismissNow()
        setSidebarTransiently(collapsed: sidebarCollapsedPreference)
    }

    func windowDidExitFullScreen(_ notification: Notification) {
        windowedFrame = nil
        onChange()
    }

    func window(
        _ window: NSWindow, willUseFullScreenPresentationOptions proposed: NSApplication.PresentationOptions = []
    ) -> NSApplication.PresentationOptions {
        proposed.union([.fullScreen, .autoHideMenuBar, .autoHideToolbar])
    }

    func windowWillClose(_ notification: Notification) {
        timer?.invalidate()
        sidebarObservation = nil
        if let token { model.unsubscribe(token) }
        stage.tearDown()
        model.leases.update(from: held, to: [:])
        held = [:]
        onClose(self)
    }

    // MARK: Toolbar

    /// The toolbar's recordings item: shown while recordings run.
    private func updateRecordingItem() {
        guard let item = recordingToolbarItem else { return }
        let count = model.capture.board.running.count
        item.isHidden = count == 0
        let title = "● \(count) recording"
        if let button = item.view as? NSButton, button.title != title { button.title = title }
    }

    /// The toolbar item's text while it shows (self-test).
    var recordingItemTitle: String? {
        guard let item = recordingToolbarItem, !item.isHidden else { return nil }
        return (item.view as? NSButton)?.title
    }

    @objc private func recordingItemClicked(_ sender: NSButton) {
        let menu = NSMenu()
        RecordingsMenu.shared.fill(menu)
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Stop All Recordings", action: #selector(stopAllRecordings(_:)), keyEquivalent: ""))
        menu.addItem(NSMenuItem(title: "Open Recordings Folder", action: #selector(openRecordingsFolder(_:)), keyEquivalent: ""))
        for i in menu.items { i.target = self }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height + 4), in: sender)
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, recordingItem, editItem, fullScreenItem]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier id: NSToolbarItem.Identifier, willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        let item = NSToolbarItem(itemIdentifier: id)
        switch id {
        case editItem:
            let button = NSButton(title: "Edit Layout", target: self, action: #selector(toggleEditing(_:)))
            button.bezelStyle = .toolbar
            editButton = button
            item.view = button
            item.label = "Edit Layout"
        case recordingItem:
            let button = NSButton(title: "", target: self, action: #selector(recordingItemClicked(_:)))
            button.bezelStyle = .toolbar
            button.contentTintColor = .systemRed
            item.view = button
            item.label = "Recordings"
            item.toolTip = "The recordings running now: stop one or all"
            recordingToolbarItem = item
            updateRecordingItem()
        case fullScreenItem:
            let button = NSButton(
                image: NSImage(systemSymbolName: "arrow.up.left.and.arrow.down.right", accessibilityDescription: "Full Screen")!,
                target: nil, action: #selector(NSWindow.toggleFullScreen(_:)))
            button.bezelStyle = .toolbar
            item.view = button
            item.label = "Full Screen"
            item.toolTip = "Full Screen"
        default:
            return nil
        }
        return item
    }
}

/// A viewer window: hands every event to its controller first (shortcuts
/// and the chrome's idle timer; see `ViewerWindowController.windowEvent`).
@MainActor
final class ViewerWindow: NSWindow {
    var onEvent: (NSEvent) -> Bool = { _ in false }

    override func sendEvent(_ event: NSEvent) {
        if onEvent(event) { return }
        super.sendEvent(event)
    }
}

/// The viewer window's split view controller (sidebar, stage).
///
/// On macOS 26, a split view peeks a collapsed sidebar by itself when the
/// pointer rests a moment (under a second) 3 to 15 points from the window's
/// leading edge while the window is key: it uncollapses the sidebar into the
/// split view, so the stage moves right and narrows. That gesture is our edge
/// reveal's, whose overlay must never move or resize the stage (docs/UX.md
/// "Sidebar"), and the two ran together: the overlay over a stage pushed
/// aside. AppKit's hover only counts in the key window and after the pointer
/// rests inside that band, so the same gesture did it only some of the time.
///
/// AppKit offers no public switch for it (the sidebar behavior turns it on;
/// spring loading, collapse behavior and the other item settings do not
/// change it), so this answers the split view's own question "may this
/// arranged view peek?" with no. An AppKit without that question never asks
/// it, and the self-test step `sidebar-system-peek` would then show whether
/// the peek came back some other way.
@MainActor
final class ViewerSplitViewController: NSSplitViewController {
    @objc(_splitView:canProactivePeekArrangedView:)
    func declineSystemPeek(_ splitView: NSSplitView, canProactivePeekArrangedView view: NSView) -> Bool { false }

    /// AppKit's answer to "can the sidebar peek now?" (nil without one).
    static func systemPeekAllowed(_ splitView: NSSplitView) -> Bool? {
        let selector = NSSelectorFromString("_canDoSidebarProactivePeek")
        guard splitView.responds(to: selector), let imp = splitView.method(for: selector) else { return nil }
        typealias Getter = @convention(c) (AnyObject, Selector) -> Bool
        return unsafeBitCast(imp, to: Getter.self)(splitView, selector)
    }

    /// Feeds AppKit's collapsed-sidebar hover view at the leading edge an
    /// enter and a move at `x` points from the edge, or an exit. Its
    /// tracking area is active only in the key window; the self-test's
    /// window is never key, so it calls the view directly.
    static func hoverCollapsedEdge(_ splitView: NSSplitView, x: CGFloat, inside: Bool) -> Bool {
        guard
            let hover = splitView.subviews.first(where: {
                NSStringFromClass(type(of: $0)).contains("CollapsedInteractionsView") && $0.frame.minX <= 0
            }), let window = splitView.window
        else { return false }
        let p = splitView.convert(CGPoint(x: x, y: splitView.bounds.midY), to: nil)
        guard
            let event = NSEvent.mouseEvent(
                with: .mouseMoved, location: p, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 0, pressure: 0)
        else { return false }
        if inside {
            hover.mouseEntered(with: event)
            hover.mouseMoved(with: event)
        } else {
            hover.mouseExited(with: event)
        }
        return true
    }
}

/// The stage with the designer's bar above it (shown while editing).
@MainActor
final class StageContainerController: NSViewController {
    private let stage: StageView
    private let editBar: EditBar

    private let overlay: SidebarOverlay

    init(stage: StageView, editBar: EditBar, overlay: SidebarOverlay) {
        self.stage = stage
        self.editBar = editBar
        self.overlay = overlay
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override func loadView() {
        let root = NSView()
        root.wantsLayer = true
        root.layer?.backgroundColor = NSColor(calibratedRed: 0.05, green: 0.055, blue: 0.063, alpha: 1).cgColor
        stage.translatesAutoresizingMaskIntoConstraints = false
        editBar.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(editBar)
        root.addSubview(stage)
        NSLayoutConstraint.activate([
            editBar.topAnchor.constraint(equalTo: root.safeAreaLayoutGuide.topAnchor),
            editBar.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            editBar.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            stage.topAnchor.constraint(equalTo: editBar.bottomAnchor),
            stage.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            stage.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            stage.bottomAnchor.constraint(equalTo: root.bottomAnchor),
        ])
        overlay.install(in: root)
        view = root
    }
}

/// The designer's controls: name, split, remove, presets, save as layout.
@MainActor
final class EditBar: NSView, NSTextFieldDelegate {
    weak var controller: ViewerWindowController?
    private let name = NSTextField()
    private let splitRow = NSButton(title: "Split Side by Side", target: nil, action: #selector(ViewerWindowController.splitSideBySide(_:)))
    private let splitCol = NSButton(title: "Split Stacked", target: nil, action: #selector(ViewerWindowController.splitStacked(_:)))
    private let remove = NSButton(title: "Remove Tile", target: nil, action: #selector(ViewerWindowController.removeTile(_:)))
    private let presets = NSPopUpButton(frame: .zero, pullsDown: true)
    private let saveAs = NSButton(title: "Save as Layout…", target: nil, action: #selector(ViewerWindowController.saveAsLayout(_:)))
    private let hint = NSTextField(labelWithString: "Drag a divider to resize · drag a tile onto another to swap · double-click a divider to even it out · drag cameras in from the sidebar")
    private let done = NSButton(title: "Done", target: nil, action: #selector(ViewerWindowController.toggleEditing(_:)))
    private var height: NSLayoutConstraint!
    private let stack = NSStackView()

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor(calibratedRed: 0.086, green: 0.09, blue: 0.1, alpha: 1).cgColor
        name.placeholderString = "Layout name"
        name.delegate = self
        name.widthAnchor.constraint(equalToConstant: 180).isActive = true
        presets.addItem(withTitle: "Start From")
        for p in LayoutNode.Preset.allCases {
            presets.addItem(withTitle: p.rawValue)
            presets.lastItem?.representedObject = p.rawValue
        }
        presets.target = self
        presets.action = #selector(presetChosen(_:))
        hint.textColor = .secondaryLabelColor
        hint.font = .systemFont(ofSize: 11)
        hint.lineBreakMode = .byTruncatingTail
        hint.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        done.keyEquivalent = "\r"
        for b in [splitRow, splitCol, remove, saveAs, done] { b.bezelStyle = .push; b.controlSize = .small }
        presets.controlSize = .small
        for v in [name, splitRow, splitCol, remove, presets, saveAs, hint, done] { stack.addArrangedSubview(v) }
        stack.orientation = .horizontal
        stack.spacing = 8
        stack.edgeInsets = NSEdgeInsets(top: 6, left: 12, bottom: 6, right: 12)
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.setHuggingPriority(.defaultLow, for: .horizontal)
        addSubview(stack)
        height = heightAnchor.constraint(equalToConstant: 0)
        // The bar is as wide as the window, which is 0 wide until the window
        // is sized, and the stack's content (a 180 pt name field, buttons)
        // cannot shrink to that: an equal-width pin at required priority is
        // an unsatisfiable constraint then, which AppKit logs. The trailing
        // pin is a strong preference instead, and the bar clips what does
        // not fit.
        let trailing = stack.trailingAnchor.constraint(equalTo: trailingAnchor)
        trailing.priority = .defaultHigh
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            trailing,
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            height,
        ])
        clipsToBounds = true
        isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func update(state: WindowState, model: AppModel) {
        isHidden = !state.editing
        stack.isHidden = !state.editing
        height.constant = state.editing ? 38 : 0
        if name.currentEditor() == nil { name.stringValue = state.name }
        let canSplit = state.selected != nil || state.tree.leafCount == 1
        splitRow.isEnabled = canSplit
        splitCol.isEnabled = canSplit
        remove.isEnabled = state.selected != nil
        if case .layout = state.content { saveAs.isHidden = true } else { saveAs.isHidden = false }
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        controller?.rename(to: name.stringValue.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    @objc private func presetChosen(_ sender: NSPopUpButton) {
        guard let raw = sender.selectedItem?.representedObject as? String,
            let preset = LayoutNode.Preset(rawValue: raw)
        else { return }
        controller?.applyPreset(preset)
    }
}
