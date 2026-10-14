import AppKit
import NVRCore
import SwiftUI
import AppBase
import AppUpdates
import AppCapture
import AppAlerts

/// The Settings window's size. The tab bar shows every tab (icon and label)
/// in one row, so the window must stay at least as wide as that row: six tabs
/// need about 640 points, and the width leaves room for a couple more. The
/// self-test (`settings-tabs-fit`) measures the row against this minimum.
enum SettingsLayout {
    static let minWidth: CGFloat = 860
    static let minHeight: CGFloat = 420
    /// About one more tab's width (icon and label), which the tab bar must
    /// still fit within the minimum width.
    static let spareTab: CGFloat = 110
}

/// Settings: devices, per-camera options, and alerts.
struct SettingsView: View {
    let model: AppModel

    var body: some View {
        TabView {
            DeviceSettingsView(model: model).tabItem { Label("Devices", systemImage: "server.rack") }
            CameraSettingsView(model: model).tabItem { Label("Cameras", systemImage: "video") }
            AlertSettingsView(model: model).tabItem { Label("Alerts", systemImage: "bell") }
            SmartAlertsSettingsView(model: model).tabItem { Label("Smart Alerts", systemImage: "checkmark.shield") }
            CaptureSettingsView(model: model).tabItem { Label("Capture", systemImage: "camera") }
            if let updater = model.updater {
                UpdateSettingsView(updater: updater).tabItem { Label("Updates", systemImage: "arrow.down.circle") }
            }
        }
        .frame(minWidth: SettingsLayout.minWidth, minHeight: SettingsLayout.minHeight)
    }
}

/// The add-device form's fields, kept outside the view so that choosing a
/// found device (a click in the list, or the self-test) fills the same form.
@MainActor
@Observable
final class DeviceForm {
    static let defaultName = "NVR"
    var draft = DeviceForm.blank()
    var port = "9000"
    var password = ""
    var isNew = true

    static func blank() -> DeviceConfig {
        DeviceConfig(key: DeviceConfig.newKey(), name: defaultName, host: "", port: 9000, user: "")
    }

    func startNew() {
        draft = Self.blank()
        port = "9000"
        password = ""
        isNew = true
    }

    func load(_ config: DeviceConfig) {
        draft = config
        port = String(config.port)
        password = ""
        isNew = false
    }

    /// A device chosen in "Find Devices on This Network": its address and
    /// port go into the form, and its name too while the form still has the
    /// default one. The user and password stay theirs to type.
    func fill(from found: DiscoveredDevice) {
        if !isNew { startNew() }
        draft.host = found.address
        port = String(found.suggestedPort)
        let current = draft.name.trimmingCharacters(in: .whitespaces)
        if let name = found.name, !name.isEmpty, current.isEmpty || current == Self.defaultName {
            draft.name = name
        }
    }
}

/// The devices (NVRs and cameras) to connect to. Passwords go to the
/// Keychain only, never to a file or the defaults; the device list file
/// holds no secret.
struct DeviceSettingsView: View {
    let model: AppModel
    @State private var selection: String?

    private var form: DeviceForm { model.deviceForm }

