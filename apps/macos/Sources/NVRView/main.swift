import AppKit
import FakeCore
import Foundation
import NVRCore
import VideoKit

// An unbundled executable gets no window from a SwiftUI `WindowGroup`, so
// this uses the AppKit lifecycle (like NVRViewDemo); SwiftUI draws the
// sidebar and Settings inside AppKit windows.

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuItemValidation {
    let options: Options
    private(set) var model: AppModel?
    private(set) var windows: [ViewerWindowController] = []
    /// Calls shown in their own windows (not viewer windows).
    private(set) var callWindows: CallWindows?
    private var settings: SettingsWindowController?
    /// Open recordings windows (docs/UX.md "Recordings").
    private(set) var recordingsWindows: [RecordingsWindowController] = []
    /// With `--selftest`: AppKit's warnings on standard error, counted.
    let logWatch: LogWatch?
    /// With `--selftest`: the viewer's clock, moved only by the self-test
    /// (animations and timeouts finish when it says, not after real time).
    let testClock: ManualViewerClock?
    /// The windows to reopen on the next launch (with `--fake` or
    /// `--selftest` only when given a `--data-dir`: their layouts and
    /// devices are made up).
    private let windowFile: JSONFileStore<SavedWindow>
    private var saveScheduled = false
    /// How often a viewer window asked to be saved (self-test).
    private(set) var windowSaveRequests = 0
    private var terminating = false

    init(options: Options) {
        self.options = options
        logWatch = options.selfTestSeconds == nil ? nil : LogWatch(patterns: SelfTest.forbiddenLog)
        testClock = options.selfTestSeconds == nil ? nil : ManualViewerClock(now: SystemViewerClock().now)
        let remembers = options.dataDirectory != nil || (!options.fake && options.selfTestSeconds == nil)
        windowFile = JSONFileStore(
            file: remembers
                ? options.dataDirectory?.appendingPathComponent("windows.json")
                    ?? JSONFileStore<SavedWindow>.appSupport("windows.json")
                : nil)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let model: AppModel
        do {
            model = try makeModel()
        } catch {
            fail("cannot start the core: \(error)")
        }
        self.model = model
        model.openWindow = { [weak self] content in self?.openWindow(content) }
        startCallWindows(model)
        model.start(extraDevices: commandLineDevice(), muted: options.selfTestSeconds != nil)
        model.recordings.start(downloads: recordingsDownloads())
        startUpdater(model)
        if options.fake { seedFakeLayouts(model) }

        if options.selfTestSeconds == nil { buildMenu() }
        if !restoreWindows() {
            let first = model.layouts.first.map { WindowContent.layout(id: $0.id) } ?? ViewerWindowController.allCameras(model)
            openWindow(first)
        }
        if options.selfTestSeconds == nil {
            NSApplication.shared.activate()
            if model.devices.isEmpty { showSettings(nil) }
        }
        if let seconds = options.selfTestSeconds { SelfTest(app: self).start(seconds: seconds) }
    }

    // MARK: Call windows

    /// Calls in their own windows (docs/UX.md "A call in its own window").
    /// The self-test remembers their frame and pin in a throwaway suite.
    private func startCallWindows(_ model: AppModel) {
        let test = options.selfTestSeconds != nil
        let defaults: UserDefaults
        if test, let suite = UserDefaults(suiteName: Self.selfTestDefaults) {
            suite.removePersistentDomain(forName: Self.selfTestDefaults)
            defaults = suite
        } else {
            defaults = .standard
        }
        model.useCallDefaults(defaults)
        model.useCaptureDefaults(defaults)
        if let data = options.dataDirectory { model.transcripts.folder = data.appendingPathComponent("Transcripts") }
        let calls = CallWindows(model: model, defaults: defaults, keepsBehind: test)
        calls.activeScreen = { [weak self] in self?.activeViewerWindow?.window?.screen ?? NSScreen.screens.first }
        model.popOutCall = { [weak calls] camera in calls?.popOut(camera) }
        model.popInCall = { [weak calls] camera in calls?.popIn(camera) }
        callWindows = calls
    }

    static let selfTestDefaults = "nvrview-selftest"

    /// The key viewer window, else the main one, else the first.
    var activeViewerWindow: ViewerWindowController? {
        windows.first { $0.window?.isKeyWindow == true } ?? windows.first { $0.window?.isMainWindow == true } ?? windows.first
    }

    // MARK: Updates

    /// Self-update (docs/UPDATES.md). Its settings live with the others; in
    /// a self-test or with `--fake` (unless given a `--data-dir`) they are
    /// kept in memory, and automatic checks are off either way there.
    private func startUpdater(_ model: AppModel) {
        let test = options.fake || options.selfTestSeconds != nil
        let file: URL? =
            options.dataDirectory?.appendingPathComponent("update-settings.json")
            ?? (test ? nil : JSONFileStore<UpdateSettings>.appSupport("update-settings.json"))
        let updater = Updater(bridge: model.bridge, store: JSONFileStore(file: file), automaticAllowed: !test)
        updater.present = { [weak self] prompt in self?.presentUpdate(prompt) }
        model.updater = updater
        updater.start()
    }

    @objc func checkForUpdates(_ sender: Any?) { model?.updater?.menuAction() }

    /// Asks or tells the user; a sheet on the front window when there is
    /// one, so live view keeps playing behind it.
    private func presentUpdate(_ prompt: Updater.Prompt) {
        guard options.selfTestSeconds == nil, let updater = model?.updater else { return }
        let alert = NSAlert()
        var respond: (NSApplication.ModalResponse) -> Void = { _ in }
        switch prompt {
        case .available(let v, let notes):
            alert.messageText = "NVRView \(v) is available"
            alert.informativeText = (notes.isEmpty ? "" : notes + "\n\n") + "You have \(updater.current)."
            alert.addButton(withTitle: "Download")
            alert.addButton(withTitle: "Not Now")
            respond = { if $0 == .alertFirstButtonReturn { updater.download(v) } }
        case .ready(let v):
            alert.messageText = "Update to \(v)?"
            alert.informativeText = "NVRView quits, installs \(v), and opens again. With Later, it installs when you next quit."
            alert.addButton(withTitle: "Restart Now")
            alert.addButton(withTitle: "Later")
            respond = { $0 == .alertFirstButtonReturn ? updater.restartNow() : updater.later() }
        case .upToDate(let v):
            alert.messageText = "NVRView is up to date"
            alert.informativeText = "Version \(v) is the newest at your update source."
        case .failed(let message):
            alert.messageText = "Update check"
            alert.informativeText = message
        }
        if let window = NSApp.keyWindow ?? NSApp.mainWindow {
            alert.beginSheetModal(for: window) { respond($0) }
        } else {
            respond(alert.runModal())
        }
    }

    // MARK: Core

    private func makeModel() throws -> AppModel {
        if options.fake {
            // A file named like the fisheye fixture plays as a fisheye camera
            // (the second NVR's channel 4) rather than in the ordinary rotation.
            let isFisheye = { (u: URL) in u.lastPathComponent.contains("fisheye") }
            let clips = try options.files.filter { !isFisheye($0) }.map { try FakeClip.load($0) }
            var o = FakeCore.Options(clips: clips)
            o.fisheyeClip = try options.files.first(where: isFisheye).map { try FakeClip.load($0) }
            o.pacing = .realtime(fps: 20)
            o.devices = [
                // Channel 4 is a made-up doorbell (talk-back capable);
                // channel 2 reports pan, tilt, zoom and presets.
                "192.0.2.10": FakeDeviceSpec(
                    name: "NVR", channelCount: 6, multiLensChannels: [5], talkChannels: [3], ptzChannels: [1]),
                "192.0.2.11": FakeDeviceSpec(
                    name: "NVR", channelCount: 4, fisheyeChannels: o.fisheyeClip == nil ? [] : [3]),
            ]
            // The self-test sends its own detections and alarms, at known times.
            o.detectionDemo = options.selfTestSeconds == nil ? 12 : nil
            // The self-test finishes thumbnail fetches and downloads itself.
            o.recordingWorkIsManual = options.selfTestSeconds != nil
            let box = FakeBox()
            let credentials = MemoryCredentials()
            let model = try AppModel(
                factory: FakeCore.factory(options: o) { box.core = $0 }, fakeBox: box, credentials: credentials,
                deviceFile: JSONFileStore(file: nil), groupFile: JSONFileStore(file: nil),
                clock: testClock ?? SystemViewerClock(),
                // The self-test never asks for the microphone or speech recognition.
                callServices: options.selfTestSeconds != nil ? .fakes() : nil)
            return model
        }
        guard let factory = AppModel.realFactory(dataDirectory: options.dataDirectory) else {
            throw CocoaError(.featureUnsupported, userInfo: [
                NSLocalizedDescriptionKey: "this build has no core library; run with --fake and fixture files"
            ])
        }
        return try AppModel(
            factory: factory, fakeBox: nil, credentials: KeychainCredentials(service: options.keychainService),
            deviceFile: JSONFileStore(
                file: options.dataDirectory?.appendingPathComponent("devices.json")
                    ?? JSONFileStore<DeviceConfig>.appSupport("devices.json")),
            groupFile: JSONFileStore(
                file: options.dataDirectory?.appendingPathComponent("groups.json")
                    ?? JSONFileStore<CameraGroup>.appSupport("groups.json")),
            cameraFile: JSONFileStore(
                file: options.dataDirectory?.appendingPathComponent("cameras.json")
                    ?? JSONFileStore<CameraOptions>.appSupport("cameras.json")),
            recordingsFile: JSONFileStore(
                file: options.dataDirectory?.appendingPathComponent("recordings-cameras.json")
                    ?? JSONFileStore<RecordingsEnablement>.appSupport("recordings-cameras.json")),
            clock: testClock ?? SystemViewerClock(),
            callServices: options.selfTestSeconds != nil ? .fakes() : nil)
    }

    /// `--host`/`--user`: one device for this run, password from
    /// NVRVIEW_PASSWORD (development against mock-nvr) or the Keychain.
    private func commandLineDevice() -> [(DeviceConfig, String)] {
        if options.fake {
            // Two made-up NVRs (FakeCore.Options.devices describes them).
            return ["192.0.2.10", "192.0.2.11"].enumerated().map { i, host in
                (DeviceConfig(key: "fake-\(i + 1)", name: "NVR \(i + 1)", host: host, port: 9000, user: "example-user"),
                 "example-password")
            }
        }
        guard let host = options.host, let user = options.user else { return [] }
        let fromEnvironment = ProcessInfo.processInfo.environment["NVRVIEW_PASSWORD"].flatMap { $0.isEmpty ? nil : $0 }
        let keychain = KeychainPassword(service: options.keychainService, account: "\(user)@\(host)")
        guard let password = fromEnvironment ?? keychain.read() else {
            FileHandle.standardError.write(Data("no password for \(user)@\(host); add the device in Settings\n".utf8))
            return []
        }
        let config = DeviceConfig(key: "cli-\(host)-\(options.port)", name: host, host: host, port: options.port, user: user)
        return [(config, password)]
    }

    /// Two example layouts for `--fake`, so the sidebar has something to show.
    private func seedFakeLayouts(_ model: AppModel) {
        guard model.layouts.isEmpty else { return }
        // Named by the keys the core gave the devices.
        let k1 = model.runtime("fake-1")?.key ?? "fake-1", k2 = model.runtime("fake-2")?.key ?? "fake-2"
        let a = (0..<6).map { CameraRef(device: k1, channel: $0) }
        let b = (0..<4).map { CameraRef(device: k2, channel: $0) }
        model.createLayout(name: "Overview", root: LayoutNode.balanced(a + b))
        model.createLayout(
            name: "Split Example",
            root: .split(
                .row, ratio: 0.62, a: .leaf(a[0]),
                b: .split(.col, ratio: 0.34, a: .leaf(b[1]), b: .split(.col, ratio: 0.5, a: .leaf(a[2]), b: .leaf(b[3])))))
    }

    // MARK: Recordings

    /// Where downloaded incidents go: `~/Movies/NVRView/Incidents`; with
    /// `--data-dir`, its `Incidents` folder; with `--fake` or `--selftest`
    /// otherwise, a folder of its own under the temporary directory.
    private func recordingsDownloads() -> URL {
        if let data = options.dataDirectory { return data.appendingPathComponent("Incidents", isDirectory: true) }
        if options.fake || options.selfTestSeconds != nil {
            return FileManager.default.temporaryDirectory
                .appendingPathComponent("nvrview-fake-incidents-\(ProcessInfo.processInfo.processIdentifier)", isDirectory: true)
        }
        return FileManager.default.urls(for: .moviesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NVRView/Incidents", isDirectory: true)
    }

    /// Window > Recordings: brings the open recordings window forward, or
    /// opens one.
    @objc func showRecordings(_ sender: Any?) {
        if let open = recordingsWindows.first {
            open.showWindow(nil)
            open.window?.makeKeyAndOrderFront(nil)
            return
        }
        openRecordingsWindow()?.showWindow(nil)
    }

    /// Opens a recordings window (behind the others, never key, in a self-test).
    @discardableResult
    func openRecordingsWindow() -> RecordingsWindowController? {
        guard let model, model.recordings.available else { return nil }
        let controller = RecordingsWindowController(model: model.recordings)
        controller.onClose = { [weak self] c in self?.recordingsWindows.removeAll { $0 === c } }
        recordingsWindows.append(controller)
        if options.selfTestSeconds != nil {
            controller.window?.orderBack(nil)
        } else if let window = controller.window, window.frame.origin == .zero {
            window.center()
        }
        return controller
    }

    // MARK: Windows

    @discardableResult
    func openWindow(_ content: WindowContent, frame: CGRect? = nil) -> ViewerWindowController? {
        guard let model else { return nil }
        let renderer: GridRenderer
        do { renderer = try GridRenderer() } catch { fail("cannot set up Metal: \(error)") }
        let controller = ViewerWindowController(model: model, content: content, renderer: renderer)
        controller.onClose = { [weak self] c in
            guard let self else { return }
            windows.removeAll { $0 === c }
            if !terminating { saveWindows() }
            // Call windows are not viewer windows: with the last viewer
            // window their calls end (and their windows close), so the app
            // quits as before.
            if windows.isEmpty, !terminating { model.calls.endAll() }
        }
        controller.onChange = { [weak self] in
            self?.windowSaveRequests += 1
            self?.scheduleSaveWindows()
        }
        windows.append(controller)
        defer { scheduleSaveWindows() }
        guard let window = controller.window else { return controller }
        if let frame {
            window.setFrame(frame, display: false)
        } else if let last = windows.dropLast().last?.window {
            window.setFrameTopLeftPoint(window.cascadeTopLeft(from: NSPoint(x: last.frame.minX, y: last.frame.maxY)))
        } else {
            window.center()
        }
        if options.selfTestSeconds != nil {
            // Never take focus from whatever the user is doing.
            window.orderBack(nil)
        } else {
            controller.showWindow(nil)
        }
        return controller
    }

    // MARK: Restoring windows

    /// Reopens the windows that were open when the app last quit, each with
    /// what it showed and where it was. Returns false if there were none.
    private func restoreWindows() -> Bool {
        guard let model else { return false }
        let saved = windowFile.load()
        let screens = NSScreen.screens.map(\.visibleFrame)
        var reopened = false
        for w in saved {
            let content: WindowContent
            switch w.content {
            case .layout(let id):
                content = model.layout(id) != nil ? .layout(id: id) : ViewerWindowController.allCameras(model)
            case .group(let id):
                content = model.group(id) != nil ? .group(id: id) : ViewerWindowController.allCameras(model)
            case .scratch(let name, let root):
                content = .scratch(name: name, root: root)
            }
            let frame = SavedWindow.placement(of: w.frame, screens: screens, minSize: ViewerWindowController.minSize)
            guard let controller = openWindow(content, frame: frame) else { continue }
            reopened = true
            controller.restoreSidebar(collapsed: w.sidebarCollapsed)
            controller.restoreInsetAnchor(w.insetAnchor)
            controller.restoreViews(w.views)
            if w.fullScreen, options.selfTestSeconds == nil { controller.window?.toggleFullScreen(nil) }
        }
        return reopened
    }

    private func scheduleSaveWindows() {
        guard windowFile.file != nil, !saveScheduled, !terminating else { return }
        saveScheduled = true
        // Coalesces a window drag or a run of edits into one write.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.saveScheduled = false
                if !self.terminating { self.saveWindows() }
            }
        }
    }

    private func saveWindows() {
        guard windowFile.file != nil else { return }
        windowFile.save(windows.compactMap(\.savedWindow))
    }

    @objc func newWindow(_ sender: Any?) {
        guard let model else { return }
        let key = NSApp.keyWindow?.windowController as? ViewerWindowController
        openWindow(key?.state.content ?? ViewerWindowController.allCameras(model))
    }

    /// The Settings window for the self-test: ordered to the back, never key.
    func settingsWindowForTest() -> SettingsWindowController? {
        guard let model else { return nil }
        if settings == nil { settings = SettingsWindowController(model: model) }
        settings?.window?.orderBack(nil)
        return settings
    }

    @objc func showSettings(_ sender: Any?) {
        guard let model else { return }
        if settings == nil { settings = SettingsWindowController(model: model) }
        settings?.showWindow(nil)
        settings?.window?.makeKeyAndOrderFront(nil)
    }

    @objc func toggleFrameRates(_ sender: Any?) { model?.showFrameRates.toggle() }
    private func cornerMenuItem() -> NSMenuItem {
        let menu = NSMenu(title: "Detection Indicators")
        for corner in ChromeCorner.allCases {
            let i = NSMenuItem(title: corner.title, action: #selector(setIndicatorCorner(_:)), keyEquivalent: "")
            i.target = self
            i.representedObject = corner.rawValue
            menu.addItem(i)
        }
        let parent = NSMenuItem(title: "Detection Indicators", action: nil, keyEquivalent: "")
        parent.submenu = menu
        return parent
    }
    @objc func setIndicatorCorner(_ sender: NSMenuItem) {
        if let raw = sender.representedObject as? String, let c = ChromeCorner(rawValue: raw) { model?.indicatorCorner = c }
    }
    @objc func toggleHideOffline(_ sender: Any?) { model?.hideOfflineChannels.toggle() }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        if item.action == #selector(showRecordings(_:)) { return model?.recordings.available == true }
        if item.action == #selector(checkForUpdates(_:)) {
            item.title = model?.updater?.menuTitle ?? "Check for Updates…"
            return model?.updater != nil
        }
        if item.action == #selector(toggleFrameRates(_:)) { item.state = model?.showFrameRates == true ? .on : .off }
        if item.action == #selector(toggleHideOffline(_:)) { item.state = model?.hideOfflineChannels == true ? .on : .off }
        if item.action == #selector(setIndicatorCorner(_:)) {
            item.state = (item.representedObject as? String) == model?.indicatorCorner.rawValue ? .on : .off
        }
        return true
    }

    private func buildMenu() {
        let main = NSMenu()
        func submenu(_ title: String, _ items: [NSMenuItem]) {
            let item = NSMenuItem()
            let menu = NSMenu(title: title)
            for i in items { menu.addItem(i) }
            item.submenu = menu
            main.addItem(item)
        }
        func item(_ title: String, _ action: Selector, _ key: String = "", _ mods: NSEvent.ModifierFlags = .command)
            -> NSMenuItem
        {
            let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
            i.keyEquivalentModifierMask = mods
            return i
        }
        /// File > New Layout from Template: each template with its shape.
        func templateItem() -> NSMenuItem {
            let parent = NSMenuItem(title: "New Layout from Template", action: nil, keyEquivalent: "")
            let menu = NSMenu(title: parent.title)
            for t in LayoutTemplate.ordered {
                let i = item(t.title, #selector(ViewerWindowController.newLayoutFromTemplate(_:)))
                i.keyEquivalent = ""
                i.representedObject = t.rawValue
                i.image = LayoutIcon.image(for: t.shape)
                menu.addItem(i)
            }
            parent.submenu = menu
            return parent
        }
        submenu("NVRView", [
            item("Settings…", #selector(showSettings(_:)), ","),
            item("Check for Updates…", #selector(checkForUpdates(_:))),
            .separator(),
            item("Hide NVRView", #selector(NSApplication.hide(_:)), "h"),
            item("Quit NVRView", #selector(NSApplication.terminate(_:)), "q"),
        ])
        submenu("File", [
            item("New Window", #selector(newWindow(_:)), "n"),
            item("New Layout…", #selector(ViewerWindowController.newLayout(_:)), "n", [.command, .shift]),
            templateItem(),
            item("Save as Layout…", #selector(ViewerWindowController.saveAsLayout(_:)), "s", [.command, .shift]),
            .separator(),
            item("Close", #selector(NSWindow.performClose(_:)), "w"),
        ])
        submenu("Edit", [
            item("Undo", Selector(("undo:")), "z"),
            item("Redo", Selector(("redo:")), "z", [.command, .shift]),
            .separator(),
            item("Cut", #selector(NSText.cut(_:)), "x"),
            item("Copy", #selector(NSText.copy(_:)), "c"),
            item("Paste", #selector(NSText.paste(_:)), "v"),
            item("Select All", #selector(NSText.selectAll(_:)), "a"),
        ])
        /// Kept working from the keyboard but not listed (the single key
        /// beside it is the one shown).
        func hidden(_ i: NSMenuItem) -> NSMenuItem {
            i.isHidden = true
            i.allowsKeyEquivalentWhenHidden = true
            return i
        }
        // The single-key shortcuts (docs/UX.md "Keyboard") are listed with
        // their keys. A viewer window takes those keys before the menus do
        // (ViewerWindow), and not while text is being edited, so the
        // equivalents here are for showing the keys. The arrows have none:
        // an arrow key equivalent swallows the key even when its item is
        // disabled, which would take the arrows from lists.
        let select = NSMenuItem(title: "Select Tile", action: nil, keyEquivalent: "")
        select.submenu = {
            let m = NSMenu(title: "Select Tile")
            m.addItem(item("Left  (Left Arrow)", #selector(ViewerWindowController.selectLeft(_:)), "", []))
            m.addItem(item("Right  (Right Arrow)", #selector(ViewerWindowController.selectRight(_:)), "", []))
            m.addItem(item("Up  (Up Arrow)", #selector(ViewerWindowController.selectUp(_:)), "", []))
            m.addItem(item("Down  (Down Arrow)", #selector(ViewerWindowController.selectDown(_:)), "", []))
            return m
        }()
        submenu("Layout", [
            item("Edit Layout", #selector(ViewerWindowController.toggleEditing(_:)), "e"),
            item("Split Side by Side", #selector(ViewerWindowController.splitSideBySide(_:)), "d"),
            item("Split Stacked", #selector(ViewerWindowController.splitStacked(_:)), "d", [.command, .shift]),
            item("Remove Tile", #selector(ViewerWindowController.removeTile(_:))),
            .separator(),
            item("Focus Tile", #selector(ViewerWindowController.toggleFocus(_:)), " ", []),
            hidden(item("Focus Tile", #selector(ViewerWindowController.toggleFocus(_:)), "f")),
            select,
            item("Fill Tiles", #selector(ViewerWindowController.toggleFill(_:))),
        ])
        submenu("View", [
            item("Hide Sidebar", #selector(ViewerWindowController.sidebarKey(_:)), "s", []),
            hidden(item("Toggle Sidebar", #selector(NSSplitViewController.toggleSidebar(_:)), "s", [.command, .control])),
            item("Enter Full Screen", #selector(ViewerWindowController.fullScreenKey(_:)), "f", []),
            hidden(item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control])),
            item("Reset Zoom", #selector(ViewerWindowController.resetZoomKey(_:)), "0", []),
            .separator(),
            item("Show Frame Rates", #selector(toggleFrameRates(_:))),
            item("Hide Offline Channels", #selector(toggleHideOffline(_:))),
            cornerMenuItem(),
        ])
        main.addItem(CameraMenu.make())
        submenu("Audio", [
            // Cmd-L: listen to the selected (or focused) tile, also in full screen.
            item("Listen or Mute", #selector(ViewerWindowController.listenKey(_:)), "m", []),
            hidden(item("Listen", #selector(ViewerWindowController.toggleListening(_:)), "l")),
            item("Stop Listening", #selector(ViewerWindowController.stopListening(_:)), "l", [.command, .shift]),
        ])
        let window = NSMenu(title: "Window")
        window.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
        window.addItem(.separator())
        window.addItem(item("Recordings", #selector(showRecordings(_:)), "r", [.command, .option]))
        let windowItem = NSMenuItem()
        windowItem.submenu = window
        main.addItem(windowItem)
        NSApplication.shared.mainMenu = main
        NSApplication.shared.windowsMenu = window
    }

    // MARK: Lifecycle

    func fail(_ message: String) -> Never {
        FileHandle.standardError.write(Data((message + "\n").utf8))
        exit(1)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        // The windows open now are the ones to reopen; closing them on the
        // way out must not forget them.
        saveWindows()
        terminating = true
        return .terminateNow
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Before the core stops: the helper installs a waiting update once
        // this process has exited.
        model?.updater?.appWillTerminate()
        model?.shutdown()
    }
}

@MainActor
func runApp() {
    guard let options = Options.parse(CommandLine.arguments) else {
        FileHandle.standardError.write(Data(Options.usage.utf8))
        exit(2)
    }
    let app = NSApplication.shared
    // A self-test gets no Dock icon or menu bar.
    app.setActivationPolicy(options.selfTestSeconds == nil ? .regular : .accessory)
    let delegate = AppDelegate(options: options)
    app.delegate = delegate
    withExtendedLifetime(delegate) { app.run() }
}

// `--apply-update`: the update helper (no window, no core); see UpdateHelper.
if CommandLine.arguments.dropFirst().first == "--apply-update" {
    exit(UpdateHelper.run(Array(CommandLine.arguments.dropFirst())))
}
MainActor.assumeIsolated { runApp() }
