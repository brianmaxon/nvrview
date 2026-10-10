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
    private var token: UUID?
    private var timer: Timer?
    private let editBar: EditBar
    private let editItem = NSToolbarItem.Identifier("edit")
    private let fullScreenItem = NSToolbarItem.Identifier("fullscreen")
    private weak var editButton: NSButton?

    init(model: AppModel, content: WindowContent, renderer: GridRenderer) {
        self.model = model
        state = WindowState(content: content, name: "", tree: .empty)
        stage = StageView(model: model, state: state, renderer: renderer)
        editBar = EditBar()

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1180, height: 700),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        window.minSize = Self.minSize
        window.collectionBehavior = [.fullScreenPrimary, .managed]
        window.titlebarSeparatorStyle = .automatic
        window.tabbingMode = .disallowed
        super.init(window: window)
        window.delegate = self

        let split = NSSplitViewController()
        let sidebarHost = NSHostingController(rootView: SidebarView(model: model, window: self, state: sidebar))
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebarHost)
        sidebarItem.minimumThickness = 200
        sidebarItem.maximumThickness = 320
        sidebarItem.canCollapse = true
        split.addSplitViewItem(sidebarItem)
        let stageVC = StageContainerController(stage: stage, editBar: editBar)
        split.addSplitViewItem(NSSplitViewItem(viewController: stageVC))
        window.contentViewController = split
        window.setContentSize(NSSize(width: 1180, height: 700))

        let toolbar = NSToolbar(identifier: "viewer")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar
        window.toolbarStyle = .unified

        stage.delegate = self
        editBar.controller = self
        token = model.subscribe { [weak self] in self?.modelChanged() }
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            // Countdowns and statuses that change without an event.
            MainActor.assumeIsolated { self?.stage.refreshChrome() }
        }
        show(content)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // MARK: Content

    /// Shows a layout, group or scratch arrangement in this window.
    func show(_ content: WindowContent) {
        if stage.isFocused { stage.tearDown() }
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
        sidebar.current = content
        stage.reload()
        syncLeases()
        updateTitle()
        stageDidChangeState(stage)
        onChange()
    }

    /// Every camera (up to 16) as an even grid, unsaved.
    static func allCameras(_ model: AppModel) -> WindowContent {
        .scratch(name: "All Cameras", root: LayoutNode.balanced(Array(model.allCameras.prefix(16))))
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
        switch state.content {
        case .layout(let id):
            if let l = model.layout(id) {
                // Another window (or app) changed it: follow, unless this
                // window is the one that just stored the same tree.
                if l.root != state.tree || l.name != state.name {
                    state.tree = l.root
                    state.name = l.name
                    stage.reload()
                    syncLeases()
                    updateTitle()
                }
            } else {
                // Deleted elsewhere: keep what is on screen, unsaved.
                state.content = .scratch(name: state.name, root: state.tree)
                sidebar.current = state.content
                updateTitle()
            }
        case .scratch:
            // The first window fills itself once cameras are known.
            if state.tree.cameras.isEmpty, state.tree.leafCount == 1, !model.allCameras.isEmpty, !state.editing {
                show(Self.allCameras(model))
            }
        case .group:
            break
        }
        stage.refreshChrome()
    }

    // MARK: StageDelegate

    func stage(_ stage: StageView, didEdit tree: LayoutNode) {
        syncLeases()
        switch state.content {
        case .layout(let id):
            var l = model.layout(id) ?? SavedLayout(id: id, name: state.name, root: tree)
            l.root = tree
            l.name = state.name
            model.saveLayout(l)
        case .group, .scratch:
            state.content = .scratch(name: state.name, root: tree)
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

    @objc func toggleListening(_ sender: Any?) {
        guard let camera = stage.selectedCamera else { return }
        model.toggleListening(camera, additive: false)
    }

    @objc func stopListening(_ sender: Any?) { model.stopListening() }

    @objc func toggleFill(_ sender: Any?) {
        state.fill.toggle()
        stage.reload()
    }

    @objc func saveAsLayout(_ sender: Any?) {
        askName("Save as Layout", initial: model.uniqueLayoutName(state.name)) { [weak self] name in
            guard let self, let id = model.createLayout(name: name, root: state.tree) else { return }
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
            state.content = .scratch(name: name, root: state.tree)
            onChange()
        }
        updateTitle()
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
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
            item.state = state.fill ? .on : .off
            return true
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
        return SavedWindow(content: content, frame: (fullScreen ? windowedFrame : nil) ?? window.frame, fullScreen: fullScreen)
    }

    func windowDidMove(_ notification: Notification) { onChange() }
    func windowDidEndLiveResize(_ notification: Notification) { onChange() }
    func windowWillEnterFullScreen(_ notification: Notification) { windowedFrame = window?.frame }
    func windowDidEnterFullScreen(_ notification: Notification) { onChange() }
    func windowDidExitFullScreen(_ notification: Notification) {
        windowedFrame = nil
        onChange()
    }

    func windowWillClose(_ notification: Notification) {
        timer?.invalidate()
        if let token { model.unsubscribe(token) }
        stage.tearDown()
        model.leases.update(from: held, to: [:])
        held = [:]
        onClose(self)
    }

    // MARK: Toolbar

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, editItem, fullScreenItem]
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

/// The stage with the designer's bar above it (shown while editing).
@MainActor
final class StageContainerController: NSViewController {
    private let stage: StageView
    private let editBar: EditBar

    init(stage: StageView, editBar: EditBar) {
        self.stage = stage
        self.editBar = editBar
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
