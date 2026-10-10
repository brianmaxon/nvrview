import AppKit
import FakeCore
import Foundation
import NVRCore
import VideoKit

// An unbundled executable gets no window from a SwiftUI `WindowGroup`, so
// this uses the AppKit lifecycle (like NVRViewDemo). One window, one live
// channel through CoreBridge, a status line under it.

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let options: Options
    var session: LiveSession?
    var window: NSWindow?
    var grid: MetalGridView?
    var coreGrid: CoreGrid?
    let statusLabel = NSTextField(labelWithString: "Starting")
    var timer: Timer?

    init(options: Options) { self.options = options }

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            session = try makeSession()
        } catch {
            fail("cannot start the core: \(error.localizedDescription)")
        }
        guard let session else { return }
        let renderer: GridRenderer
        do { renderer = try GridRenderer() } catch { return fail("cannot set up Metal: \(error)") }
        let view = MetalGridView(renderer: renderer)
        let layout = CoreGrid(bridge: session.bridge, gap: 0)
        coreGrid = layout
        view.prepare = { [weak session] renderer, size in
            guard let tile = session?.tile else {
                if !renderer.tiles.isEmpty { renderer.tiles = [] }
                return
            }
            layout.update(renderer: renderer, tiles: [tile], viewport: size)
        }
        grid = view

        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        view.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(view)
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            view.topAnchor.constraint(equalTo: content.topAnchor),
            view.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            view.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusLabel.topAnchor.constraint(equalTo: view.bottomAnchor, constant: 4),
            statusLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 8),
            statusLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -8),
            statusLabel.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -4),
        ])

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 560), styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "NVRView"
        window.contentMinSize = NSSize(width: 480, height: 300)
        window.contentView = content
        window.delegate = self
        window.center()
        self.window = window

        session.onChange = { [weak self] in self?.statusLabel.stringValue = self?.session?.status.text ?? "" }
        timer = Timer.scheduledTimer(withTimeInterval: 0.25, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.session?.refresh() }
        }

        if options.selfTestSeconds != nil {
            // Never take focus from whatever the user is doing.
            window.orderBack(nil)
        } else {
            buildMenu()
            window.makeKeyAndOrderFront(nil)
            NSApplication.shared.activate()
        }

        if options.fake || options.host != nil {
            startConnection()
        } else {
            showSettings()
        }
        if let seconds = options.selfTestSeconds { SelfTest(app: self).start(seconds: seconds) }
    }

    // MARK: Core

    private func makeSession() throws -> LiveSession {
        if options.fake {
            let clips = try options.files.map { try FakeClip.load($0) }
            var o = FakeCore.Options(clips: clips)
            o.pacing = .realtime(fps: 20)
            o.channelCount = Int(options.channel) + 1
            let box = FakeBox()
            return try LiveSession(factory: FakeCore.factory(options: o) { box.core = $0 }, fakeBox: box)
        }
        guard let factory = LiveSession.realFactory() else {
            throw CocoaError(.featureUnsupported, userInfo: [
                NSLocalizedDescriptionKey: "this build has no core library; run with --fake and a fixture file"
            ])
        }
        return try LiveSession(factory: factory)
    }

    func startConnection() {
        if options.fake {
            connect(ConnectionSettings(
                host: "192.0.2.1", port: 9000, user: "example-user", channel: options.channel, tier: options.tier),
                password: "example")
            return
        }
        guard let host = options.host, let user = options.user, let account = options.account else {
            return showSettings()
        }
        // NVRVIEW_PASSWORD is for development against mock-nvr, like bc-cli;
        // the Keychain is the normal source. Never from the command line.
        let keychain = KeychainPassword(service: options.keychainService, account: account)
        let fromEnvironment = ProcessInfo.processInfo.environment["NVRVIEW_PASSWORD"].flatMap { $0.isEmpty ? nil : $0 }
        guard let password = fromEnvironment ?? keychain.read() else {
            statusLabel.stringValue = "No password in the Keychain for \(account); enter it in Settings"
            return showSettings()
        }
        connect(ConnectionSettings(host: host, port: options.port, user: user, channel: options.channel, tier: options.tier),
                password: password)
    }

    func connect(_ settings: ConnectionSettings, password: String) {
        do {
            try session?.connect(settings, password: password)
        } catch {
            statusLabel.stringValue = "Cannot connect: \(error)"
        }
    }

    // MARK: Settings

    @objc func showSettings() {
        guard let window else { return }
        let sheet = SettingsSheet(options: options)
        sheet.present(on: window) { [weak self] settings, password, save in
            guard let self else { return }
            if save {
                KeychainPassword(
                    service: options.keychainService, account: options.keychainAccount ?? "\(settings.user)@\(settings.host)")
                    .save(password)
            }
            connect(settings, password: password)
        }
    }

    private func buildMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let menu = NSMenu()
        menu.addItem(NSMenuItem(title: "Settings...", action: #selector(showSettings), keyEquivalent: ","))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit NVRView", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        appItem.submenu = menu
        NSApplication.shared.mainMenu = main
    }

    // MARK: Lifecycle

    private func fail(_ message: String) {
        FileHandle.standardError.write(Data((message + "\n").utf8))
        exit(1)
    }

    func windowWillClose(_ notification: Notification) {
        timer?.invalidate()
        session?.shutdown()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        session?.shutdown()
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
