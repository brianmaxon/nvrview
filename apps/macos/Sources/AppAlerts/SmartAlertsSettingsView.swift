import AppKit
import NVRCore
import SwiftUI
import AppBase

// Settings > Smart Alerts (docs/UX.md "Smart alerts"): turning them on,
// night, the always-alert rules, each camera's exposure, classes, schedule
// and zones, the MQTT sensors and the cameras that confirm them, the
// outputs (desktop, Home Assistant), and where live checks show. Every
// edit goes through `SmartAlertsModel.update` (saved, then sent to
// the core); passwords and tokens go to the Keychain only.

package enum SmartAlertsSection: String, CaseIterable, Identifiable {
    case general = "General", cameras = "Cameras", sensors = "Sensors", outputs = "Outputs", view = "Live View"
    package var id: String { rawValue }
}

package struct SmartAlertsSettingsView: View {
    package let model: any AlertsHost
    @State private var section: SmartAlertsSection = .general

    private var alerts: SmartAlertsModel { model.alertsModel }

    package var body: some View {
        VStack(spacing: 0) {
            Picker("", selection: $section) {
                ForEach(SmartAlertsSection.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(8)
            Divider()
            switch section {
            case .general: SmartAlertsGeneralView(alerts: alerts)
            case .cameras: SmartAlertsCamerasView(model: model)
            case .sensors: SmartAlertsSensorsView(model: model)
            case .outputs: SmartAlertsOutputsView(model: model)
            case .view: SmartAlertsLiveViewSettings(alerts: alerts)
            }
        }
        .onAppear { alerts.refreshStatus() }
    }

    package init(model: any AlertsHost) {
        self.model = model
    }
}

/// A binding to one part of the settings.
@MainActor
private func setting<T>(_ alerts: SmartAlertsModel, _ path: WritableKeyPath<SmartAlertSettings, T>) -> Binding<T> {
    Binding(get: { alerts.settings[keyPath: path] }, set: { v in alerts.update { $0[keyPath: path] = v } })
}

private func caption(_ text: String) -> some View {
    Text(text).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
}

/// Minutes after midnight as a time picker's date (today, local).
private func minuteBinding(_ b: Binding<Int>) -> Binding<Date> {
    Binding(
        get: { Calendar.current.startOfDay(for: Date()).addingTimeInterval(TimeInterval(b.wrappedValue * 60)) },
        set: { d in
            let c = Calendar.current.dateComponents([.hour, .minute], from: d)
            b.wrappedValue = (c.hour ?? 0) * 60 + (c.minute ?? 0)
        })
}

// MARK: General

package struct SmartAlertsGeneralView: View {
    package let alerts: SmartAlertsModel

    private var editor: NightLocationEditor { alerts.locationEditor }

    package var body: some View {
        Form {
            Section {
                Toggle("Smart alerts", isOn: setting(alerts, \.enabled))
                caption(
                    "A camera's own AI, a smoke or CO alarm, or a sensor starts a check: this Mac looks at the camera's main stream, confirms what is there, and only then alerts, saying why. Everything runs on this Mac; nothing leaves the local network unless you set up an output that sends it."
                )
                if let error = alerts.sendError ?? alerts.hostProblem { Text(error).foregroundStyle(.red) }
                if let issues = alerts.configured?["issues"]?.array?.compactMap(\.string), !issues.isEmpty {
                    caption("Adjusted: " + issues.joined(separator: "; "))
                }
            }
            Section("Always alert") {
                Toggle("Smoke and CO alarms", isOn: setting(alerts, \.alwaysAlert.smokeCO))
                Toggle("A verified person on an outside camera at night", isOn: setting(alerts, \.alwaysAlert.nightOutsidePerson))
                Toggle("A doorbell press at night", isOn: setting(alerts, \.alwaysAlert.doorbellNight))
                caption(
                    "These alert whatever the schedules, learning and feedback say. Only muting one camera (at most 24 hours) stops them."
                )
            }
            Section("Night") {
                Toggle(
                    "Use a location for sunset and sunrise",
                    isOn: Binding(get: { editor.isOn }, set: { editor.setOn($0) }))
                if editor.isOn {
                    @Bindable var editor = editor
                    HStack {
                        TextField("Latitude", text: $editor.latText, prompt: Text("12.3 or 12.3° N"))
                            .onSubmit(editor.set)
                        TextField("Longitude", text: $editor.lonText, prompt: Text("-45.6 or 45.6° W"))
                            .onSubmit(editor.set)
                        Button("Set", action: editor.set)
                    }
                    if let message = editor.message {
                        Text(message).font(.caption).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
                    }
                    caption(
                        "You can also paste \"lat, lon\" into either field. Rounded to 0.1° (about 11 km) and kept on this Mac, used only for sunset and sunrise. Night is from \(alerts.settings.night.afterSunsetMin) minutes after sunset to \(alerts.settings.night.beforeSunriseMin) minutes before sunrise."
                    )
                    if alerts.settings.location == nil {
                        caption("No location is set yet, so the night hours below are in use.")
                    } else {
                        SunTimesLines(alerts: alerts)
                    }
                }
                if alerts.settings.location == nil {
                    DatePicker(
                        "Night starts", selection: minuteBinding(setting(alerts, \.night.fallbackStartMin)),
                        displayedComponents: .hourAndMinute)
                    DatePicker(
                        "Night ends", selection: minuteBinding(setting(alerts, \.night.fallbackEndMin)),
                        displayedComponents: .hourAndMinute)
                }
                Picker("Mode", selection: setting(alerts, \.mode)) {
                    ForEach(AlertMode.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                caption("Home: people on inside cameras are expected. Away: everything one step more urgent. Night: treated as night whatever the sun does.")
            }
            Section("Review") {
                Picker("Keep checked pictures for", selection: setting(alerts, \.evidenceRetentionMinutes)) {
                    ForEach([30, 60, 120, 360, 720, 1440], id: \.self) { m in
                        Text(m < 60 ? "\(m) minutes" : m == 60 ? "1 hour" : "\(m / 60) hours").tag(m)
                    }
                }
                Picker("Keep night pictures until", selection: setting(alerts, \.evidenceNightHoldHours)) {
                    ForEach([0, 1, 2, 4, 6, 8, 12], id: \.self) { h in
                        Text(h == 0 ? "the night ends" : h == 1 ? "1 hour after the night ends" : "\(h) hours after the night ends").tag(h)
                    }
                }
                caption(
                    "For checks of an outdoor person, animal or vehicle, the pictures and what each method found are kept on this computer so you can see in Smart Alert Events why an alert was or was not raised. They are never sent anywhere. Pictures made during the night are kept until the time above (so the night can be reviewed in the morning, if that is later than the first setting); the rest are deleted when the first time is up. Every detection is kept, including those inside a cooldown that did not notify you. At most 600 sets or 400 MB are kept, oldest first. Keep in the Events window saves one for 7 days."
                )
            }
            Section("Alerting") {
                Picker("Lowest severity that alerts", selection: setting(alerts, \.minAlertSeverity)) {
                    ForEach([AlertSeverity.low, .normal, .high], id: \.self) { Text($0.title).tag($0) }
                }
                caption("Anything lower goes to the event log only.")
                Toggle("Switch to standby automatically on battery", isOn: setting(alerts, \.standbyOnBattery))
            }
            Section("Advanced: checking a camera's own detections") {
                Stepper(
                    "Pictures with a hit needed: \(alerts.settings.verification.k)",
                    value: setting(alerts, \.verification.k), in: 1...4)
                HStack {
                    Text("Score floor")
                    Slider(value: setting(alerts, \.verification.minScore), in: 0.2...0.9, step: 0.05)
                    Text(String(format: "%.2f", alerts.settings.verification.minScore)).monospacedDigit()
                }
                Stepper(
                    "Look from \(alerts.settings.verification.windowBeforeS) s before",
                    value: setting(alerts, \.verification.windowBeforeS), in: 0...10)
                Stepper(
                    "Look until \(alerts.settings.verification.windowAfterS) s after",
                    value: setting(alerts, \.verification.windowAfterS), in: 2...30)
            }
        }
        .formStyle(.grouped)
        .onAppear { editor.load() }
    }

    package init(alerts: SmartAlertsModel) {
        self.alerts = alerts
    }
}

/// Today's predicted sunrise, sunset, and night window, in local time. It
/// follows the location and the night margins (both are read here), and a
/// new minute of the clock refreshes it, which covers midnight.
private struct SunTimesLines: View {
    package let alerts: SmartAlertsModel

    package var body: some View {
        TimelineView(.everyMinute) { context in
            let night = alerts.settings.night
            let _ = (alerts.settings.location, night.afterSunsetMin, night.beforeSunriseMin)
            if let times = alerts.sunTimes(now: context.date) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(times.lines(), id: \.self) { Text($0).font(.callout).monospacedDigit() }
                }
                .accessibilityIdentifier("sun-times")
            }
        }
    }

    package init(alerts: SmartAlertsModel) {
        self.alerts = alerts
    }
}

// MARK: Cameras

package struct SmartAlertsCamerasView: View {
    package let model: any AlertsHost
    @State private var selection: CameraRef?
    @State private var editingZone: Int?

    private var alerts: SmartAlertsModel { model.alertsModel }

    package var body: some View {
        HStack(spacing: 0) {
            List(selection: $selection) {
                ForEach(model.devices) { d in
                    Section(d.config.name) {
                        ForEach(model.listedChannels(d), id: \.channel) { c in
                            ForEach(model.cameras(device: d.key, channel: c), id: \.self) { cam in
                                Text(model.name(of: cam)).tag(cam)
                            }
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            .frame(width: 200)
            Divider()
            if let camera = selection {
                cameraForm(camera)
            } else {
                Text("Choose a camera").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .sheet(
            isPresented: Binding(get: { editingZone != nil && selection != nil }, set: { if !$0 { editingZone = nil } })
        ) {
            if let camera = selection, let i = editingZone {
                RegionEditorSheet(
                    alerts: alerts, camera: camera, title: "Zone on \(model.name(of: camera))",
                    points: alerts.settings.camera(camera).zones.indices.contains(i) ? alerts.settings.camera(camera).zones[i].points : [],
                    onSave: { points in
                        update(camera) { c in
                            if c.zones.indices.contains(i) { c.zones[i].points = points } else { c.zones.append(AlertZone(points: points)) }
                            c.zones.removeAll { $0.points.count < 3 }
                        }
                        editingZone = nil
                    }, onCancel: { editingZone = nil })
            }
        }
    }

    private func update(_ camera: CameraRef, _ body: @escaping (inout AlertCameraSettings) -> Void) {
        alerts.update { s in
            var c = s.camera(camera)
            body(&c)
            s.setCamera(c)
        }
    }

    private func binding<T>(_ camera: CameraRef, _ path: WritableKeyPath<AlertCameraSettings, T>) -> Binding<T> {
        Binding(get: { alerts.settings.camera(camera)[keyPath: path] }, set: { v in update(camera) { $0[keyPath: path] = v } })
    }

    @ViewBuilder
    private func cameraForm(_ camera: CameraRef) -> some View {
        let c = alerts.settings.camera(camera)
        Form {
            Section(model.name(of: camera)) {
                Picker("Exposure", selection: binding(camera, \.exposure)) {
                    ForEach(AlertExposure.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                caption("Outside and perimeter cameras count as outside for the always-alert rules.")
                Toggle(
                    "Entrance", isOn: Binding(get: { model.cameraOptions(camera).entrance }, set: { model.setEntrance(camera, $0) }))
                Toggle("Trust the camera's AI", isOn: binding(camera, \.trustCameraAI))
                caption("When this Mac's check sees nothing on an always-alert case, alert anyway, marked unverified.")
                Toggle("Check motion without a class", isOn: binding(camera, \.verifyMotion))
            }
            Section("Schedule") {
                Picker(
                    "Alerts",
                    selection: Binding(
                        get: { scheduleKind(c.schedule) },
                        set: { kind in
                            update(camera) { c in
                                c.schedule =
                                    switch kind {
                                    case 1: .night
                                    case 2: .windows([AlertDailyWindow(startMin: 22 * 60, endMin: 6 * 60)])
                                    default: .always
                                    }
                            }
                        })
                ) {
                    Text("Always").tag(0)
                    Text("Only at night").tag(1)
                    Text("At these times").tag(2)
                }
                if case .windows(let windows) = c.schedule {
                    ForEach(windows.indices, id: \.self) { i in windowRow(camera, windows, i) }
                    Button("Add Time Window") {
                        update(camera) { c in
                            if case .windows(var w) = c.schedule {
                                w.append(AlertDailyWindow(startMin: 9 * 60, endMin: 17 * 60))
                                c.schedule = .windows(w)
                            }
                        }
                    }
                }
                caption("Always-alert rules ignore the schedule.")
            }
            Section("Classes") {
                ForEach(AlertClass.allCases, id: \.self) { cls in
                    Toggle(
                        cls.title,
                        isOn: Binding(
                            get: { alerts.settings.camera(camera).classes.contains(cls) },
                            set: { on in
                                update(camera) { c in
                                    c.classes.removeAll { $0 == cls }
                                    if on { c.classes.append(cls) }
                                }
                            }))
                }
            }
            Section("Zones") {
                ForEach(c.zones.indices, id: \.self) { i in
                    HStack {
                        Text("Zone \(i + 1): \(c.zones[i].points.count) corners")
                        Picker(
                            "", selection: Binding(get: { c.zones[i].include }, set: { v in update(camera) { $0.zones[i].include = v } })
                        ) {
                            Text("Alert only here").tag(true)
                            Text("Never here (mask)").tag(false)
                        }
                        .labelsHidden()
                        .frame(width: 160)
                        Button("Edit…") { editingZone = i }
                        Button("Remove") { update(camera) { $0.zones.remove(at: i) } }
                    }
                }
                Button("Draw a Zone…") { editingZone = c.zones.count }
                caption("Drawn on a still of the camera. A box counts where its feet are.")
            }
        }
        .formStyle(.grouped)
    }

    private func scheduleKind(_ s: AlertSchedule) -> Int {
        switch s {
        case .always: 0
        case .night: 1
        case .windows: 2
        }
    }

    private func windowRow(_ camera: CameraRef, _ windows: [AlertDailyWindow], _ i: Int) -> some View {
        let days = ["M", "T", "W", "T", "F", "S", "S"]
        func set(_ body: @escaping (inout AlertDailyWindow) -> Void) {
            update(camera) { c in
                if case .windows(var w) = c.schedule, w.indices.contains(i) {
                    body(&w[i])
                    c.schedule = .windows(w)
                }
            }
        }
        return HStack(spacing: 4) {
            ForEach(0..<7, id: \.self) { d in
                Toggle(
                    days[d],
                    isOn: Binding(
                        get: { windows[i].days & (1 << d) != 0 }, set: { on in set { $0.days = on ? $0.days | (1 << d) : $0.days & ~(1 << d) } })
                )
                .toggleStyle(.button)
            }
            DatePicker(
                "", selection: minuteBinding(Binding(get: { windows[i].startMin }, set: { v in set { $0.startMin = v } })),
                displayedComponents: .hourAndMinute
            )
            .labelsHidden()
            Text("to")
            DatePicker(
                "", selection: minuteBinding(Binding(get: { windows[i].endMin }, set: { v in set { $0.endMin = v } })),
                displayedComponents: .hourAndMinute
            )
            .labelsHidden()
            Button {
                update(camera) { c in
                    if case .windows(var w) = c.schedule, w.indices.contains(i) {
                        w.remove(at: i)
                        c.schedule = .windows(w)
                    }
                }
            } label: {
                Image(systemName: "minus.circle")
            }
            .buttonStyle(.borderless)
            .accessibilityLabel("Remove Time Window")
        }
    }

    package init(model: any AlertsHost) {
        self.model = model
    }
}

// MARK: Sensors

package struct SmartAlertsSensorsView: View {
    package let model: any AlertsHost
    @State private var selection: String?
    @State private var password = ""
    @State private var regionFor: CameraRef?

    private var alerts: SmartAlertsModel { model.alertsModel }

    package var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                List(selection: $selection) {
                    ForEach(alerts.settings.sensors) { s in
                        Text(s.name.isEmpty ? s.key : s.name).tag(s.key)
                    }
                }
                .listStyle(.bordered)
                HStack(spacing: 4) {
                    Button {
                        let key = MQTTSensor.newKey(taken: alerts.settings.sensors.map(\.key))
                        alerts.update { $0.sensors.append(MQTTSensor(key: key, name: "Sensor \($0.sensors.count + 1)")) }
                        selection = key
                    } label: {
                        Image(systemName: "plus").frame(width: 16, height: 16)
                    }
                    .accessibilityLabel("Add Sensor")
                    Button {
                        if let key = selection { alerts.removeSensor(key) }
                        selection = nil
                    } label: {
                        Image(systemName: "minus").frame(width: 16, height: 16)
                    }
                    .disabled(selection == nil)
                    .accessibilityLabel("Remove Sensor")
                    Spacer()
                }
                .buttonStyle(.borderless)
                .padding(6)
            }
            .frame(width: 180)
            Divider()
            if let key = selection, let i = alerts.settings.sensors.firstIndex(where: { $0.key == key }) {
                sensorForm(i).id(key)
            } else {
                VStack(spacing: 8) {
                    Text("No sensor chosen").foregroundStyle(.secondary)
                    caption(
                        "Add a motion, infrared or beam sensor, or a door contact, that your home automation publishes to your own MQTT broker. A sensor never alerts on its own: the cameras that see its area must confirm what is there."
                    )
                    .frame(maxWidth: 360)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .sheet(isPresented: Binding(get: { regionFor != nil }, set: { if !$0 { regionFor = nil } })) {
            if let camera = regionFor, let key = selection, let i = alerts.settings.sensors.firstIndex(where: { $0.key == key }) {
                RegionEditorSheet(
                    alerts: alerts, camera: camera, title: "What \(model.name(of: camera)) sees of the sensor's area",
                    points: alerts.settings.sensors[i].cameras.first { $0.camera == camera }?.roi ?? [],
                    onSave: { points in
                        alerts.update { s in
                            if let j = s.sensors[i].cameras.firstIndex(where: { $0.camera == camera }) {
                                s.sensors[i].cameras[j].roi = points
                            }
                        }
                        regionFor = nil
                    }, onCancel: { regionFor = nil })
            }
        }
    }

    private func b<T>(_ i: Int, _ path: WritableKeyPath<MQTTSensor, T>) -> Binding<T> {
        Binding(
            get: { alerts.settings.sensors.indices.contains(i) ? alerts.settings.sensors[i][keyPath: path] : MQTTSensor(key: "", name: "")[keyPath: path] },
            set: { v in alerts.update { if $0.sensors.indices.contains(i) { $0.sensors[i][keyPath: path] = v } } })
    }

    private func list(_ i: Int, _ path: WritableKeyPath<MQTTSensor, [String]>) -> Binding<String> {
        Binding(
            get: { alerts.settings.sensors[i][keyPath: path].joined(separator: ", ") },
            set: { v in
                let items = v.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                alerts.update { $0.sensors[i][keyPath: path] = items }
            })
    }

    private func intText(_ i: Int, _ path: WritableKeyPath<MQTTSensor, Int>) -> Binding<String> {
        Binding(get: { String(alerts.settings.sensors[i][keyPath: path]) }, set: { v in if let n = Int(v) { alerts.update { $0.sensors[i][keyPath: path] = n } } })
    }

    @ViewBuilder
    private func sensorForm(_ i: Int) -> some View {
        let s = alerts.settings.sensors[i]
        Form {
            Section("Sensor") {
                TextField("Name", text: b(i, \.name))
                LabeledContent("Key", value: s.key)
                TextField("Trigger kind", text: b(i, \.triggerKind), prompt: Text("sensor_motion"))
                if let status = alerts.sensorStatus(i) { LabeledContent("Status", value: status) }
                HStack {
                    Button("Sensor Messages…") { alerts.showSensorMessages(s.key) }
                    Menu("Simulate Trigger") {
                        Button("As if the sensor fired") {
                            alerts.simulate(sensor: s.key, sendToHomeAssistant: alerts.simulateToHomeAssistant)
                        }
                        if !s.cameras.isEmpty { Divider() }
                        ForEach(s.cameras) { sc in
                            if let camera = model.alertCamera(sc.camera) {
                                Button("Only for \(model.name(of: sc.camera))") {
                                    alerts.simulate(
                                        sensor: s.key, camera: camera, sendToHomeAssistant: alerts.simulateToHomeAssistant)
                                }
                            }
                        }
                    }
                    .fixedSize()
                }
                Toggle(
                    "Also send simulated triggers to Home Assistant",
                    isOn: Binding(get: { alerts.simulateToHomeAssistant }, set: { alerts.simulateToHomeAssistant = $0 }))
                if let note = alerts.simulateNote { caption(note) }
                caption(
                    "Simulate Trigger runs the whole chain: the cameras wake, the pictures are checked, and the decision is made. The decision is marked Test in Smart Alert Events. Home Assistant gets it only if the box is ticked."
                )
            }
            Section("Broker") {
                TextField("Address", text: b(i, \.host), prompt: Text("broker.local"))
                TextField("Port", text: intText(i, \.port))
                Toggle("TLS", isOn: b(i, \.tls))
                if s.tls {
                    VStack(alignment: .leading) {
                        Text("Broker CA (PEM; empty uses the public roots)").font(.caption)
                        TextEditor(text: b(i, \.caPEM)).font(.system(.caption, design: .monospaced)).frame(height: 60)
                    }
                }
                TextField(
                    "User name", text: Binding(get: { s.username ?? "" }, set: { v in alerts.update { $0.sensors[i].username = v.isEmpty ? nil : v } }),
                    prompt: Text("None"))
                SecureField(
                    "Password", text: $password,
                    prompt: Text(alerts.hasSecret(.mqttPassword(s.key)) ? "Saved in the Keychain" : "None"))
                HStack {
                    caption("The password is stored only in the macOS Keychain, never in a file. A login the broker rejects is not retried.")
                    Spacer()
                    Button("Save Password") {
                        alerts.setSecret(password, for: .mqttPassword(s.key))
                        password = ""
                    }
                    .disabled(password.isEmpty)
                }
                if !AlertEndpoint.hostIsLocal(s.host), !s.host.isEmpty {
                    Toggle("Allow a broker outside the local network", isOn: b(i, \.allowPublic))
                }
            }
            Section("Message") {
                TextField("Topic", text: b(i, \.topic), prompt: Text("example/sensors/motion"))
                Toggle("Allow wildcards (+, #) in the topic", isOn: b(i, \.allowWildcards))
                Picker("Payload", selection: b(i, \.payload)) {
                    Text("ON or OFF").tag(MQTTPayloadFormat.plain)
                    Text("JSON").tag(MQTTPayloadFormat.json)
                }
                if s.payload == .json { TextField("Path to the value", text: b(i, \.jsonPath), prompt: Text("state")) }
                TextField("On values", text: list(i, \.onValues))
                TextField("Off values", text: list(i, \.offValues))
                Picker("Each change", selection: b(i, \.pulse)) {
                    Text("On starts, off stops").tag(false)
                    Text("Every on is one pulse").tag(true)
                }
                Picker("Retained messages", selection: b(i, \.retained)) {
                    Text("Ignore").tag("ignore")
                    Text("Seed the state").tag("seed")
                    Text("Treat like any message").tag("accept")
                }
                TextField("Debounce (ms)", text: intText(i, \.debounceMs))
            }
            Section("Cameras that see the sensor's area") {
                ForEach(s.cameras) { sc in
                    HStack {
                        Text(model.name(of: sc.camera))
                        Spacer()
                        Text(sc.roi.count >= 3 ? "Region: \(sc.roi.count) corners" : "Whole picture").foregroundStyle(.secondary)
                        Button("Draw Region…") { regionFor = sc.camera }
                        Stepper(
                            "Weight \(String(format: "%.1f", sc.weight))",
                            value: Binding(
                                get: { sc.weight },
                                set: { v in
                                    alerts.update { st in
                                        if let j = st.sensors[i].cameras.firstIndex(where: { $0.camera == sc.camera }) {
                                            st.sensors[i].cameras[j].weight = v
                                        }
                                    }
                                }), in: 0.5...2, step: 0.5)
                        Button {
                            alerts.update { $0.sensors[i].cameras.removeAll { $0.camera == sc.camera } }
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel("Remove Camera")
                    }
                }
                Menu("Add Camera") {
                    ForEach(model.allCameras.filter { cam in !s.cameras.contains { $0.camera == cam } }, id: \.self) { cam in
                        Button(model.name(of: cam)) { alerts.update { $0.sensors[i].cameras.append(SensorCamera(camera: cam)) } }
                    }
                }
                if s.cameras.isEmpty, s.requireVerified {
                    Text("Choose at least one camera: a sensor alerts only when a camera confirms it.").foregroundStyle(.orange)
                }
            }
            Section("Confirmation") {
                Stepper("Look from \(s.windowBeforeMs / 1000) s before", value: b(i, \.windowBeforeMs), in: 0...10000, step: 1000)
                Stepper("Look until \(s.windowAfterMs / 1000) s after", value: b(i, \.windowAfterMs), in: 1000...30000, step: 1000)
                Stepper(
                    "Retriggers extend it to at most \(s.windowMaxMs / 1000) s", value: b(i, \.windowMaxMs), in: 5000...120000, step: 5000)
                ForEach(AlertClass.detector, id: \.self) { cls in
                    Toggle(
                        cls.title,
                        isOn: Binding(
                            get: { s.classes.contains(cls) },
                            set: { on in
                                alerts.update { st in
                                    st.sensors[i].classes.removeAll { $0 == cls }
                                    if on { st.sensors[i].classes.append(cls) }
                                }
                            }))
                }
                Stepper("Pictures needed (weighted): \(String(format: "%.1f", s.k))", value: b(i, \.k), in: 1...6, step: 0.5)
                Toggle("Alert unverified when no picture could be checked", isOn: b(i, \.alertWhenUnavailable))
                Toggle("Require a camera to confirm", isOn: b(i, \.requireVerified))
                if !s.requireVerified {
                    Text("This sensor alerts on its own, marked unverified.").foregroundStyle(.orange)
                }
            }
        }
        .formStyle(.grouped)
    }

    package init(model: any AlertsHost) {
        self.model = model
    }
}

// MARK: Outputs

package struct SmartAlertsOutputsView: View {
    package let model: any AlertsHost
    @State private var token = ""
    @State private var confirmTestAlert = false

    private var alerts: SmartAlertsModel { model.alertsModel }

    package var body: some View {
        let ha = alerts.settings.homeAssistant
        Form {
            Section("Desktop notifications") {
                Toggle("Notify on this Mac", isOn: setting(alerts, \.desktop.enabled))
                Picker("From severity", selection: setting(alerts, \.desktop.minSeverity)) {
                    ForEach(AlertSeverity.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Toggle("With a picture", isOn: setting(alerts, \.desktop.pictures))
            }
            Section("Home Assistant") {
                Toggle("Send alerts to Home Assistant", isOn: setting(alerts, \.homeAssistant.enabled))
                TextField("Address", text: setting(alerts, \.homeAssistant.baseURL), prompt: Text("http://homeassistant.local:8123"))
                if ha.enabled, let why = AlertEndpoint.checkHomeAssistantURL(ha.baseURL) {
                    Text(why).foregroundStyle(.orange)
                }
                HStack {
                    SecureField(
                        "Access token", text: $token,
                        prompt: Text(alerts.hasSecret(.homeAssistantToken) ? "Saved in the Keychain" : "A long-lived access token"))
                    Button("Save") {
                        alerts.setSecret(token, for: .homeAssistantToken)
                        token = ""
                    }
                    .disabled(token.isEmpty)
                    Button("Test") { alerts.testHomeAssistant() }
                    Button("Send Test Alert…") { confirmTestAlert = true }
                        .disabled(!ha.enabled || ha.baseURL.isEmpty)
                }
                if let t = alerts.homeAssistantTest { caption(t) }
                if let t = alerts.homeAssistantSend { caption(t) }
                if let status = alerts.homeAssistantStatus { LabeledContent("Status", value: status) }
                Picker("From severity", selection: setting(alerts, \.homeAssistant.minSeverity)) {
                    ForEach(AlertSeverity.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Toggle("With a picture", isOn: setting(alerts, \.homeAssistant.pictures))
                if ha.pictures, AlertEndpoint.mayLeaveNetwork(ha.baseURL), !ha.baseURL.isEmpty {
                    Text("This address may be outside your local network: pictures would leave the house.").foregroundStyle(.orange)
                }
                Picker("Rules", selection: setting(alerts, \.homeAssistant.allMatches)) {
                    Text("First match").tag(false)
                    Text("Every match").tag(true)
                }
                Stepper("Pulse gap \(ha.pulseGapMs / 1000) s", value: setting(alerts, \.homeAssistant.pulseGapMs), in: 1000...10000, step: 1000)
                Stepper("Cooldown \(ha.cooldownMs / 1000) s", value: setting(alerts, \.homeAssistant.cooldownMs), in: 0...600_000, step: 5000)
                caption(
                    "The token is stored only in the macOS Keychain. Test asks Home Assistant's API with it and turns nothing on. A pulse turns a helper on and off again (resetting it first if it was left on), for routines that start when it turns on."
                )
            }
            Section("Home Assistant rules") {
                // (The test alert's confirmation is attached to the form below.)
                ForEach(ha.rules.indices, id: \.self) { i in HARuleRow(model: model, index: i) }
                Button("Add Rule") { alerts.update { $0.homeAssistant.rules.append(HARule()) } }
            }
            let problems = alerts.settings.problems()
            if !problems.isEmpty {
                Section("To fix") {
                    ForEach(problems, id: \.self) { Text($0).foregroundStyle(.orange) }
                }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog(
            "Send a test alert to Home Assistant?", isPresented: $confirmTestAlert, titleVisibility: .visible
        ) {
            Button("Send Test Alert") { alerts.sendHomeAssistantTestAlert() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "This runs the rules below as if an alert had happened: a pulse turns its helper on and off again, a notify rule sends a message, a script runs. Do it when that is safe."
            )
        }
    }

    package init(model: any AlertsHost) {
        self.model = model
    }
}

package struct HARuleRow: View {
    package let model: any AlertsHost
    package let index: Int

    private var alerts: SmartAlertsModel { model.alertsModel }

    private func b<T>(_ path: WritableKeyPath<HARule, T>) -> Binding<T> {
        Binding(
            get: { alerts.settings.homeAssistant.rules.indices.contains(index) ? alerts.settings.homeAssistant.rules[index][keyPath: path] : HARule()[keyPath: path] },
            set: { v in alerts.update { if $0.homeAssistant.rules.indices.contains(index) { $0.homeAssistant.rules[index][keyPath: path] = v } } })
    }

    package var body: some View {
        let r = alerts.settings.homeAssistant.rules[index]
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Picker("", selection: b(\.action)) {
                    ForEach(HAActionKind.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .labelsHidden()
                .frame(width: 170)
                TextField(
                    "", text: b(\.target),
                    prompt: Text(r.action == .notify ? "notify target (for example example_phone)" : r.action == .script ? "script.example" : "input_boolean.example_alert"))
                if r.action == .notify { targetMenu }
                Button {
                    alerts.update { $0.homeAssistant.rules.remove(at: index) }
                } label: {
                    Image(systemName: "minus.circle")
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Remove Rule")
            }
            HStack {
                Menu(r.classes.isEmpty ? "Any class" : r.classes.map(\.title).joined(separator: ", ")) {
                    ForEach(AlertClass.allCases, id: \.self) { cls in
                        Toggle(cls.title, isOn: Binding(get: { r.classes.contains(cls) }, set: { on in toggle(\.classes, cls, on) }))
                    }
                }
                Menu(r.cameras.isEmpty ? "Any camera" : "\(r.cameras.count) camera\(r.cameras.count == 1 ? "" : "s")") {
                    ForEach(model.allCameras, id: \.self) { cam in
                        Toggle(model.name(of: cam), isOn: Binding(get: { r.cameras.contains(cam) }, set: { on in toggle(\.cameras, cam, on) }))
                    }
                }
                Menu(r.sources.isEmpty ? "Any trigger" : "\(r.sources.count) sensor\(r.sources.count == 1 ? "" : "s")") {
                    ForEach(alerts.settings.sensors) { s in
                        Toggle(s.name.isEmpty ? s.key : s.name, isOn: Binding(get: { r.sources.contains(s.key) }, set: { on in toggle(\.sources, s.key, on) }))
                    }
                }
                Picker("From", selection: b(\.minSeverity)) {
                    Text("Any severity").tag(AlertSeverity?.none)
                    ForEach(AlertSeverity.allCases, id: \.self) { Text($0.title).tag(AlertSeverity?.some($0)) }
                }
                .frame(width: 170)
            }
            if let why = HARuleCheck.problem(r), !r.target.isEmpty { Text(why).font(.caption).foregroundStyle(.orange) }
            if r.action == .notify, let status = alerts.notifyTargetsStatus {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    /// The notify targets Home Assistant offers, fetched on demand; typing a
    /// name stays possible.
    @ViewBuilder private var targetMenu: some View {
        Menu {
            if let list = alerts.notifyTargets {
                if !list.entities.isEmpty {
                    Section("Notify entities") {
                        ForEach(list.entities, id: \.self) { t in Button(t.name) { b(\.target).wrappedValue = t.name } }
                    }
                }
                if !list.services.isEmpty {
                    Section("Legacy notify services") {
                        ForEach(list.services, id: \.self) { t in Button(t.name) { b(\.target).wrappedValue = t.name } }
                    }
                }
                Divider()
            }
            Button(alerts.notifyTargets == nil ? "Load from Home Assistant" : "Reload") { alerts.loadNotifyTargets() }
        } label: {
            Image(systemName: "list.bullet")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityLabel("Notify targets")
        .help("Choose a notify target from Home Assistant (it asks with the saved address and token)")
    }

    private func toggle<T: Equatable>(_ path: WritableKeyPath<HARule, [T]>, _ value: T, _ on: Bool) {
        alerts.update { s in
            guard s.homeAssistant.rules.indices.contains(index) else { return }
            s.homeAssistant.rules[index][keyPath: path].removeAll { $0 == value }
            if on { s.homeAssistant.rules[index][keyPath: path].append(value) }
        }
    }

    package init(model: any AlertsHost, index: Int) {
        self.model = model
        self.index = index
    }
}

// MARK: Live view

package struct SmartAlertsLiveViewSettings: View {
    package let alerts: SmartAlertsModel

    package var body: some View {
        Form {
            Section("Live checks") {
                Picker("Show checks", selection: setting(alerts, \.view.placement)) {
                    ForEach(VerifyPlacementSetting.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Toggle("Also keep checks that end quiet or suppressed", isOn: setting(alerts, \.view.showQuiet))
                Picker("Stay after the decision", selection: setting(alerts, \.view.minSeconds)) {
                    ForEach(VerifyViewSettings.stayChoices(including: alerts.settings.view.minSeconds), id: \.self) {
                        Text(VerifyViewSettings.stayTitle($0)).tag($0)
                    }
                }
                caption(
                    "Each check shows the camera's picture with the detector's boxes and every step from the trigger to the decision. It stays longer while the alert is still going, never takes the keyboard, and closes with its × button, Esc, or Dismiss All."
                )
            }
        }
        .formStyle(.grouped)
    }

    package init(alerts: SmartAlertsModel) {
        self.alerts = alerts
    }
}

// MARK: Drawing a region on a still

package struct RegionEditorSheet: View {
    package let alerts: SmartAlertsModel
    package let camera: CameraRef
    package let title: String
    package let points: [ROIPoint]
    package let onSave: ([ROIPoint]) -> Void
    package let onCancel: () -> Void

    @State private var editor = ROIEditor()
    @State private var still: CGImage?
    @State private var loading = true
    @State private var started = false

    package var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            caption("Click to add corners (at least three), drag a corner to move it. A box counts where its feet are.")
            GeometryReader { geo in
                let size = still.map { CGSize(width: $0.width, height: $0.height) } ?? CGSize(width: 16, height: 9)
                let fit = ROIGeometry.fitRect(picture: size, in: geo.size)
                ZStack(alignment: .topLeading) {
                    Color.black
                    if let still {
                        Image(decorative: still, scale: 1).resizable().frame(width: fit.width, height: fit.height)
                            .offset(x: fit.minX, y: fit.minY)
                    } else {
                        Text(loading ? "Getting a picture…" : "No picture from this camera; drawing on a blank frame.")
                            .foregroundStyle(.white.opacity(0.7))
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    Canvas { ctx, _ in
                        let pts = editor.points.map { ROIGeometry.toView($0, fit: fit) }
                        if pts.count >= 2 {
                            var path = Path()
                            path.addLines(pts)
                            if pts.count >= 3 { path.closeSubpath() }
                            ctx.fill(path, with: .color(.yellow.opacity(0.2)))
                            ctx.stroke(path, with: .color(.yellow), lineWidth: 2)
                        }
                        for (i, p) in pts.enumerated() {
                            let r = CGRect(x: p.x - 5, y: p.y - 5, width: 10, height: 10)
                            ctx.fill(Path(ellipseIn: r), with: .color(i == editor.grabbed ? .orange : .yellow))
                        }
                    }
                }
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { g in
                            if !started {
                                started = true
                                editor.press(at: g.startLocation, fit: fit)
                            }
                            editor.drag(to: g.location, fit: fit)
                        }
                        .onEnded { _ in
                            started = false
                            editor.release()
                        })
            }
            .frame(minWidth: 560, minHeight: 315)
            HStack {
                Button("Delete Corner") { editor.deleteCorner() }.disabled(editor.points.isEmpty)
                Button("Clear") { editor.clear() }
                Spacer()
                Text(editor.isClosed ? "\(editor.points.count) corners" : "Whole picture (fewer than three corners)")
                    .font(.caption).foregroundStyle(.secondary)
                Button("Cancel", action: onCancel).keyboardShortcut(.cancelAction)
                Button("Save") { onSave(editor.saved) }.keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(minWidth: 600, minHeight: 440)
        .task {
            editor = ROIEditor(points: points)
            still = await alerts.still(of: camera)
            loading = false
        }
    }

    package init(alerts: SmartAlertsModel, camera: CameraRef, title: String, points: [ROIPoint], onSave: @escaping ([ROIPoint]) -> Void, onCancel: @escaping () -> Void) {
        self.alerts = alerts
        self.camera = camera
        self.title = title
        self.points = points
        self.onSave = onSave
        self.onCancel = onCancel
    }
}
