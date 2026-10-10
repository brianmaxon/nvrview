import AppKit
import NVRCore
import SwiftUI

/// Settings: the devices (NVRs and cameras) to connect to. Passwords go to
/// the Keychain only, never to a file or the defaults; the device list file
/// holds no secret.
struct SettingsView: View {
    let model: AppModel
    @State private var selection: String?
    @State private var draft = SettingsView.blank()
    @State private var port = "9000"
    @State private var password = ""
    @State private var isNew = true

    static func blank() -> DeviceConfig {
        DeviceConfig(key: DeviceConfig.newKey(), name: "NVR", host: "", port: 9000, user: "")
    }

    var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                List(selection: $selection) {
                    ForEach(model.devices) { d in
                        HStack {
                            Text(d.config.name)
                            Spacer()
                            Text(SidebarView.stateText(d)).font(.caption)
                                .foregroundStyle(SidebarView.stateColor(d.link.state))
                        }
                        .tag(d.key)
                    }
                }
                .listStyle(.bordered)
                HStack(spacing: 4) {
                    Button {
                        startNew()
                    } label: {
                        Image(systemName: "plus").frame(width: 16, height: 16)
                    }
                    .accessibilityLabel("Add Device")
                    Button {
                        if let key = selection {
                            model.removeDevice(key)
                            startNew()
                        }
                    } label: {
                        Image(systemName: "minus").frame(width: 16, height: 16)
                    }
                    .disabled(selection == nil)
                    .accessibilityLabel("Remove Device")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(width: 220)
            Divider()
            Form {
                Section(isNew ? "New device" : "Device") {
                    TextField("Name", text: $draft.name)
                    TextField("Address", text: $draft.host, prompt: Text("192.0.2.10"))
                    TextField("Port", text: $port)
                    TextField("User", text: $draft.user)
                    SecureField(
                        "Password", text: $password,
                        prompt: Text(!isNew && model.hasPassword(draft.key) ? "Saved in the Keychain" : "Required"))
                }
                Section {
                    Text(
                        "The password is stored only in the macOS Keychain, never in a file. A failed login is not retried automatically (devices lock accounts out); saving here connects again."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    HStack {
                        Spacer()
                        if !isNew {
                            Button("Reconnect") { model.reconnect(draft.key) }
                        }
                        Button(isNew ? "Add Device" : "Save") { save() }
                            .keyboardShortcut(.defaultAction)
                            .disabled(!valid)
                    }
                }
            }
            .formStyle(.grouped)
            .frame(minWidth: 380)
        }
        .frame(minWidth: 620, minHeight: 360)
        .onChange(of: selection) { _, key in load(key) }
        .onAppear {
            if let first = model.devices.first {
                selection = first.key
                load(first.key)
            }
        }
    }

    private var valid: Bool {
        guard let p = UInt16(port), p > 0 else { return false }
        let needsPassword = isNew || !model.hasPassword(draft.key)
        return !draft.host.trimmingCharacters(in: .whitespaces).isEmpty
            && !draft.user.trimmingCharacters(in: .whitespaces).isEmpty
            && !draft.name.trimmingCharacters(in: .whitespaces).isEmpty
            && (!needsPassword || !password.isEmpty)
    }

    private func load(_ key: String?) {
        guard let key, let d = model.runtime(key) else { return }
        draft = d.config
        port = String(d.config.port)
        password = ""
        isNew = false
    }

    private func startNew() {
        selection = nil
        draft = Self.blank()
        port = "9000"
        password = ""
        isNew = true
    }

    private func save() {
        var config = draft
        config.host = config.host.trimmingCharacters(in: .whitespaces)
        config.user = config.user.trimmingCharacters(in: .whitespaces)
        config.name = config.name.trimmingCharacters(in: .whitespaces)
        config.port = UInt16(port) ?? 9000
        model.saveDevice(config, password: password.isEmpty ? nil : password)
        password = ""
        isNew = false
        selection = config.key
    }
}

/// The one settings window.
@MainActor
final class SettingsWindowController: NSWindowController {
    init(model: AppModel) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 660, height: 400), styleMask: [.titled, .closable, .resizable],
            backing: .buffered, defer: false)
        window.title = "Settings"
        window.contentViewController = NSHostingController(rootView: SettingsView(model: model))
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }
}
