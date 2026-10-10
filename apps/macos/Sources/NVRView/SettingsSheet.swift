import AppKit
import NVRCore

/// A sheet on the main window asking for host, port, user, channel, tier and
/// password, with a choice to keep the password in the Keychain. Never shown
/// unless something is missing or the user asks for it (Settings...).
@MainActor
final class SettingsSheet {
    private let host = NSTextField()
    private let port = NSTextField()
    private let user = NSTextField()
    private let channel = NSTextField()
    private let tier = NSPopUpButton()
    private let password = NSSecureTextField()
    private let save = NSButton(checkboxWithTitle: "Save the password in the Keychain", target: nil, action: nil)

    init(options: Options) {
        host.stringValue = options.host ?? ""
        port.stringValue = String(options.port)
        user.stringValue = options.user ?? ""
        channel.stringValue = String(options.channel)
        for t in StreamTier.allCases { tier.addItem(withTitle: t.name) }
        tier.selectItem(withTitle: options.tier.name)
        save.state = .on
    }

    func present(
        on window: NSWindow, completion: @escaping (ConnectionSettings, String, Bool) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = "Connection settings"
        alert.informativeText = "The password is stored only in the macOS Keychain, never in a file or on the command line."
        alert.addButton(withTitle: "Connect")
        alert.addButton(withTitle: "Cancel")

        let rows: [(String, NSView)] = [
            ("Host", host), ("Port", port), ("User", user), ("Password", password), ("Channel", channel),
            ("Stream", tier),
        ]
        let grid = NSGridView(views: rows.map { [NSTextField(labelWithString: $0.0), $0.1] })
        grid.addRow(with: [NSView(), save])
        grid.columnSpacing = 8
        grid.rowSpacing = 6
        for field in [host, port, user, password, channel] {
            field.widthAnchor.constraint(equalToConstant: 240).isActive = true
        }
        grid.column(at: 0).xPlacement = .trailing
        grid.frame = NSRect(x: 0, y: 0, width: 340, height: 190)
        alert.accessoryView = grid

        alert.beginSheetModal(for: window) { [self] response in
            guard response == .alertFirstButtonReturn,
                let port = UInt16(port.stringValue), let channel = UInt8(channel.stringValue),
                !host.stringValue.isEmpty, !user.stringValue.isEmpty,
                let tier = StreamTier(name: tier.titleOfSelectedItem ?? "")
            else { return }
            completion(
                ConnectionSettings(
                    host: host.stringValue, port: port, user: user.stringValue, channel: channel, tier: tier),
                password.stringValue, save.state == .on)
        }
    }
}
