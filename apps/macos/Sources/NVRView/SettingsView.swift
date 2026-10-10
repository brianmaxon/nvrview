import AppKit
import NVRCore
import SwiftUI

/// Settings: devices, per-camera options, and alerts.
struct SettingsView: View {
    let model: AppModel

    var body: some View {
        TabView {
            DeviceSettingsView(model: model).tabItem { Label("Devices", systemImage: "server.rack") }
            CameraSettingsView(model: model).tabItem { Label("Cameras", systemImage: "video") }
            AlertSettingsView(model: model).tabItem { Label("Alerts", systemImage: "bell") }
        }
        .frame(minWidth: 640, minHeight: 400)
    }
}

/// The devices (NVRs and cameras) to connect to. Passwords go to the
/// Keychain only, never to a file or the defaults; the device list file
/// holds no secret.
struct DeviceSettingsView: View {
    let model: AppModel
    @State private var selection: String?
    @State private var draft = DeviceSettingsView.blank()
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

/// Per camera: Entrance (a person opens an alert inset), and for doorbells
/// the ring's audio and talk-back (camera motion control, off by default).
struct CameraSettingsView: View {
    let model: AppModel

    var body: some View {
        Form {
            Section {
                Text(
                    "Talk-back sends this Mac's microphone, or a typed reply spoken by the system voice, through a doorbell's speaker. It is off for every doorbell until allowed here, starts only when you press Talk or send a reply, is rate-limited by the core, and every use is logged."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            ForEach(model.devices) { d in
                Section(d.config.name) {
                    if d.channels.isEmpty {
                        Text("No cameras known yet (connect the device first)").foregroundStyle(.secondary)
                    }
                    ForEach(model.listedChannels(d), id: \.channel) { c in
                        row(CameraRef(device: d.key, channel: c.channel))
                    }
                }
            }
        }
        .formStyle(.grouped)
    }

    private func row(_ camera: CameraRef) -> some View {
        let o = model.cameraOptions(camera)
        return VStack(alignment: .leading, spacing: 4) {
            Text(model.name(of: camera)).font(.headline)
            Toggle(
                "Entrance: a person opens an alert inset",
                isOn: Binding(get: { o.entrance }, set: { model.setEntrance(camera, $0) }))
            Toggle(
                "Doorbell: unmute on ring even when not listening",
                isOn: Binding(get: { o.unmuteOnRing }, set: { model.setUnmuteOnRing(camera, $0) }))
            Toggle(
                "Doorbell: allow talk-back",
                isOn: Binding(get: { o.allowTalk }, set: { model.setAllowTalk(camera, $0) }))
            if model.isFisheye(camera) {
                Picker(
                    "Fisheye mount",
                    selection: Binding(get: { o.fisheyeMount }, set: { model.setFisheyeMount(camera, $0) })
                ) {
                    ForEach(FisheyeMount.allCases, id: \.self) { Text($0.title).tag($0) }
                }
            }
        }
        .padding(.vertical, 2)
    }
}

/// Alert insets and doorbell call options.
struct AlertSettingsView: View {
    let model: AppModel

    var body: some View {
        Form {
            Section("Alert insets") {
                Picker(
                    "Corner",
                    selection: Binding(
                        get: { model.insetCornerChoice?.rawValue ?? "" },
                        set: { model.insetCornerChoice = ChromeCorner(rawValue: $0) })
                ) {
                    Text("Opposite the detection indicators").tag("")
                    ForEach(ChromeCorner.allCases, id: \.rawValue) { Text($0.title).tag($0.rawValue) }
                }
                Text(
                    "When a doorbell rings, a smoke or CO alarm is heard, or a person is detected on a camera marked as an entrance, and that camera is not on screen, its live video appears in an inset. Click it (or press Tab) to swap it with the stage; Esc closes it."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Section("Doorbell calls") {
                Toggle(
                    "Save call transcripts in the event log",
                    isOn: Binding(get: { model.saveCallTranscripts }, set: { model.saveCallTranscripts = $0 }))
                Text(
                    "Calls are transcribed on this Mac while they last. With saving off (the default), the transcript is gone when the call ends. Recording a conversation may need the other person's consent where you live."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
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
