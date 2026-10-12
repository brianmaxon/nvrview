import AppKit
import NVRCore
import SwiftUI

/// The Recordings window (docs/UX.md "Recordings"): Window > Recordings
/// (⌥⌘R). A sidebar with the calendar and the cameras to search; the day's
/// incidents, timeline, or downloads in the middle; the selected incident on
/// the right.
@MainActor
final class RecordingsWindowController: NSWindowController, NSWindowDelegate {
    let browser: RecordingsBrowser
    var onClose: (RecordingsWindowController) -> Void = { _ in }

    init(model: RecordingsModel) {
        browser = RecordingsBrowser(model: model)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1100, height: 680),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Recordings"
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("Recordings")
        window.contentMinSize = NSSize(width: 860, height: 480)
        window.contentViewController = NSHostingController(rootView: RecordingsView(browser: browser))
        super.init(window: window)
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    func windowWillClose(_ notification: Notification) {
        browser.close()
        onClose(self)
    }
}

// MARK: The window's content

struct RecordingsView: View {
    @Bindable var browser: RecordingsBrowser

    var body: some View {
        let model = browser.model
        HStack(spacing: 0) {
            RecordingsSidebar(browser: browser)
                .frame(width: 250)
            Divider()
            VStack(spacing: 0) {
                header
                Divider()
                switch browser.tab {
                case .incidents: IncidentList(browser: browser)
                case .timeline: TimelineLanes(browser: browser)
                case .downloads: DownloadsPanel(model: model)
                }
                if let notice = model.notice ?? browser.message {
                    Divider()
                    HStack {
                        Text(notice).font(.callout).foregroundStyle(.secondary)
                        Spacer()
                        Button("Dismiss") {
                            model.notice = nil
                            browser.message = nil
                        }
                        .buttonStyle(.link)
                    }
                    .padding(.horizontal, 12).padding(.vertical, 6)
                }
            }
            .frame(minWidth: 420, maxWidth: .infinity)
            if browser.tab != .downloads, let incident = browser.selected {
                Divider()
                IncidentDetail(browser: browser, incident: incident)
                    .frame(width: 320)
            }
        }
        .sheet(isPresented: $browser.showingGroups) { CameraGroupsEditor(browser: browser) }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Button { browser.previousDay() } label: { Image(systemName: "chevron.left") }
                .help("Previous day")
            Text(browser.dayTitle).font(.headline).lineLimit(1)
            Button { browser.nextDay() } label: { Image(systemName: "chevron.right") }
                .disabled(browser.isToday)
                .help("Next day")
            Spacer()
            Picker("", selection: $browser.tab) {
                ForEach(RecordingsBrowser.Tab.allCases) { tab in
                    if tab == .downloads, !browser.model.downloads.isEmpty {
                        Text("Downloads (\(browser.model.downloads.count))").tag(tab)
                    } else {
                        Text(tab.rawValue).tag(tab)
                    }
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
    }
}

// MARK: Sidebar: calendar, search state, cameras

struct RecordingsSidebar: View {
    let browser: RecordingsBrowser

    var body: some View {
        let model = browser.model
        VStack(alignment: .leading, spacing: 10) {
            MonthCalendar(browser: browser)
            HStack {
                IndexStateText(model: model)
                Spacer()
                Button { browser.refresh() } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.borderless)
                    .help("Search this day on the device again")
                    .disabled(browser.cameras.isEmpty)
            }
            Divider()
            Text("Cameras to Search").font(.subheadline.weight(.semibold))
            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if let app = model.app {
                        ForEach(app.devices) { device in
                            DeviceCameraToggles(model: model, app: app, device: device)
                        }
                        if app.devices.isEmpty { Text("Add a device in Settings first.").foregroundStyle(.secondary) }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button("Camera Groups…") { browser.showingGroups = true }
                .help("Cameras that cover the same area; their events join one incident")
        }
        .padding(12)
    }
}

struct IndexStateText: View {
    let model: RecordingsModel

    var body: some View {
        let s = model.searching
        Group {
            if s.busy {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small)
                    Text(s.pendingDays > 0 ? "Searching, \(s.pendingDays) left" : "Searching")
                }
            } else if s.waitingDevice {
                Text("Waiting for the device")
            } else {
                Text(model.enabledCameras.isEmpty ? "No cameras chosen" : "Up to date")
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}

struct DeviceCameraToggles: View {
    let model: RecordingsModel
    let app: AppModel
    let device: AppModel.DeviceRuntime

    var body: some View {
        let cameras = device.channels.flatMap { app.cameras(device: device.key, channel: $0) }
            .map { RecordingCamera(device: $0.device, channel: $0.channel, lens: $0.lens) }
        let on = cameras.filter { model.isEnabled($0) }
        VStack(alignment: .leading, spacing: 4) {
            Toggle(isOn: Binding(
                get: { !cameras.isEmpty && on.count == cameras.count },
                set: { model.setEnabled(device: device.key, cameras: $0 ? cameras : []) })
            ) {
                Text(device.config.name).fontWeight(.medium)
            }
            .disabled(cameras.isEmpty)
            ForEach(cameras, id: \.self) { c in
                Toggle(isOn: Binding(get: { model.isEnabled(c) }, set: { model.setEnabled(c, $0) })) {
                    Text(app.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens)))
                }
                .padding(.leading, 16)
            }
        }
        .toggleStyle(.checkbox)
    }
}

struct MonthCalendar: View {
    let browser: RecordingsBrowser
    private let columns = Array(repeating: GridItem(.flexible(), spacing: 2), count: 7)

