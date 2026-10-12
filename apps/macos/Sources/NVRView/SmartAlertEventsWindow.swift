import AppKit
import NVRCore
import SwiftUI

// The smart-alert event log (docs/UX.md "Smart alerts", "Events"): Window >
// Smart Alert Events (⌥⌘E). Every decision, alerts, quiet and suppressed
// ones, newest first, with filters; an entry opens its whole trace and
// snapshot, jumps to the recording at that time, takes feedback, and mutes
// its camera.

/// The window's state: the filter, the entries, the one opened.
@MainActor
@Observable
final class SmartAlertEventsModel {
    @ObservationIgnored let app: AppModel
    var filter = SmartAlertLogFilter() {
        didSet { if filter != oldValue { reload() } }
    }
    /// The date range choice (`filter.sinceMs` follows it).
    var range: DateRange = .week {
        didSet { if range != oldValue { applyRange() } }
    }
    private(set) var entries: [SmartAlertLogEntry] = []
    private(set) var more = false
    /// The log could not be read (a core without smart alerts).
    private(set) var unavailable = false
    var selection: SmartAlertLogEntry.ID?
    /// The opened entry's snapshot.
    private(set) var snapshot: NSImage?
    /// What the last action did, in words.
    var note: String?
    /// Opens the recordings window at a camera and time (set by the app
    /// delegate).
    @ObservationIgnored var jump: (CameraRef, Int64) -> Void = { _, _ in }
    @ObservationIgnored private var token: UUID?
    @ObservationIgnored private var reloadQueued = false

    enum DateRange: String, CaseIterable, Identifiable {
        case today = "Today", week = "7 Days", month = "30 Days", all = "All"
        var id: String { rawValue }
    }

    init(app: AppModel) {
        self.app = app
        applyRange()
        token = app.smartAlerts.subscribeEvents { [weak self] e in
            // A new decision: the list follows (once per burst).
            if case .alert = e { self?.queueReload() }
        }
    }

    func close() {
        if let token { app.smartAlerts.unsubscribeEvents(token) }
        token = nil
    }

    private func applyRange() {
        let now = Date()
        switch range {
        case .today: filter.sinceMs = Int64(Calendar.current.startOfDay(for: now).timeIntervalSince1970 * 1000)
        case .week: filter.sinceMs = Int64(now.addingTimeInterval(-7 * 86_400).timeIntervalSince1970 * 1000)
        case .month: filter.sinceMs = Int64(now.addingTimeInterval(-30 * 86_400).timeIntervalSince1970 * 1000)
        case .all: filter.sinceMs = nil
        }
        reload()
    }

    private func queueReload() {
        guard !reloadQueued else { return }
        reloadQueued = true
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                self?.reloadQueued = false
                self?.reload()
            }
        }
    }

    func reload() {
        guard let page = app.smartAlerts.log(filter) else {
            unavailable = true
            entries = []
            more = false
            return
        }
        unavailable = false
        entries = page.entries.filter(filter.matches)
        more = page.more
        if let s = selection, !entries.contains(where: { $0.id == s }) { selection = nil }
        loadSnapshot()
    }

    /// The next page (older entries).
    func loadMore() {
        guard more, let last = entries.last, let page = app.smartAlerts.log(filter, beforeMs: last.decidedMs) else { return }
        entries += page.entries.filter(filter.matches).filter { e in !entries.contains { $0.id == e.id } }
        more = page.more
    }

    var selected: SmartAlertLogEntry? { entries.first { $0.id == selection } }

    func select(_ id: SmartAlertLogEntry.ID?) {
        selection = id
        note = nil
        loadSnapshot()
    }

    private func loadSnapshot() {
        snapshot = selected.flatMap { e in e.hasSnapshot ? e.alertID.flatMap { app.smartAlerts.snapshot(alertID: $0) } : nil }
    }

    func cameraName(_ c: AlertCamera) -> String { app.cameraRef(c).map(app.name(of:)) ?? "Camera \(c.channel + 1)" }

    /// The list's "where": the camera, or the sensor's name.
    func place(_ e: SmartAlertLogEntry) -> String {
        if let c = e.camera { return cameraName(c) }
        if e.sourceGroup == .sensor { return "Sensor" }
        return "—"
    }

    func feedback(_ e: SmartAlertLogEntry, useful: Bool, reason: AlertFeedbackReason? = nil) {
        guard let id = e.alertID else { return }
        if app.smartAlerts.feedback(alertID: id, useful: useful, reason: reason) {
            note = useful ? "Marked useful." : "Marked not useful\(reason.map { ": \($0.title.lowercased())" } ?? "")."
            reload()
        } else {
            note = "The feedback was not taken."
        }
    }

    func mute(_ e: SmartAlertLogEntry, _ choice: AlertMuteChoice) {
        guard let c = e.camera else { return }
        let ok = app.smartAlerts.mute(c, seconds: choice.seconds(now: Date()))
        note = ok ? "\(cameraName(c)) muted (\(choice.title.lowercased()))." : "The camera could not be muted."
    }

    func openRecording(_ e: SmartAlertLogEntry) {
        guard let c = e.camera, let ref = app.cameraRef(c) else { return }
        jump(ref, e.decidedMs)
    }
}

