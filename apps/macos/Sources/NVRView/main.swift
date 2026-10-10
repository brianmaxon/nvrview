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
    private var settings: SettingsWindowController?
    /// With `--selftest`: AppKit's warnings on standard error, counted.
    let logWatch: LogWatch?

    init(options: Options) {
        self.options = options
        logWatch = options.selfTestSeconds == nil ? nil : LogWatch(patterns: SelfTest.forbiddenLog)
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
        model.start(extraDevices: commandLineDevice(), muted: options.selfTestSeconds != nil)
        if options.fake { seedFakeLayouts(model) }

        if options.selfTestSeconds == nil { buildMenu() }
        let first = model.layouts.first.map { WindowContent.layout(id: $0.id) } ?? ViewerWindowController.allCameras(model)
        openWindow(first)
        if options.selfTestSeconds == nil {
            NSApplication.shared.activate()
            if model.devices.isEmpty { showSettings(nil) }
        }
        if let seconds = options.selfTestSeconds { SelfTest(app: self).start(seconds: seconds) }
    }

    // MARK: Core

    private func makeModel() throws -> AppModel {
        if options.fake {
            let clips = try options.files.map { try FakeClip.load($0) }
            var o = FakeCore.Options(clips: clips)
            o.pacing = .realtime(fps: 20)
            o.devices = [
                "192.0.2.10": FakeDeviceSpec(name: "NVR", channelCount: 6),
                "192.0.2.11": FakeDeviceSpec(name: "NVR", channelCount: 4),
            ]
            o.detectionDemo = 12
            let box = FakeBox()
            let credentials = MemoryCredentials()
            let model = try AppModel(
                factory: FakeCore.factory(options: o) { box.core = $0 }, fakeBox: box, credentials: credentials,
                deviceFile: JSONFileStore(file: nil), groupFile: JSONFileStore(file: nil))
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
                    ?? JSONFileStore<CameraGroup>.appSupport("groups.json")))
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

    // MARK: Windows

    @discardableResult
    func openWindow(_ content: WindowContent) -> ViewerWindowController? {
        guard let model else { return nil }
        let renderer: GridRenderer
        do { renderer = try GridRenderer() } catch { fail("cannot set up Metal: \(error)") }
        let controller = ViewerWindowController(model: model, content: content, renderer: renderer)
        controller.onClose = { [weak self] c in self?.windows.removeAll { $0 === c } }
        windows.append(controller)
        guard let window = controller.window else { return controller }
        if let last = windows.dropLast().last?.window {
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

    @objc func newWindow(_ sender: Any?) {
        guard let model else { return }
        let key = NSApp.keyWindow?.windowController as? ViewerWindowController
        openWindow(key?.state.content ?? ViewerWindowController.allCameras(model))
    }

    @objc func showSettings(_ sender: Any?) {
        guard let model else { return }
        if settings == nil { settings = SettingsWindowController(model: model) }
        settings?.showWindow(nil)
        settings?.window?.makeKeyAndOrderFront(nil)
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool { true }

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
        submenu("NVRView", [
            item("Settings…", #selector(showSettings(_:)), ","),
            .separator(),
            item("Hide NVRView", #selector(NSApplication.hide(_:)), "h"),
            item("Quit NVRView", #selector(NSApplication.terminate(_:)), "q"),
        ])
        submenu("File", [
            item("New Window", #selector(newWindow(_:)), "n"),
            item("New Layout…", #selector(ViewerWindowController.newLayout(_:)), "n", [.command, .shift]),
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
        submenu("Layout", [
            item("Edit Layout", #selector(ViewerWindowController.toggleEditing(_:)), "e"),
            item("Split Side by Side", #selector(ViewerWindowController.splitSideBySide(_:)), "d"),
            item("Split Stacked", #selector(ViewerWindowController.splitStacked(_:)), "d", [.command, .shift]),
            item("Remove Tile", #selector(ViewerWindowController.removeTile(_:))),
            .separator(),
            item("Focus Tile", #selector(ViewerWindowController.toggleFocus(_:)), "f"),
            item("Fill Tiles", #selector(ViewerWindowController.toggleFill(_:))),
        ])
        submenu("View", [
            item("Toggle Sidebar", #selector(NSSplitViewController.toggleSidebar(_:)), "s", [.command, .control]),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
        ])
        submenu("Audio", [
            // Cmd-L: listen to the selected (or focused) tile, also in full screen.
            item("Listen", #selector(ViewerWindowController.toggleListening(_:)), "l"),
            item("Stop Listening", #selector(ViewerWindowController.stopListening(_:)), "l", [.command, .shift]),
        ])
        let window = NSMenu(title: "Window")
        window.addItem(item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"))
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

    func applicationWillTerminate(_ notification: Notification) {
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

MainActor.assumeIsolated { runApp() }