    var body: some View {
        let today = browser.model.today
        let first = Calendar.current.firstWeekday
        VStack(spacing: 6) {
            HStack {
                Button { browser.showMonth(adding: -1) } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.borderless)
                Spacer()
                Text(DeviceTime.monthTitle(browser.month)).font(.subheadline.weight(.semibold))
                Spacer()
                Button { browser.showMonth(adding: 1) } label: { Image(systemName: "chevron.right") }
                    .buttonStyle(.borderless)
                    .disabled(DeviceTime.month(containing: browser.month).next > today)
            }
            LazyVGrid(columns: columns, spacing: 2) {
                ForEach(0..<7, id: \.self) { i in
                    Text(Calendar.current.veryShortWeekdaySymbols[(first - 1 + i) % 7])
                        .font(.caption2).foregroundStyle(.secondary)
                }
                ForEach(Array(DeviceTime.weeks(ofMonth: browser.month, firstWeekday: first).joined().enumerated()), id: \.offset) {
                    _, cell in
                    if let d = cell {
                        DayCell(
                            day: d, selected: d == browser.day, today: d == today, recorded: browser.recordedDays.contains(d),
                            future: d > today
                        ) { browser.show(day: d) }
                    } else {
                        Color.clear.frame(height: 24)
                    }
                }
            }
        }
    }
}

struct DayCell: View {
    let day: Int64
    let selected: Bool
    let today: Bool
    let recorded: Bool
    let future: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            VStack(spacing: 1) {
                Text("\(DeviceTime.dayOfMonth(day))")
                    .font(.caption.weight(recorded ? .semibold : .regular))
                    .foregroundStyle(selected ? Color.white : (future ? Color.secondary : Color.primary))
                Circle().fill(recorded ? (selected ? Color.white : Color.accentColor) : Color.clear)
                    .frame(width: 4, height: 4)
            }
            .frame(maxWidth: .infinity, minHeight: 24)
            .background(RoundedRectangle(cornerRadius: 5).fill(selected ? Color.accentColor : Color.clear))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(today && !selected ? Color.accentColor : Color.clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(future)
        .accessibilityLabel(DeviceTime.longDay(day) + (recorded ? ", has recordings" : ""))
    }
}

// MARK: Kind badges

/// The live detection badges' symbols, for recorded kinds.
struct KindBadges: View {
    let kinds: [String]

    var body: some View {
        let items = Self.unique(kinds)
        HStack(spacing: 3) {
            ForEach(items, id: \.self) { k in
                Image(systemName: k.symbolName)
                    .font(.caption2)
                    .foregroundStyle(k.isSound ? Color.orange : Color.secondary)
                    .help(k.label)
            }
        }
    }

    static func unique(_ kinds: [String]) -> [IndicatorKind] {
        var seen = Set<IndicatorKind>()
        return kinds.map(IndicatorKind.init(detectionKind:)).filter { seen.insert($0).inserted }
    }
}