@MainActor
final class SmartAlertEventsWindowController: NSWindowController, NSWindowDelegate {
    let events: SmartAlertEventsModel
    var onClose: (SmartAlertEventsWindowController) -> Void = { _ in }

    init(app: AppModel) {
        events = SmartAlertEventsModel(app: app)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 980, height: 620),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Smart Alert Events"
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("SmartAlertEvents")
        window.contentMinSize = NSSize(width: 720, height: 420)
        super.init(window: window)
        let hosting = NSHostingController(rootView: SmartAlertEventsView(events: events))
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 980, height: 620))
        window.setFrameUsingName("SmartAlertEvents")
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func windowWillClose(_ notification: Notification) {
        events.close()
        onClose(self)
    }
}

struct SmartAlertEventsView: View {
    @Bindable var events: SmartAlertEventsModel

    var body: some View {
        HSplitView {
            VStack(spacing: 0) {
                filters.padding(8)
                Divider()
                list
            }
            .frame(minWidth: 440)
            detail.frame(minWidth: 280, maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var filters: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                TextField("Search explanations", text: $events.filter.text)
                Picker("", selection: $events.range) {
                    ForEach(SmartAlertEventsModel.DateRange.allCases) { Text($0.rawValue).tag($0) }
                }
                .labelsHidden()
                .frame(width: 110)
            }
            HStack {
                Picker("Camera", selection: $events.filter.camera) {
                    Text("Any camera").tag(AlertCamera?.none)
                    ForEach(events.app.allCameras, id: \.self) { cam in
                        if let a = events.app.alertCamera(cam) { Text(events.app.name(of: cam)).tag(AlertCamera?.some(a)) }
                    }
                }
                .frame(maxWidth: 220)
                Picker("Severity", selection: $events.filter.minSeverity) {
                    Text("Any").tag(AlertSeverity?.none)
                    ForEach(AlertSeverity.allCases, id: \.self) { Text("\($0.title) or higher").tag(AlertSeverity?.some($0)) }
                }
                .frame(maxWidth: 200)
            }
            HStack(spacing: 4) {
                ForEach(AlertSourceGroup.allCases, id: \.self) { g in
                    Toggle(g.title, isOn: member(\.sources, g)).toggleStyle(.button)
                }
                Divider().frame(height: 16)
                ForEach(SmartAlertLogFilter.decisionKinds, id: \.self) { d in
                    Toggle(Self.decisionTitle(d), isOn: member(\.decisions, d)).toggleStyle(.button)
                }
                Divider().frame(height: 16)
                Menu(events.filter.classes.isEmpty ? "Any class" : events.filter.classes.sorted().joined(separator: ", ")) {
                    ForEach(AlertClass.allCases.map(\.rawValue) + ["sensor"], id: \.self) { c in
                        Toggle(c, isOn: member(\.classes, c))
                    }
                }
                .fixedSize()
            }
            .controlSize(.small)
        }
    }

