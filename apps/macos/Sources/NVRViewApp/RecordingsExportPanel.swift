import AppKit
import NVRCore
import SwiftUI
import AppBase

// The Export tab in the Recordings window's right pane (docs/UX.md
// "Recordings", "Export tab"): what to export (the incident or the in and out
// range, with editable times), the cameras, the kind, the stream, the
// options, and the Export button with its progress. The decisions are
// NVRCore's `ExportPanelRules` (tested, shared with Windows); the commands
// are the Export model's (`trim`, `exportEdit`, `downloadRange`, `saveStills`).

/// The right pane's tabs.
enum InspectorTab: String, CaseIterable, Identifiable {
    case incident = "Incident"
    case export = "Export"
    var id: String { rawValue }
}

extension RecordingsBrowser {
    // MARK: What the tab offers

    /// The cameras the range has: the playing ones, or the follow cut's or grid's.
    var exportSource: [RecordingCamera] { trimCameras }

    /// The cameras checked: the source's, unless the user unchecked some.
    var exportChecked: Set<RecordingCamera> { ExportPanelRules.checked(chosen: export.cameras, available: exportSource) }

    var exportCheckedList: [RecordingCamera] { exportSource.filter(exportChecked.contains) }

    var exportContext: ExportContext {
        let pictures: Bool = playerView == .cameras
            ? (player.playback != nil && !player.cells.isEmpty)
            : (editPlayer.plan != nil && editPlayer.session != nil)
        return ExportContext(
            hasRange: scrubSpan != nil, hasPictures: pictures, editable: editable != nil, canWriteEdit: true,
            sourceCameras: exportSource.count, checkedCameras: exportCheckedList.count)
    }

    var exportKinds: [ExportKind] { ExportPanelRules.kinds(exportContext) }

    /// The kind shown: the user's choice while it applies, else the first that does.
    var exportKind: ExportKind? { ExportPanelRules.defaultKind(exportKinds, keeping: export.kind) }

    /// The range to export: the points', else the whole range of the incident (or day) shown.
    var exportWindow: (start: Int64, end: Int64)? {
        pointsWindow ?? scrubSpan.map { (start: $0.lowerBound, end: $0.upperBound) }
    }

    var exportQuality: ExportQuality { export.quality }

    var exportStartText: String { exportWindow.map { ExportPanelRules.timeText(ms: $0.start) } ?? "" }
    var exportEndText: String { exportWindow.map { ExportPanelRules.timeText(ms: $0.end) } ?? "" }

    /// What the tab says is about to be written.
    var exportSummary: String {
        guard let kind = exportKind else { return "" }
        let window = exportWindow ?? (0, 0)
        return ExportPanelRules.summary(
            kind: kind, durationMs: window.end - window.start, checkedCameras: exportCheckedList.count,
            estimatedBytes: exportEstimate(window))
    }

    /// The size the core's spans add up to for the checked cameras and the stream chosen.
    private func exportEstimate(_ window: (start: Int64, end: Int64)) -> Int64? {
        let tier = exportQuality.tier
        var total: Int64?
        for lane in lanes where exportChecked.contains(lane.camera) {
            let spans = lane.spans.filter { $0.tier == nil || $0.tier == tier }.compactMap { s in
                s.bytes.map { (start: s.start, end: s.end, bytes: $0) }
            }
            if let bytes = ExportPanelRules.estimate(spans: spans, start: window.start / 1000, end: window.end / 1000) {
                total = (total ?? 0) + bytes
            }
        }
        return total
    }

    /// Why the Export button would not start anything, or nil.
    var exportProblem: String? {
        guard let kind = exportKind else { return "Play an incident or a camera to export from it" }
        let window = exportWindow ?? (0, 0)
        return ExportPanelRules.problem(
            kind: kind, start: window.start, end: window.end, checkedCameras: exportCheckedList.count, busy: export.busy)
    }

    // MARK: Editing the range