extension IndicatorKind {
    /// The timeline's color for an event of this kind.
    var laneColor: Color {
        switch self {
        case .person: .blue
        case .vehicle: .purple
        case .animal: .green
        case .motion: .gray
        case .smokeAlarm, .coAlarm: .orange
        case .visitor: .teal
        default: .brown
        }
    }
}

// MARK: Incidents

struct IncidentList: View {
    @Bindable var browser: RecordingsBrowser

    var body: some View {
        VStack(spacing: 0) {
            filters
            Divider()
            if browser.cameras.isEmpty {
                ContentUnavailableView(
                    "No Cameras Chosen", systemImage: "video.slash",
                    description: Text("Choose the cameras to search in the sidebar."))
            } else if browser.incidents.isEmpty {
                ContentUnavailableView(
                    browser.dayIncidents.isEmpty ? "No Incidents" : "Nothing Matches", systemImage: "film",
                    description: Text(browser.dayIncidents.isEmpty ? "Nothing was recorded or found on this day yet." : "Change the filters to see more."))
            } else {
                // A stack, not a List: incidents come and go as the index
                // fills and edits apply, and a table whose selected row
                // disappears re-enters its delegate when SwiftUI updates it.
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(browser.incidents) { i in
                            let selected = browser.selection.contains(i.key)
                            IncidentRow(browser: browser, incident: i)
                                .padding(.horizontal, 8)
                                .background(
                                    RoundedRectangle(cornerRadius: 6)
                                        .fill(selected ? Color.accentColor.opacity(0.22) : Color.clear))
                                .contentShape(Rectangle())
                                .onTapGesture { browser.click(i.key, extend: NSEvent.modifierFlags.contains(.command)) }
                                .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
                        }
                    }
                    .padding(8)
                }
            }
        }
    }

    private var filters: some View {
        HStack(spacing: 8) {
            ForEach(KindFilter.allCases, id: \.self) { f in
                Toggle(isOn: Binding(
                    get: { browser.kindFilters.contains(f) },
                    set: { on in if on { browser.kindFilters.insert(f) } else { browser.kindFilters.remove(f) } })
                ) {
                    Label(f.label, systemImage: f.symbolName)
                }
                .toggleStyle(.button)
                .controlSize(.small)
            }
            Picker("Cameras", selection: $browser.scope) {
                ForEach(browser.scopes, id: \.0) { scope, title in Text(title).tag(scope) }
            }
            .labelsHidden()
            .fixedSize()
            .controlSize(.small)
            Toggle(isOn: $browser.lockedOnly) { Label("Locked", systemImage: "lock.fill") }
                .toggleStyle(.button)
                .controlSize(.small)
            Spacer()
            if browser.selection.count >= 2 {
                Button("Merge \(browser.selectedKeys.count)") { browser.merge() }
                    .controlSize(.small)
                    .help("Join the selected incidents into one")
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 6)
    }
}

struct IncidentRow: View {
    let browser: RecordingsBrowser
    let incident: Incident

    var body: some View {
        HStack(spacing: 10) {
            HStack(spacing: 3) {
                ForEach(incident.coverKeys.prefix(4), id: \.self) { key in
                    ThumbnailView(model: browser.model, key: key).frame(width: 80, height: 45)
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(incident.title).font(.body.weight(.medium)).lineLimit(1)
                    if incident.locked { Image(systemName: "lock.fill").font(.caption).help("Locked: kept") }
                    if incident.growing { Text("ongoing").font(.caption2).foregroundStyle(.secondary) }
                }
                HStack(spacing: 6) {
                    Text(DeviceTime.span(incident.start, incident.end)).font(.caption.monospacedDigit())
                    KindBadges(kinds: incident.kinds)
                }
                Text(incident.cameras.map { $0.name ?? browser.cameraName($0.camera) }.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            if !incident.downloads.isEmpty {
                Image(systemName: "arrow.down.circle").foregroundStyle(.secondary).help("Downloaded or downloading")
            }
        }
        .padding(.vertical, 2)
    }
}

struct ThumbnailView: View {
    let model: RecordingsModel
    let key: ThumbnailKey

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 4).fill(Color.black.opacity(0.85))
            switch model.thumbnails[key] {
            case .image(let data):
                if let image = NSImage(data: data) {
                    Image(nsImage: image).resizable().aspectRatio(contentMode: .fill)
                }
            case .pending, .decoding, nil:
                ProgressView().controlSize(.mini)
            case .empty:
                Image(systemName: "video.slash").foregroundStyle(.secondary).help("No recording in this range")
            case .failed(let reason):
                Image(systemName: "exclamationmark.triangle").foregroundStyle(.secondary).help(reason ?? "No picture")
            case .dropped:
                Image(systemName: "photo").foregroundStyle(.secondary)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 4))
        // Asked after the row appears, not while the list builds it: a
        // change of the model inside the table's delegate would re-enter it.
        .task(id: key) { model.requestThumbnail(key) }
    }
}