    private func member<T: Hashable>(_ path: WritableKeyPath<SmartAlertLogFilter, Set<T>>, _ v: T) -> Binding<Bool> {
        Binding(
            get: { events.filter[keyPath: path].contains(v) },
            set: { on in
                if on { events.filter[keyPath: path].insert(v) } else { events.filter[keyPath: path].remove(v) }
            })
    }

    static func decisionTitle(_ d: String) -> String {
        switch d {
        case "alert": "Alerts"
        case "quiet": "Quiet"
        default: "Suppressed"
        }
    }

    private var list: some View {
        Group {
            if events.unavailable {
                Text("The event log is not available with this core.").foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if events.entries.isEmpty {
                Text("No decisions match.").foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(selection: Binding(get: { events.selection }, set: { events.select($0) })) {
                    ForEach(events.entries) { e in row(e).tag(e.id) }
                    if events.more {
                        Button("Older…") { events.loadMore() }
                    }
                }
            }
        }
    }

    private func row(_ e: SmartAlertLogEntry) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(Self.time(e.decidedMs)).font(.caption.monospacedDigit()).foregroundStyle(.secondary).frame(width: 110, alignment: .leading)
            Text(events.place(e)).frame(width: 110, alignment: .leading).lineLimit(1)
            Text(e.className ?? "—").frame(width: 60, alignment: .leading)
            Text(Self.decisionLabel(e)).foregroundStyle(e.decision == "alert" ? .primary : .secondary).frame(width: 90, alignment: .leading)
            Text(e.firstSentence).lineLimit(1).foregroundStyle(.secondary)
        }
        .font(.callout)
    }

    static func decisionLabel(_ e: SmartAlertLogEntry) -> String {
        switch e.decision {
        case "alert": "Alert" + (e.severity.map { " (\($0))" } ?? "")
        case "quiet": "Quiet"
        default: "Suppressed"
        }
    }

    static func time(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateStyle = .short
        f.timeStyle = .medium
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    @ViewBuilder
    private var detail: some View {
        if let e = events.selected {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    Text("\(Self.decisionLabel(e)) · \(events.place(e)) · \(Self.time(e.decidedMs))").font(.headline)
                    if e.unverified || e.late || e.always != nil {
                        Text(
                            [
                                e.always.map { "Always alerts: \(TraceText.ruleWords($0))" }, e.unverified ? "unverified" : nil,
                                e.late ? "late" : nil,
                            ].compactMap(\.self).joined(separator: " · ")
                        )
                        .font(.caption).foregroundStyle(.orange)
                    }
                    Text(e.explanation).textSelection(.enabled)
                    if let image = events.snapshot {
                        Image(nsImage: image).resizable().aspectRatio(contentMode: .fit).frame(maxHeight: 220)
                    }
                    HStack {
                        Button("Open Recording") { events.openRecording(e) }.disabled(e.camera == nil)
                        Menu("Mute Camera") {
                            ForEach(AlertMuteChoice.allCases, id: \.self) { m in Button(m.title) { events.mute(e, m) } }
                        }
                        .fixedSize()
                        .disabled(e.camera == nil)
                    }
                    if e.alertID != nil {
                        HStack {
                            Button {
                                events.feedback(e, useful: true)
                            } label: {
                                Label("Useful", systemImage: e.vote == "up" ? "hand.thumbsup.fill" : "hand.thumbsup")
                            }
                            Menu {
                                ForEach(AlertFeedbackReason.allCases, id: \.self) { r in
                                    Button(r.title) { events.feedback(e, useful: false, reason: r) }
                                }
                            } label: {
                                Label("Not Useful", systemImage: e.vote == "down" ? "hand.thumbsdown.fill" : "hand.thumbsdown")
                            }
                            .fixedSize()
                        }
                        if let v = e.vote {
                            Text(v == "up" ? "You marked this useful." : "You marked this not useful\(e.voteReason.flatMap { AlertFeedbackReason(rawValue: $0) }.map { ": \($0.title.lowercased())" } ?? "").")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    if let note = events.note { Text(note).font(.caption).foregroundStyle(.secondary) }
                    Divider()
                    Text("Steps").font(.subheadline.bold())
                    TraceLogView(steps: e.allSteps, cameraName: events.cameraName)
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            Text("Choose a decision to see its steps.").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}
