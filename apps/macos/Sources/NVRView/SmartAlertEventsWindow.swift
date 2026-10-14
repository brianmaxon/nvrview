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
    /// Bursts opened to show their members.
    var expandedBursts: Set<String> = []
    /// The opened entry's snapshot.
    private(set) var snapshot: NSImage?
    /// The opened entry's kept evidence (frames and every method's boxes),
    /// the frames as pictures, which frame is shown, and which methods'
    /// boxes are drawn.
    private(set) var evidence: AlertEvidence?
    private(set) var evidenceImages: [Int: NSImage] = [:]
    var frame = 0
    var visibleMethods: Set<String> = []
    /// Camera AI against our detector over the kept pictures.
    private(set) var comparison = EvidenceComparison([])
    /// Every kept set by trigger (the list's picture icon and its filter).
    private(set) var evidenceSets: [UInt64: AlertEvidence] = [:]
    /// The evidence filter "with pictures": only entries whose pictures are
    /// still kept.
    var withPictures = false {
        didSet { if withPictures != oldValue { reload() } }
    }
    /// What the last action did, in words.
    var note: String?
    /// The clock (a self-test moves it past an expiry).
    @ObservationIgnored var now: () -> Date = { Date() }
    /// Where Open in Preview and drags put temporary files.
    @ObservationIgnored var tempFolder = SmartAlertEventsModel.defaultTempFolder
    /// Opens a temporary picture (Preview; the self-test replaces it).
    @ObservationIgnored var opener: (URL) -> Void = { SmartAlertEventsModel.openInPreviewApp($0) }
    /// The large viewer, while it is open.
    @ObservationIgnored var viewer: EvidenceViewerWindowController?
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
        token = app.alertsModel.subscribeEvents { [weak self] e in
            // A new decision: the list follows (once per burst).
            if case .alert = e { self?.queueReload() }
        }
    }

    func close() {
        viewer?.close()
        if let token { app.alertsModel.unsubscribeEvents(token) }
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
        guard let page = app.alertsModel.log(filter) else {
            unavailable = true
            entries = []
            more = false
            return
        }
        unavailable = false
        let kept = app.alertsModel.evidenceList()?.sets ?? []
        evidenceSets = Dictionary(kept.map { ($0.triggerID, $0) }, uniquingKeysWith: { first, _ in first })
        entries = page.entries.filter(filter.matches).filter(passesPictures)
        more = page.more
        if let s = selection, !entries.contains(where: { $0.id == s }) { selection = nil }
        loadSnapshot()
        comparison = EvidenceComparison(kept)
    }

    /// The row's picture icon: nil when no frames are kept (or they expired).
    func badge(_ e: SmartAlertLogEntry) -> EvidenceBadge.Info? {
        evidenceSets[e.triggerID].flatMap { EvidenceBadge.info($0, nowMs: Int64(now().timeIntervalSince1970 * 1000)) }
    }

    private func passesPictures(_ e: SmartAlertLogEntry) -> Bool { !withPictures || badge(e) != nil }

    /// Steps the shown frame (the large viewer's arrow keys).
    func stepFrame(by delta: Int) {
        guard let evidence else { return }
        frame = EvidenceFrameStep.step(from: frame, by: delta, in: evidence.frames.map(\.index))
    }

    /// Opens the large viewer on the shown frame.
    func openViewer() {
        guard subject != nil else { return }
        if let viewer {
            viewer.showWindow(nil)
            viewer.window?.makeKeyAndOrderFront(nil)
            return
        }
        let c = EvidenceViewerWindowController(events: self)
        c.onClose = { [weak self] in self?.viewer = nil }
        viewer = c
        c.showWindow(nil)
    }

    /// The next page (older entries).
    func loadMore() {
        guard more, let last = entries.last, let page = app.alertsModel.log(filter, beforeMs: last.decidedMs) else { return }
        entries += page.entries.filter(filter.matches).filter(passesPictures).filter { e in !entries.contains { $0.id == e.id } }
        more = page.more
    }

    var selected: SmartAlertLogEntry? { entries.first { $0.id == selection } }

    /// The list's rows: decisions, bursts folded.
    var rows: [SmartAlertLogRow] { SmartAlertLogGrouping.rows(entries) }

    func toggleBurst(_ id: String) {
        if expandedBursts.contains(id) { expandedBursts.remove(id) } else { expandedBursts.insert(id) }
    }

    func select(_ id: SmartAlertLogEntry.ID?) {
        selection = id
        note = nil
        loadSnapshot()
    }

    private func loadSnapshot() {
        snapshot = selected.flatMap { e in e.hasSnapshot ? e.alertID.flatMap { app.alertsModel.snapshot(alertID: $0) } : nil }
        loadEvidence()
    }

    /// The kept set of the opened entry, if the core still has it.
    private func loadEvidence() {
        guard let e = selected, let set = app.alertsModel.evidence(trigger: e.triggerID) else {
            evidence = nil
            evidenceImages = [:]
            return
        }
        if evidence?.triggerID != set.triggerID {
            frame = set.frames.first?.index ?? 0
            visibleMethods = Set(set.methods.map(\.name))
        }
        evidence = set
        evidenceImages = Dictionary(
            uniqueKeysWithValues: set.frames.compactMap { f in
                app.alertsModel.evidenceFrame(trigger: set.triggerID, index: f.index).map { (f.index, $0) }
            })
    }

    func toggleMethod(_ name: String) {
        if visibleMethods.contains(name) { visibleMethods.remove(name) } else { visibleMethods.insert(name) }
    }

    /// Keeps the opened entry's evidence for 7 days.
    func keepEvidence() {
        guard let e = selected else { return }
        note = app.alertsModel.pinEvidence(trigger: e.triggerID) ? "Kept for 7 days." : "The pictures are gone."
        // The list's icon follows (kept, not expiring).
        reload()
    }

    /// Opens the sensor's message log (the messages around the decision).
    func openSensorMessages(_ e: SmartAlertLogEntry) {
        guard let key = e.sensor else { return }
        app.alertsModel.showSensorMessages(key)
    }

    var sensors: [MQTTSensor] { app.alertsModel.settings.sensors }

    func cameraName(_ c: AlertCamera) -> String { app.cameraRef(c).map(app.name(of:)) ?? "Camera \(c.channel + 1)" }

    /// The list's "where": the camera, or the sensor's name.
    func place(_ e: SmartAlertLogEntry) -> String {
        if let c = e.camera { return cameraName(c) }
        if e.sourceGroup == .sensor { return "Sensor" }
        return "—"
    }

    func feedback(_ e: SmartAlertLogEntry, useful: Bool, reason: AlertFeedbackReason? = nil) {
        guard let id = e.alertID else { return }
        if app.alertsModel.feedback(alertID: id, useful: useful, reason: reason) {
            note = useful ? "Marked useful." : "Marked not useful\(reason.map { ": \($0.title.lowercased())" } ?? "")."
            reload()
        } else {
            note = "The feedback was not taken."
        }
    }

    func mute(_ e: SmartAlertLogEntry, _ choice: AlertMuteChoice) {
        guard let c = e.camera else { return }
        let ok = app.alertsModel.mute(c, seconds: choice.seconds(now: Date()))
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
        // The divider positions are remembered (UserDefaults).
        PersistentSplit(
            vertical: false, key: "eventsListFraction", defaultFraction: 0.58, minFirst: 440, minSecond: 280
        ) {
            VStack(spacing: 0) {
                filters.padding(8)
                Divider()
                list
                comparisonSummary
            }
        } second: {
            detail.frame(maxWidth: .infinity, maxHeight: .infinity)
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
                Toggle("With pictures", isOn: $events.withPictures).toggleStyle(.button)
                    .help("Only decisions whose kept pictures are still there.")
                Divider().frame(height: 16)
                Menu(events.filter.classes.isEmpty ? "Any class" : events.filter.classes.sorted().joined(separator: ", ")) {
                    ForEach(AlertClass.allCases.map(\.rawValue) + ["sensor"], id: \.self) { c in
                        Toggle(c, isOn: member(\.classes, c))
                    }
                }
                .fixedSize()
                if !events.sensors.isEmpty {
                    Picker("Sensor", selection: $events.filter.sensor) {
                        Text("Any sensor").tag(String?.none)
                        ForEach(events.sensors) { s in Text(s.name.isEmpty ? s.key : s.name).tag(String?.some(s.key)) }
                    }
                    .labelsHidden()
                    .fixedSize()
                }
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
        case "not_confirmed": "Not confirmed"
        case "quiet": "Quiet"
        case "test": "Tests"
        default: "Suppressed"
        }
    }

    /// How the camera's AI and our detector agreed over the kept pictures.
    @ViewBuilder
    private var comparisonSummary: some View {
        let c = events.comparison
        if c.compared > 0 {
            Divider()
            VStack(alignment: .leading, spacing: 2) {
                Text("Camera AI against our detector, \(c.compared) kept checks").font(.caption.bold())
                ForEach(c.rows, id: \.cls) { r in Text(c.line(r)).font(.caption).foregroundStyle(.secondary) }
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(8)
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
                    ForEach(events.rows) { r in
                        switch r {
                        case .single(let e): row(e).tag(e.id)
                        case .burst(let b):
                            burstRow(r.id, b)
                            if events.expandedBursts.contains(r.id) {
                                ForEach(b.members) { e in row(e).padding(.leading, 18).tag(e.id) }
                            }
                        }
                    }
                    if events.more {
                        Button("Older…") { events.loadMore() }
                    }
                }
            }
        }
    }

    /// A folded burst: the count and span, the camera, whether all notified,
    /// and a picture icon when any member's pictures are kept.
    private func burstRow(_ id: String, _ b: SmartAlertBurst) -> some View {
        let open = events.expandedBursts.contains(id)
        let kept = b.members.contains { events.badge($0) != nil }
        return Button {
            events.toggleBurst(id)
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: open ? "chevron.down" : "chevron.right").frame(width: 12)
                Text(Self.time(b.members[0].decidedMs)).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                    .frame(width: 98, alignment: .leading)
                Text(b.camera.map(events.cameraName) ?? "—").frame(width: 110, alignment: .leading).lineLimit(1)
                Text(b.title).bold()
                Text(b.notified == b.members.count ? "all notified" : "\(b.notified) notified")
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                if kept {
                    Image(systemName: "photo").foregroundStyle(.secondary).accessibilityLabel("Pictures kept")
                }
            }
            .font(.callout)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(b.title), \(open ? "expanded" : "collapsed")")
        .accessibilityHint("Shows or hides the individual detections")
    }

    private func row(_ e: SmartAlertLogEntry) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(Self.time(e.decidedMs)).font(.caption.monospacedDigit()).foregroundStyle(.secondary).frame(width: 110, alignment: .leading)
            Text(events.place(e)).frame(width: 110, alignment: .leading).lineLimit(1)
            Text(e.className ?? "—").frame(width: 60, alignment: .leading)
            Text(Self.decisionLabel(e)).foregroundStyle(e.decision == "alert" ? .primary : .secondary).frame(width: 120, alignment: .leading)
            if let q = e.notNotified {
                Text(q).font(.caption).foregroundStyle(.orange)
            }
            Text(e.firstSentence).lineLimit(1).foregroundStyle(.secondary)
            Spacer(minLength: 0)
            if let badge = events.badge(e) {
                Image(systemName: badge.pinned ? "photo.badge.checkmark" : "photo")
                    .foregroundStyle(.secondary)
                    .help(badge.tooltip)
                    .accessibilityLabel(badge.tooltip)
            }
        }
        .font(.callout)
    }

    static func decisionLabel(_ e: SmartAlertLogEntry) -> String {
        let base: String =
            switch e.category == "test" ? e.decision : e.category {
            case "alert": "Alert" + (e.severity.map { " (\($0))" } ?? "")
            case "not_confirmed": "Not confirmed"
            case "quiet": "Quiet"
            default: "Suppressed"
            }
        return e.isTest ? "Test: \(base)" : base
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
            if let ev = events.evidence {
                // The picture above, the text (scores and steps) below; the
                // divider is remembered.
                PersistentSplit(
                    vertical: true, key: "eventsDetailFraction", defaultFraction: 0.5, minFirst: 180, minSecond: 140
                ) {
                    EvidenceView(events: events, evidence: ev).padding(8)
                } second: {
                    detailText(e)
                }
            } else {
                detailText(e)
            }
        } else {
            Text("Choose a decision to see its steps.").foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func detailText(_ e: SmartAlertLogEntry) -> some View {
        Group {
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    Text("\(Self.decisionLabel(e)) · \(events.place(e)) · \(Self.time(e.decidedMs))").font(.headline)
                    if let q = e.notNotified {
                        Text("This detection was logged and its pictures kept, but nothing notified you: an earlier alert for this camera and class was still in its cooldown.")
                            .font(.caption).foregroundStyle(.orange)
                        Text(q.prefix(1).uppercased() + q.dropFirst()).font(.caption.bold()).foregroundStyle(.orange)
                    }
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
                    if e.sensor != nil {
                        Button("Sensor Messages…") { events.openSensorMessages(e) }
                    }
                    if let ev = events.evidence {
                        Divider()
                        Text("Scores").font(.subheadline.bold())
                        ForEach(Array(ev.methods.enumerated()), id: \.element.name) { i, m in
                            HStack(alignment: .firstTextBaseline, spacing: 6) {
                                Image(systemName: EvidenceView.symbols[i % EvidenceView.symbols.count])
                                    .foregroundStyle(EvidenceView.colors[i % EvidenceView.colors.count])
                                Text(m.title).font(.callout.bold())
                                Text(EvidenceView.summary(m, evidence: ev)).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    Divider()
                    Text("Steps").font(.subheadline.bold())
                    TraceLogView(steps: e.allSteps, cameraName: events.cameraName)
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

/// The kept frames of a check with every method's boxes over them (colors and
/// labels, not color alone), a switch per method, the scores, and Keep.
struct EvidenceView: View {
    @Bindable var events: SmartAlertEventsModel
    let evidence: AlertEvidence

    static let colors: [Color] = [.orange, .cyan, .pink, .yellow, .mint]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Kept pictures").font(.subheadline.bold())
                if evidence.methodsDisagree {
                    Label("Methods disagree", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption).foregroundStyle(.orange)
                        .accessibilityLabel("The camera's AI and our detector disagree")
                }
                Spacer()
                if evidence.pinned {
                    Text("Kept until \(Self.day(evidence.expiresMs))").font(.caption).foregroundStyle(.secondary)
                } else {
                    Text("Deleted \(Self.time(evidence.expiresMs))").font(.caption).foregroundStyle(.secondary)
                    Button("Keep") { events.keepEvidence() }
                        .help("Saves these pictures for 7 days.")
                }
            }
            if let image = events.evidenceImages[events.frame] {
                // Scales to the space, aspect kept; click to enlarge, drag
                // out for a file, right-click for the rest.
                EvidenceOverlayView(image: image, evidence: evidence, frame: events.frame, visible: events.visibleMethods)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .contentShape(Rectangle())
                    .onTapGesture { events.openViewer() }
                    .onDrag { events.dragProvider() ?? NSItemProvider() }
                    .contextMenu { EvidencePictureMenu(events: events) }
                    .help("Click to enlarge. Drag to Finder or another app for a file.")
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint("Opens the picture in a large viewer")
            } else {
                Text("This picture is gone.").foregroundStyle(.secondary)
            }
            if evidence.frames.count > 1 {
                Picker("Picture", selection: $events.frame) {
                    ForEach(evidence.frames, id: \.index) { f in Text("Picture \(f.index + 1)").tag(f.index) }
                }
                .pickerStyle(.segmented).labelsHidden()
            }
            MethodToggles(events: events, evidence: evidence)
        }
    }

    static let symbols = ["square", "circle", "triangle", "diamond", "star"]

    static func summary(_ m: EvidenceMethod, evidence: AlertEvidence) -> String {
        var parts: [String] = []
        if m.name == "camera_ai", let c = evidence.cameraClaim { parts.append("claimed \(c)") }
        let all = m.boxes.values.flatMap { $0 }.sorted { $0.score > $1.score }
        if all.isEmpty, m.name != "camera_ai" { parts.append("found nothing") }
        parts += all.prefix(4).map { "\($0.label) \(Int(($0.score * 100).rounded()))%" }
        return parts.joined(separator: ", ")
    }

    static func time(_ ms: Int64?) -> String {
        guard let ms else { return "soon" }
        let f = DateFormatter()
        f.timeStyle = .short
        f.dateStyle = .none
        return "at " + f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    static func day(_ ms: Int64?) -> String {
        guard let ms else { return "7 days" }
        let f = DateFormatter()
        f.timeStyle = .none
        f.dateStyle = .medium
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }
}

/// A picture with boxes over it, placed by `EvidenceOverlay`.
struct EvidenceOverlayView: View {
    let image: NSImage
    let evidence: AlertEvidence
    let frame: Int
    let visible: Set<String>

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                Image(nsImage: image).resizable().aspectRatio(contentMode: .fit)
                    .frame(width: geo.size.width, height: geo.size.height)
                let drawn = EvidenceOverlay.layout(evidence, frame: frame, visible: visible, picture: image.size, in: geo.size)
                ForEach(Array(drawn.enumerated()), id: \.offset) { _, d in
                    let color = EvidenceView.colors[d.colorIndex % EvidenceView.colors.count]
                    Rectangle().strokeBorder(color, style: StrokeStyle(lineWidth: 2, dash: d.colorIndex % 2 == 0 ? [] : [6, 3]))
                        .frame(width: d.rect.width, height: d.rect.height)
                        .overlay(alignment: .topLeading) {
                            Text(d.caption).font(.caption2.bold()).padding(.horizontal, 3)
                                .background(color).foregroundStyle(.black)
                                .offset(y: -14)
                        }
                        .offset(x: d.rect.minX, y: d.rect.minY)
                }
            }
        }
        .accessibilityLabel("Kept picture with detection boxes")
    }
}