struct IncidentDetail: View {
    let browser: RecordingsBrowser
    let incident: Incident
    @State private var name = ""
    @State private var splitAt: Double = 0

    var body: some View {
        let model = browser.model
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    TextField("Name", text: $name, prompt: Text(incident.title))
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { browser.rename(incident.key, name) }
                    Button("Rename") { browser.rename(incident.key, name) }
                        .disabled(name == (incident.name ?? ""))
                }
                Text(DeviceTime.span(incident.start, incident.end) + " · " + DeviceTime.format(incident.start, "yyyy-MM-dd"))
                    .font(.callout.monospacedDigit())
                KindBadges(kinds: incident.kinds)
                if let group = incident.group { Text("Group: \(group)").font(.caption).foregroundStyle(.secondary) }

                HStack {
                    Toggle(isOn: Binding(get: { incident.locked }, set: { browser.setLocked(incident.key, $0) })) {
                        Label("Locked", systemImage: incident.locked ? "lock.fill" : "lock.open")
                    }
                    .toggleStyle(.button)
                    .help("A locked incident is kept: its video is downloaded and nothing about it expires")
                    Button {
                        browser.download(incident.key)
                    } label: {
                        Label("Download", systemImage: "arrow.down.circle")
                    }
                    if incident.edited {
                        Button("Revert Edits") { browser.revert(incident.key) }
                    }
                }

                Divider()
                Text("Cameras").font(.subheadline.weight(.semibold))
                ForEach(Array(zip(incident.cameras, incident.coverKeys)), id: \.1) { cam, key in
                    VStack(alignment: .leading, spacing: 4) {
                        ThumbnailView(model: model, key: key).frame(height: 160)
                        HStack {
                            Text(cam.name ?? browser.cameraName(cam.camera)).font(.callout)
                            if cam.added { Text("added").font(.caption2).foregroundStyle(.secondary) }
                            Spacer()
                            Button("Remove") { browser.remove(cam.camera, from: incident.key) }
                                .buttonStyle(.link)
                                .disabled(incident.cameras.count < 2)
                        }
                    }
                }
                let addable = browser.addable(to: incident)
                if !addable.isEmpty {
                    Menu("Add Camera") {
                        ForEach(addable, id: \.self) { c in
                            Button(browser.cameraName(c)) { browser.add(c, to: incident.key) }
                        }
                    }
                    .fixedSize()
                }

                if incident.end - incident.start >= 2 {
                    Divider()
                    Text("Split").font(.subheadline.weight(.semibold))
                    Slider(value: $splitAt, in: Double(incident.start + 1)...Double(incident.end - 1), step: 1)
                    HStack {
                        Text("at " + DeviceTime.time(Int64(splitAt))).font(.caption.monospacedDigit())
                        Spacer()
                        Button("Split Here") { browser.split(incident.key, at: Int64(splitAt)) }
                    }
                }
            }
            .padding(12)
        }
        .onAppear { reset() }
        .onChange(of: incident.key) { reset() }
    }

    private func reset() {
        name = incident.name ?? ""
        splitAt = Double((incident.start + incident.end) / 2)
    }
}

// MARK: Timeline