    var body: some View {
        @Bindable var form = model.deviceForm
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
                if form.isNew {
                    FindDevicesSection(model: model)
                }
                Section(form.isNew ? "New device" : "Device") {
                    TextField("Name", text: $form.draft.name)
                    TextField("Address", text: $form.draft.host, prompt: Text("192.0.2.10"))
                    TextField("Port", text: $form.port)
                    TextField("User", text: $form.draft.user)
                    SecureField(
                        "Password", text: $form.password,
                        prompt: Text(!form.isNew && model.hasPassword(form.draft.key) ? "Saved in the Keychain" : "Required"))
                }
                Section {
                    Text(
                        "The password is stored only in the macOS Keychain, never in a file. A failed login is not retried automatically (devices lock accounts out); saving here connects again."
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    HStack {
                        Spacer()
                        if !form.isNew {
                            Button("Reconnect") { model.reconnect(form.draft.key) }
                        }
                        Button(form.isNew ? "Add Device" : "Save") { save() }
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
            // With devices already added, show the first; with none, the
            // new-device form opens with the network search.
            if form.isNew, let first = model.devices.first {
                selection = first.key
                load(first.key)
            }
        }
    }

    private var valid: Bool {
        guard let p = UInt16(form.port), p > 0 else { return false }
        let needsPassword = form.isNew || !model.hasPassword(form.draft.key)
        return !form.draft.host.trimmingCharacters(in: .whitespaces).isEmpty
            && !form.draft.user.trimmingCharacters(in: .whitespaces).isEmpty
            && !form.draft.name.trimmingCharacters(in: .whitespaces).isEmpty
            && (!needsPassword || !form.password.isEmpty)
    }

    private func load(_ key: String?) {
        guard let key, let d = model.runtime(key) else { return }
        form.load(d.config)
    }

    private func startNew() {
        selection = nil
        form.startNew()
    }

    private func save() {
        var config = form.draft
        config.host = config.host.trimmingCharacters(in: .whitespaces)
        config.user = config.user.trimmingCharacters(in: .whitespaces)
        config.name = config.name.trimmingCharacters(in: .whitespaces)
        config.port = UInt16(form.port) ?? 9000
        model.saveDevice(config, password: form.password.isEmpty ? nil : form.password)
        form.password = ""
        form.isNew = false
        selection = config.key
    }
}

/// "Find Devices on This Network" (docs/UX.md "Finding devices"): a search
/// starts when this section appears and stops when it goes away (or the
/// Settings window closes); Search Again restarts it. Choosing a row fills
/// the add form's address and port.
struct FindDevicesSection: View {
    let model: AppModel

    var body: some View {
        let list = model.discovery
        Section("Find Devices on This Network") {
            if list.devices.isEmpty, !list.isSearching {
                Text(emptyText(list)).font(.caption).foregroundStyle(.secondary)
            }
            ForEach(list.devices) { found in row(found) }
            HStack {
                if list.isSearching {
                    ProgressView().controlSize(.small)
                    Text("Searching\u{2026}").font(.caption).foregroundStyle(.secondary)
                } else if !list.devices.isEmpty {
                    Text(list.devices.count == 1 ? "1 device found" : "\(list.devices.count) devices found")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if !list.errors.isEmpty {
                    Text("Some network interfaces could not be searched.")
                        .font(.caption).foregroundStyle(.secondary)
                        .help(list.errors.joined(separator: "\n"))
                }
                Spacer()
                Button("Search Again") { model.startDiscovery() }
                    .accessibilityLabel("Search Again")
            }
        }
        .onAppear {
            model.discoveryPanelShown = true
            model.startDiscovery()
        }
        .onDisappear {
            model.discoveryPanelShown = false
            model.stopDiscovery()
        }
    }

    private func emptyText(_ list: DiscoveryList) -> String {
        if list.unavailable { return "This build cannot search the network for devices." }
        if list.permissionLikelyBlocked {
            return
                "Nothing answered, and macOS refused to send on the local network. Allow NVRView in System Settings \u{203A} Privacy & Security \u{203A} Local Network, then search again. You can also type the address below."
        }
        if list.foundNothing {
            return
                "No devices found. If macOS asked to allow Local Network access, allow it in System Settings \u{203A} Privacy & Security \u{203A} Local Network, then search again. You can also type the address below."
        }
        return "Devices that answer on this network appear here."
    }

    private func row(_ found: DiscoveredDevice) -> some View {
        let added = model.isAdded(found)
        return Button {
            model.chooseDiscovered(found)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: Self.icon(found.kind)).frame(width: 20).foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 1) {
                    Text(found.title)
                    Text(Self.detail(found)).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if let c = found.channels {
                    Text(c == 1 ? "1 channel" : "\(c) channels").font(.caption).foregroundStyle(.secondary)
                }
                if let b = found.batteryPercent {
                    Text("\(b)%").font(.caption).foregroundStyle(.secondary)
                }
                if added {
                    Text("Added").font(.caption).foregroundStyle(.secondary)
                } else if !found.isNative {
                    Text("Not supported yet").font(.caption).foregroundStyle(.secondary)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(added || !found.isNative)
        .accessibilityElement(children: .combine)
    }

    static func icon(_ kind: DiscoveredDevice.Kind) -> String {
        switch kind {
        case .nvr: return "server.rack"
        case .camera: return "video"
        case .unknown: return "questionmark.circle"
        }
    }

    /// Model and address, as the list shows them.
    static func detail(_ d: DiscoveredDevice) -> String {
        var parts: [String] = []
        if let m = d.model, !m.isEmpty { parts.append(m) }
        parts.append(d.port.map { "\(d.address):\($0)" } ?? d.address)
        return parts.joined(separator: " \u{00B7} ")
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
                    "Talk-back sends this Mac's microphone, or a typed reply spoken by the system voice, through a doorbell's speaker. It is off for every doorbell until allowed here, starts only when you press Talk or send a reply, is rate-limited by the core, and every use is logged. Your own pan, tilt, zoom and preset controls on a tile need no setting; the one for a camera that moves lets automation (automatic correction) move it, and is off until allowed here."
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
            if model.canMove(camera) {
                Toggle(
                    "Camera that moves: let automatic correction move it",
                    isOn: Binding(
                        get: { o.motionGrants.contains(MotionClient.autoCorrection) },
                        set: { model.setMotionGrant(camera, client: MotionClient.autoCorrection, $0) }))
            }
            Picker(
                "Calls open in",
                selection: Binding(
                    get: { o.callPresentation?.rawValue ?? "" },
                    set: { model.setCallPresentation(camera, CallPresentation(rawValue: $0)) })
            ) {
                Text("Default (\(model.callsOpenInWindow ? "window" : "inset"))").tag("")
                Text("Window").tag(CallPresentation.window.rawValue)
                Text("Inset").tag(CallPresentation.inset.rawValue)
            }
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
    /// The consent note shown the first time "Record calls" is turned on.
    @State private var askingConsent = false

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
                Stepper(
                    "Inset rows: \(model.insetGrid.rows)",
                    value: Binding(
                        get: { model.insetGrid.rows },
                        set: { model.insetGrid = InsetGridSetting(rows: $0, columns: model.insetGrid.columns) }),
                    in: InsetGridSetting.rowRange)
                Stepper(
                    "Inset columns: \(model.insetGrid.columns)",
                    value: Binding(
                        get: { model.insetGrid.columns },
                        set: { model.insetGrid = InsetGridSetting(rows: model.insetGrid.rows, columns: $0) }),
                    in: InsetGridSetting.columnRange)
                Text(
                    "Up to rows x columns insets show at once; the rest wait behind a \"+N more\" chip. A window can use its own grid: right-click an inset, Inset Grid. The size follows the window, so a small window shows fewer columns."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                Text(
                    "When a doorbell rings, a smoke or CO alarm is heard, or a person is detected on a camera marked as an entrance, and that camera is not on screen, its live video appears in an inset. Click it (or press Tab) to swap it with the stage; Esc closes it."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Section("Doorbell calls") {
                Toggle(
                    "Open calls in their own window",
                    isOn: Binding(get: { model.callsOpenInWindow }, set: { model.callsOpenInWindow = $0 }))
                Picker(
                    "Open call windows",
                    selection: Binding(get: { model.callWindowPlacement }, set: { model.callWindowPlacement = $0 })
                ) {
                    ForEach(CallWindowPlacement.Mode.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Text(
                    "A ring or Start Conversation opens the call in a small window of its own instead of the viewer windows' insets (each camera can choose otherwise in Settings > Cameras). A call window opened by a ring never takes the keyboard from what you are doing."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                HStack {
                    Toggle(
                        "Save call transcripts",
                        isOn: Binding(get: { model.saveCallTranscripts }, set: { model.saveCallTranscripts = $0 }))
                    Spacer()
                    Button("Open Transcripts Folder") { model.transcripts.openFolder() }
                }
                Text(
                    "Calls are transcribed on this Mac while they last. With saving on, each call's transcript goes into the event log and into a text file in the Transcripts folder (the newest 500 files, at most 50 MB in all, are kept). With saving off (the default), the transcript is gone when the call ends. Recording a conversation may need the other person's consent where you live."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                HStack {
                    Toggle(
                        "Record calls",
                        isOn: Binding(
                            get: { model.recordCalls },
                            set: { on in
                                if on && !model.recordCallsConsentShown { askingConsent = true } else { model.recordCalls = on }
                            }))
                    Spacer()
                    Button("Open Call Recordings Folder") { model.transcripts.openFolder() }
                }
                Text(
                    "A call is recorded from the moment it opens: the doorbell's video and three sound tracks, the conversation, the visitor, and you. The call's Record button (⇧⌘R) starts or stops a recording at any time, with this on or off. Recordings go beside the transcripts, with the same name (the newest 100, at most 10 GB in all, are kept)."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .alert("Record calls?", isPresented: $askingConsent) {
            Button("Record Calls") {
                model.recordCallsConsentShown = true
                model.recordCalls = true
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(AppModel.recordCallsConsent)
        }
    }
}

/// The one settings window.
@MainActor
final class SettingsWindowController: NSWindowController {
    private let model: AppModel
    private var closing: NSObjectProtocol?
    /// The window was closed since it was last shown (its search stopped).
    private var wasClosed = false

    init(model: AppModel) {
        self.model = model
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: SettingsLayout.minWidth, height: SettingsLayout.minHeight), styleMask: [.titled, .closable, .resizable],
            backing: .buffered, defer: false)
        window.title = "Settings"
        window.contentViewController = NSHostingController(rootView: SettingsView(model: model))
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.center()
        // Closing the window ends the network search (its view may not
        // disappear when the window is only ordered out).
        closing = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: window, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.wasClosed = true
                self?.model.stopDiscovery()
            }
        }
    }

    /// Shown again after a close: the add-device screen searches again.
    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        if wasClosed {
            wasClosed = false
            if model.discoveryPanelShown { model.startDiscovery() }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }
}