    /// A start time typed in the tab; false when the text is not a time inside the range.
    @discardableResult
    func setExportStart(_ text: String) -> Bool {
        guard let span = scrubSpan, let window = exportWindow,
            let ms = ExportPanelRules.parseTime(text, dayStartMs: DeviceTime.dayStart(window.start.floorDiv(1000)) * 1000),
            span.contains(ms)
        else { return false }
        export.points = points
        export.points.setIn(ms, within: span)
        return export.points.inMs == ms
    }

    @discardableResult
    func setExportEnd(_ text: String) -> Bool {
        guard let span = scrubSpan, let window = exportWindow,
            let ms = ExportPanelRules.parseTime(text, dayStartMs: DeviceTime.dayStart(window.start.floorDiv(1000)) * 1000),
            span.contains(ms)
        else { return false }
        export.points = points
        export.points.setOut(ms, within: span)
        return export.points.outMs == ms
    }

    func setExportCamera(_ camera: RecordingCamera, checked: Bool) {
        var set = exportChecked
        if checked { set.insert(camera) } else { set.remove(camera) }
        export.cameras = set
    }

    // MARK: The tab and the button

    /// Cmd+E: shows the right pane on its Export tab.
    func openExportTab() {
        inspectorTab = .export
        showInspector = true
    }

    /// The Export button: checks the choice, then runs the kind's command.
    func startExport() {
        guard let kind = exportKind else {
            message = exportProblem
            return
        }
        if let problem = exportProblem {
            message = problem
            return
        }
        let window = exportWindow ?? (0, 0)
        let all = exportCheckedList.count == exportSource.count
        switch kind {
        case .trimCopy, .trimExact:
            export.trimExact = kind == .trimExact
            trim(cameras: all ? nil : exportCheckedList)
        case .followCut: exportEdit(.follow)
        case .grid2: exportEdit(.grid2)
        case .grid4: exportEdit(.grid4)
        case .download:
            downloadRange(RangeDownloadRequest(startMs: window.start, endMs: window.end, cameras: exportChecked))
        case .stillCurrent: saveStills(eachCamera: false)
        case .stillEach: saveStills(eachCamera: true)
        }
    }
}

// MARK: The right pane

/// The right pane: the Incident and Export tabs.
struct InspectorPane: View {
    @Bindable var browser: RecordingsBrowser

    var body: some View {
        VStack(spacing: 0) {
            Picker("", selection: $browser.inspectorTab) {
                ForEach(InspectorTab.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            switch browser.inspectorTab {
            case .incident: IncidentInspector(browser: browser)
            case .export: ExportPanel(browser: browser)
            }
        }
    }
}

struct ExportPanel: View {
    @Bindable var browser: RecordingsBrowser
    @State private var startText = ""
    @State private var endText = ""

    var body: some View {
        let export = browser.export
        let kinds = browser.exportKinds
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if let kind = browser.exportKind {
                    what(kind)
                    if ExportPanelRules.usesCameras(kind) { cameras }
                    kindList(kinds, selected: kind)
                    if ExportPanelRules.usesQuality(kind, editable: browser.editable != nil) { quality(export) }
                    options(kind, export)
                    action(kind, export)
                } else {
                    ContentUnavailableView(
                        "Nothing to Export", systemImage: "square.and.arrow.up",
                        description: Text("Play an incident or a camera, then set the range with I and O."))
                }
                if let job = export.job { ExportProgress(job: job, export: export) }
                Button("Downloads…") { browser.center = .downloads }
                    .buttonStyle(.link)
                    .help("The history of downloads, with their progress")
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onChange(of: browser.exportStartText, initial: true) { _, t in startText = t }
        .onChange(of: browser.exportEndText, initial: true) { _, t in endText = t }
    }

    private func heading(_ text: String) -> some View {
        Text(text).font(.caption.weight(.semibold)).foregroundStyle(.secondary).textCase(.uppercase)
    }

    private func what(_ kind: ExportKind) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            heading("What")
            Text(browser.playingIncident?.title ?? "The range playing").font(.callout.weight(.medium)).lineLimit(2)
            if !kind.isStill {
                HStack(spacing: 6) {
                    TextField("Start", text: $startText)
                        .onSubmit { if !browser.setExportStart(startText) { startText = browser.exportStartText } }
                    Text("to")
                    TextField("End", text: $endText)
                        .onSubmit { if !browser.setExportEnd(endText) { endText = browser.exportEndText } }
                }
                .textFieldStyle(.roundedBorder)
                .font(.callout.monospacedDigit())
                .help("Edit the times, or set them with I and O or the flags on the scrubber")
                if browser.pointsWindow != nil {
                    Button("Whole Range") { browser.clearPoints() }.buttonStyle(.link).font(.caption)
                }
            }
            Text(browser.exportSummary).font(.callout).foregroundStyle(.secondary)
        }
    }