struct TimelineLanes: View {
    @Bindable var browser: RecordingsBrowser
    private let laneHeight: CGFloat = 34
    private let labelWidth: CGFloat = 140

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Zoom").font(.caption).foregroundStyle(.secondary)
                Button { browser.zoom = max(1, browser.zoom / 2) } label: { Image(systemName: "minus.magnifyingglass") }
                    .disabled(browser.zoom <= 1)
                Button { browser.zoom = min(48, browser.zoom * 2) } label: { Image(systemName: "plus.magnifyingglass") }
                    .disabled(browser.zoom >= 48)
                Text(browser.zoom == 1 ? "whole day" : "\(Int(browser.zoom))×").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Text("Click an event to select its incident").font(.caption).foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .padding(.horizontal, 12).padding(.vertical, 6)
            Divider()
            if browser.lanes.isEmpty {
                ContentUnavailableView(
                    "No Cameras Chosen", systemImage: "video.slash",
                    description: Text("Choose the cameras to search in the sidebar."))
            } else {
                GeometryReader { geo in
                    let width = max(geo.size.width - labelWidth, 100) * browser.zoom
                    ScrollView(.vertical) {
                        HStack(alignment: .top, spacing: 0) {
                            VStack(alignment: .leading, spacing: 0) {
                                Color.clear.frame(height: 18)
                                ForEach(browser.lanes) { lane in
                                    HStack(spacing: 4) {
                                        Text(lane.name).font(.caption).lineLimit(1)
                                        if lane.failed {
                                            Image(systemName: "exclamationmark.triangle").font(.caption)
                                                .help("The device could not search this day; use Refresh")
                                        }
                                    }
                                    .frame(width: labelWidth - 8, height: laneHeight, alignment: .leading)
                                }
                            }
                            .padding(.leading, 8)
                            ScrollView(.horizontal) {
                                VStack(alignment: .leading, spacing: 0) {
                                    HourRuler(width: width).frame(width: width, height: 18)
                                    ForEach(browser.lanes) { lane in
                                        LaneView(browser: browser, lane: lane, width: width)
                                            .frame(width: width, height: laneHeight)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

struct HourRuler: View {
    let width: CGFloat

    var body: some View {
        Canvas { ctx, size in
            let step = width / 24 < 28 ? 3 : 1
            for h in stride(from: 0, through: 24, by: step) {
                let x = CGFloat(h) / 24 * size.width
                ctx.fill(Path(CGRect(x: x, y: size.height - 5, width: 1, height: 5)), with: .color(.secondary))
                if h < 24 {
                    ctx.draw(
                        Text(String(format: "%02d:00", h)).font(.caption2).foregroundStyle(.secondary),
                        at: CGPoint(x: x + 2, y: 0), anchor: .topLeading)
                }
            }
        }
    }
}

struct LaneView: View {
    let browser: RecordingsBrowser
    let lane: RecordingsBrowser.Lane
    let width: CGFloat

    var body: some View {
        let day = browser.day
        let scale = width / CGFloat(DeviceTime.day)
        let selected = Set(browser.selected.map { i in i.events.filter { $0.camera == lane.camera } } ?? [])
        Canvas { ctx, size in
            ctx.fill(Path(CGRect(x: 0, y: 6, width: size.width, height: size.height - 12)), with: .color(.secondary.opacity(0.08)))
            for s in lane.spans {
                let x = CGFloat(s.start - day) * scale, w = max(1, CGFloat(s.end - s.start) * scale)
                ctx.fill(Path(CGRect(x: x, y: 10, width: w, height: size.height - 20)), with: .color(.secondary.opacity(0.35)))
            }
            for e in lane.events {
                let kind = IndicatorKind(detectionKind: e.kind)
                let x = CGFloat(e.start - day) * scale, w = max(3, CGFloat(e.end - e.start) * scale)
                let rect = CGRect(x: x, y: 4, width: w, height: size.height - 8)
                ctx.fill(Path(roundedRect: rect, cornerRadius: 2), with: .color(kind.laneColor))
                if selected.contains(e) {
                    ctx.stroke(Path(roundedRect: rect.insetBy(dx: -1.5, dy: -1.5), cornerRadius: 3), with: .color(.primary), lineWidth: 2)
                }
                if w >= 14, let symbol = ctx.resolveSymbol(id: kind) {
                    ctx.draw(symbol, at: CGPoint(x: x + w / 2, y: size.height / 2))
                }
            }
        } symbols: {
            ForEach(Array(Set(lane.events.map { IndicatorKind(detectionKind: $0.kind) })), id: \.self) { k in
                Image(systemName: k.symbolName).font(.system(size: 9)).foregroundStyle(.white).tag(k)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { point in
            let t = day + Int64(point.x / scale)
            // A generous target: the nearest event within 6 points.
            let slack = Int64(6 / scale)
            if let e = lane.events.min(by: { distance($0, t) < distance($1, t) }), distance(e, t) <= slack {
                browser.select(event: e)
            }
        }
        .help(lane.events.isEmpty ? lane.name : "\(lane.name): \(lane.events.count) events")
    }

    private func distance(_ e: RecordedEvent, _ t: Int64) -> Int64 {
        t < e.start ? e.start - t : (t > e.end ? t - e.end : 0)
    }
}

// MARK: Downloads

struct DownloadsPanel: View {
    let model: RecordingsModel

    var body: some View {
        VStack(spacing: 0) {
            if model.downloads.isEmpty {
                ContentUnavailableView(
                    "No Downloads", systemImage: "arrow.down.circle",
                    description: Text("Download an incident, or lock it to keep it."))
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(model.downloads, id: \.download) { d in
                            DownloadRow(model: model, download: d).padding(.horizontal, 12)
                            Divider()
                        }
                    }
                }
            }
            if let folder = model.downloadsFolder {
                Divider()
                HStack {
                    Text(folder.path).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                    Spacer()
                    Button("Open Folder") { model.revealer([folder]) }.buttonStyle(.link)
                }
                .padding(.horizontal, 12).padding(.vertical, 6)
            }
        }
    }
}

struct DownloadRow: View {
    let model: RecordingsModel
    let download: DownloadProgress

    var body: some View {
        let d = download
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(d.folderName).font(.body.weight(.medium)).lineLimit(1).truncationMode(.middle)
                Spacer()
                Text(d.stateText).font(.caption).foregroundStyle(d.state == "failed" ? Color.red : Color.secondary)
            }
            ProgressView(value: d.fraction).progressViewStyle(.linear)
            HStack(spacing: 12) {
                Text(d.cameras.map { ($0.name ?? "Channel \($0.camera.channel + 1)") + " " + Self.cameraText($0) }.joined(separator: " · "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                if d.state != "done" {
                    Button(d.paused ? "Resume" : "Pause") { model.pause(d.download, !d.paused) }
                }
                if d.state == "failed" { Button("Retry") { model.retry(d.download) } }
                Button("Show in Finder") { model.reveal(d) }
                if !d.isWorking { Button("Forget") { model.forget(d.download) }.help("Remove from the list; files stay") }
                Button("Move to Trash") { model.trash(d.download) }
            }
            .buttonStyle(.link)
            .font(.caption)
        }
        .padding(.vertical, 3)
    }

    static func cameraText(_ c: DownloadCamera) -> String {
        if c.state == "done" { return "done" }
        if c.chunksTotal > 0 { return "\(c.chunksDone)/\(c.chunksTotal)" }
        return c.state.replacingOccurrences(of: "_", with: " ")
    }
}

// MARK: Camera groups

struct CameraGroupsEditor: View {
    let browser: RecordingsBrowser
    @Environment(\.dismiss) private var dismiss
    @State private var groups: [IncidentGroups.Group] = []

    var body: some View {
        let cameras = browser.cameras
        VStack(alignment: .leading, spacing: 10) {
            Text("Camera Groups").font(.title3.weight(.semibold))
            Text("Cameras that cover the same area. Events on cameras of one group that overlap or follow each other join one incident. Changing a group changes its incidents (edits of locked incidents follow).")
                .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(groups.indices, id: \.self) { i in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                TextField("Name", text: $groups[i].name).textFieldStyle(.roundedBorder)
                                Button { groups.remove(at: i) } label: { Image(systemName: "trash") }
                                    .buttonStyle(.borderless)
                                    .help("Delete the group")
                            }
                            ForEach(cameras, id: \.self) { c in
                                Toggle(browser.cameraName(c), isOn: Binding(
                                    get: { i < groups.count && groups[i].cameras.contains(c) },
                                    set: { on in
                                        guard i < groups.count else { return }
                                        if on { groups[i].cameras.append(c) } else { groups[i].cameras.removeAll { $0 == c } }
                                    }))
                                .toggleStyle(.checkbox)
                                .padding(.leading, 12)
                            }
                        }
                    }
                }
            }
            .frame(minHeight: 200)
            HStack {
                Button("Add Group") { groups.append(.init(name: "Group \(groups.count + 1)", cameras: [])) }
                    .disabled(groups.count >= 64)
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Save") {
                    var new = browser.model.groups
                    new.groups = groups
                    if browser.model.putGroups(new) { dismiss() }
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 440, height: 480)
        .onAppear { groups = browser.model.groups.groups }
    }
}