    private var cameras: some View {
        VStack(alignment: .leading, spacing: 4) {
            heading("Cameras")
            ForEach(browser.exportSource, id: \.self) { c in
                Toggle(browser.cameraName(c), isOn: Binding(
                    get: { browser.exportChecked.contains(c) }, set: { browser.setExportCamera(c, checked: $0) }))
                .toggleStyle(.checkbox)
            }
        }
    }

    private func kindList(_ kinds: [ExportKind], selected: ExportKind) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            heading("Kind")
            Picker("", selection: Binding(get: { selected }, set: { browser.export.kind = $0 })) {
                ForEach(kinds, id: \.self) { Text($0.title).tag($0) }
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            if selected == .trimExact {
                Text("Starts at the exact picture; the video is encoded again, so it takes longer.").font(.caption).foregroundStyle(.secondary)
            }
            if selected == .download {
                Text("The device's files for the range, as they are.").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private func quality(_ export: ExportModel) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            heading("Quality")
            Picker("", selection: Binding(get: { export.quality }, set: { export.qualityChoice = $0 })) {
                ForEach(ExportQuality.allCases, id: \.self) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            Text(export.quality.detail).font(.caption).foregroundStyle(.secondary)
        }
    }

    private func options(_ kind: ExportKind, _ export: ExportModel) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            heading("Options")
            Toggle("Share-safe (no names, times or metadata)", isOn: Bindable(export).shareSafe)
            if ExportPanelRules.usesSound(kind) {
                Toggle("Sound", isOn: Bindable(export).sound)
                Picker("Video", selection: Bindable(export).codec) {
                    Text("H.264").tag(ExportCodec.h264)
                    Text("HEVC").tag(ExportCodec.hevc)
                }
            }
            HStack {
                Text(export.folderPath.map { ($0 as NSString).abbreviatingWithTildeInPath } ?? "Ask where to save each time")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 4)
                Button("Choose…") {
                    if let f = export.chooseFolder("Choose where exports are saved", export.directFolder ?? export.folder.url) {
                        export.choose(folder: f)
                    }
                }
                if export.direct { Button("Ask Each Time") { export.askEachTime() } }
            }
            .controlSize(.small)
        }
    }

    private func action(_ kind: ExportKind, _ export: ExportModel) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(kind.buttonTitle) { browser.startExport() }
                .buttonStyle(.borderedProminent)
                .disabled(export.busy)
                .keyboardShortcut(.defaultAction)
            if !export.busy, let problem = browser.exportProblem {
                Text(problem).font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

/// A running or just-ended export, in place in the tab: progress and Cancel, then what was written with Show in Finder.
struct ExportProgress: View {
    let job: ExportJob
    let export: ExportModel

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if job.running {
                Text(job.title).font(.caption)
                HStack {
                    ProgressView(value: job.fraction).progressViewStyle(.linear)
                    Button("Cancel") { job.cancel() }.controlSize(.small)
                }
            } else {
                Text(job.result ?? "Done").font(.caption).lineLimit(2)
                HStack {
                    if !job.files.isEmpty {
                        Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting(job.files) }
                    }
                    Button("Dismiss") { export.dismissJob() }
                }
                .buttonStyle(.link).font(.caption)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 6))
    }
}
