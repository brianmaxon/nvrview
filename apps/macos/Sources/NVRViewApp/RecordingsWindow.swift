import AppKit
import NVRCore
import SwiftUI
import AppBase

/// The Recordings window (docs/UX.md "Recordings"): Window > Recordings
/// (⌥⌘R). The player in the middle with its transport and the day's
/// timeline lanes under it; on the left (collapsible) the calendar, the
/// incident list with its filters, and the cameras to search; on the right
/// (collapsible) the selected incident's details and actions.
@MainActor
final class RecordingsWindowController: NSWindowController, NSWindowDelegate {
    let browser: RecordingsBrowser
    var onClose: (RecordingsWindowController) -> Void = { _ in }
    /// The player's picture view, once SwiftUI made it (the self-test).
    private(set) weak var stage: PlayerStageView?

    init(model: RecordingsModel) {
        browser = RecordingsBrowser(model: model)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Recordings"
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("Recordings")
        window.contentMinSize = RecordingsPanes.windowMin
        super.init(window: window)
        let hosting = NSHostingController(
            rootView: RecordingsView(browser: browser, onStage: { [weak self] in self?.stage = $0 }, grow: { [weak self] in
                self?.grow(toWidth: $0)
            }))
        // The window decides its size; the content fits into it (and hides
        // side panes that do not fit) instead of pushing the window around.
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 1280, height: 800))
        window.setFrameUsingName("Recordings")
        window.delegate = self
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    /// A side pane was turned on in a window too narrow for it: the window
    /// grows (within its screen) so the player keeps its room.
    private func grow(toWidth width: CGFloat) {
        guard let window, let screen = window.screen ?? NSScreen.main else { return }
        let content = window.contentRect(forFrameRect: window.frame)
        guard content.width < width else { return }
        var frame = window.frame
        let extra = min(width - content.width, screen.visibleFrame.width - frame.width)
        guard extra > 0 else { return }
        frame.size.width += extra
        frame.origin.x = max(screen.visibleFrame.minX, min(frame.origin.x - extra / 2, screen.visibleFrame.maxX - frame.width))
        window.setFrame(frame, display: true, animate: false)
    }

    func windowWillClose(_ notification: Notification) {
        browser.close()
        onClose(self)
    }
}

// MARK: The window's content

struct RecordingsView: View {
    @Bindable var browser: RecordingsBrowser
    var onStage: (PlayerStageView) -> Void = { _ in }
    var grow: (CGFloat) -> Void = { _ in }

    var body: some View {
        GeometryReader { geo in
            let panes = RecordingsPanes.shown(
                width: geo.size.width, sidebar: browser.showSidebar, inspector: browser.showInspector)
            VStack(spacing: 0) {
                RecordingsHeader(browser: browser, panes: panes, toggle: toggle(_:width:))
                Divider()
                HStack(spacing: 0) {
                    if panes.sidebar {
                        RecordingsSidebar(browser: browser)
                            .frame(width: RecordingsPanes.sidebarWidth)
                            .frame(maxHeight: .infinity)
                        Divider()
                    }
                    Group {
                        switch browser.center {
                        case .player: PlayerColumn(browser: browser, onStage: onStage)
                        case .downloads: DownloadsPanel(model: browser.model)
                        }
                    }
                    .frame(minWidth: 0, maxWidth: .infinity, maxHeight: .infinity)
                    if panes.inspector {
                        Divider()
                        InspectorPane(browser: browser)
                            .frame(width: RecordingsPanes.inspectorWidth)
                            .frame(maxHeight: .infinity)
                    }
                }
                .frame(maxHeight: .infinity)
                if let notice = browser.model.notice ?? browser.message ?? browser.player.message {
                    Divider()
                    HStack {
                        Text(notice).font(.callout).foregroundStyle(.secondary).lineLimit(2)
                        Spacer()
                        Button("Dismiss") {
                            browser.model.notice = nil
                            browser.message = nil
                        }
                        .buttonStyle(.link)
                    }
                    .padding(.horizontal, 12).padding(.vertical, 6)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
            .clipped()
            .onChange(of: PaneKey(width: geo.size.width, sidebar: panes.sidebar, inspector: panes.inspector), initial: true) {
                _, k in browser.shownPanes = (k.width, k.sidebar, k.inspector)
            }
        }
        .sheet(isPresented: $browser.showingGroups) { CameraGroupsEditor(browser: browser) }
        .background {
            // Cmd+E opens the Export tab (showing the right pane, in a window wide enough for it).
            Button("Export Tab") { openExportTab() }
                .keyboardShortcut("e", modifiers: .command)
                .opacity(0)
                .accessibilityHidden(true)
        }
    }

    private struct PaneKey: Equatable {
        var width: CGFloat
        var sidebar: Bool
        var inspector: Bool
    }

    private func openExportTab() {
        let wasShown = browser.showInspector
        browser.openExportTab()
        if !wasShown {
            let need = RecordingsPanes.centerMin + (browser.showSidebar ? RecordingsPanes.sidebarWidth : 0) + RecordingsPanes.inspectorWidth
            if need > (browser.shownPanes?.width ?? need) { grow(need) }
        }
    }

    /// The header's pane buttons: turning a pane on in a window too narrow
    /// for it grows the window.
    private func toggle(_ inspector: Bool, width: CGFloat) {
        if inspector { browser.showInspector.toggle() } else { browser.showSidebar.toggle() }
        let need = RecordingsPanes.centerMin + (browser.showSidebar ? RecordingsPanes.sidebarWidth : 0)
            + (browser.showInspector ? RecordingsPanes.inspectorWidth : 0)
        if need > width { grow(need) }
    }
}

struct RecordingsHeader: View {
    @Bindable var browser: RecordingsBrowser
    let panes: (sidebar: Bool, inspector: Bool)
    let toggle: (Bool, CGFloat) -> Void

    var body: some View {
        GeometryReader { geo in
            HStack(spacing: 8) {
                Button { toggle(false, geo.size.width) } label: { Image(systemName: "sidebar.left") }
                    .help(panes.sidebar ? "Hide the calendar and incidents" : "Show the calendar and incidents")
                Divider().frame(height: 18)
                Button { browser.previousDay() } label: { Image(systemName: "chevron.left") }
                    .help("Previous day")
                Text(browser.dayTitle).font(.headline).lineLimit(1).truncationMode(.tail)
                    .layoutPriority(-1)
                Button { browser.nextDay() } label: { Image(systemName: "chevron.right") }
                    .disabled(browser.isToday)
                    .help("Next day")
                Button { browser.analyzeSound() } label: { Image(systemName: "waveform.badge.magnifyingglass") }
                    .disabled(!browser.canAnalyze)
                    .help("Analyze Sounds: find sounds in this day's downloaded video for all searched cameras")
                    .accessibilityLabel("Analyze Sounds")
                Spacer(minLength: 8)
                Picker("", selection: $browser.center) {
                    ForEach(RecordingsBrowser.Center.allCases) { c in
                        if c == .downloads, !browser.model.downloads.isEmpty {
                            Text("Downloads (\(browser.model.downloads.count))").tag(c)
                        } else {
                            Text(c.rawValue).tag(c)
                        }
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
                Divider().frame(height: 18)
                Button { toggle(true, geo.size.width) } label: { Image(systemName: "sidebar.right") }
                    .help(panes.inspector ? "Hide the incident's details" : "Show the incident's details")
            }
            .buttonStyle(.borderless)
            .padding(.horizontal, 10)
            .frame(width: geo.size.width, height: geo.size.height)
        }
        .frame(height: 38)
    }
}

// MARK: Sidebar: calendar, incidents, cameras

struct RecordingsSidebar: View {
    @Bindable var browser: RecordingsBrowser
    @State private var camerasShown = false

    var body: some View {
        let model = browser.model
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                MonthCalendar(browser: browser)
                HStack {
                    IndexStateText(model: model)
                    Spacer()
                    Button { browser.refresh() } label: { Image(systemName: "arrow.clockwise") }
                        .buttonStyle(.borderless)
                        .help("Search this day on the device again")
                        .disabled(browser.cameras.isEmpty)
                }
            }
            .padding(10)
            Divider()
            IncidentFilters(browser: browser)
            Divider()
            IncidentList(browser: browser)
                .frame(maxHeight: .infinity)
            Divider()
            DisclosureGroup(isExpanded: $camerasShown) {
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
                .frame(maxHeight: 180)
                Button("Camera Groups…") { browser.showingGroups = true }
                    .help("Cameras that cover the same area; their events join one incident")
                    .padding(.top, 4)
            } label: {
                Text("Cameras to Search (\(model.enabledCameras.count))").font(.subheadline.weight(.semibold))
            }
            .padding(10)
        }
        .onAppear { if browser.cameras.isEmpty { camerasShown = true } }
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
    let device: DeviceRuntime

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
                HStack(spacing: 4) {
                    Toggle(isOn: Binding(get: { model.isEnabled(c) }, set: { model.setEnabled(c, $0) })) {
                        Text(app.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens)))
                    }
                    Spacer(minLength: 0)
                    ListenToggle(model: model, camera: c)
                }
                .padding(.leading, 16)
            }
        }
        .toggleStyle(.checkbox)
    }
}

/// A camera's "Listen for sounds" switch (`nv_sound_watch`): its sub-stream
/// stays open with no tile so its sound is analyzed (loudness, alarms) while
/// it is not on screen; at most four cameras of a device.
struct ListenToggle: View {
    let model: RecordingsModel
    let camera: RecordingCamera

    var body: some View {
        let on = model.isListening(camera)
        let state = model.listenStates[camera]
        Toggle(isOn: Binding(get: { on }, set: { model.setListening(camera, $0) })) {
            Image(systemName: state?.state == "refused" ? "ear.trianglebadge.exclamationmark" : "ear")
        }
        .toggleStyle(.button)
        .buttonStyle(.borderless)
        .controlSize(.mini)
        .help(
            on
                ? "Listen for sounds: \(state.map { SoundListening.describe(state: $0.state, reason: $0.reason) } ?? "starting"). Click to stop."
                : "Listen for sounds while not on screen (keeps this camera's sub-stream open; at most \(SoundListening.maxPerDevice) per device)")
        .accessibilityLabel("Listen for sounds")
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
                    .help("Previous month")
                Spacer()
                Text(DeviceTime.monthTitle(browser.month)).font(.subheadline.weight(.semibold))
                Spacer()
                Button { browser.showMonth(adding: 1) } label: { Image(systemName: "chevron.right") }
                    .buttonStyle(.borderless)
                    .disabled(DeviceTime.month(containing: browser.month).next > today)
                    .help("Next month")
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
                        Color.clear.frame(height: 22)
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
            .frame(maxWidth: .infinity, minHeight: 22)
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
/// An incident's sounds other than the alarms (those have their own
/// badges): one waveform, the sounds' names on hover.
struct SoundBadge: View {
    let events: [RecordedEvent]

    var body: some View {
        let names = SoundKinds.names(events.filter { !IndicatorKind(detectionKind: $0.kind).isSound })
        if !names.isEmpty {
            Image(systemName: "waveform")
                .font(.caption2)
                .foregroundStyle(Color.orange)
                .help("Sounds: " + names.joined(separator: ", "))
                .accessibilityLabel("Sounds: " + names.joined(separator: ", "))
        }
    }
}

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

struct IncidentFilters: View {
    @Bindable var browser: RecordingsBrowser

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 4) {
                ForEach(KindFilter.allCases, id: \.self) { f in
                    Toggle(isOn: Binding(
                        get: { browser.kindFilters.contains(f) },
                        set: { on in if on { browser.kindFilters.insert(f) } else { browser.kindFilters.remove(f) } })
                    ) {
                        Image(systemName: f.symbolName)
                    }
                    .toggleStyle(.button)
                    .help("Only \(f.label)")
                    .accessibilityLabel(f.label)
                }
                Toggle(isOn: $browser.lockedOnly) { Image(systemName: "lock.fill") }
                    .toggleStyle(.button)
                    .help("Only locked incidents")
                    .accessibilityLabel("Locked")
                Spacer(minLength: 0)
            }
            .controlSize(.small)
            HStack(spacing: 6) {
                Picker("Cameras", selection: $browser.scope) {
                    ForEach(browser.scopes, id: \.0) { scope, title in Text(title).tag(scope) }
                }
                .labelsHidden()
                .controlSize(.small)
                if browser.selection.count >= 2 {
                    Button("Merge \(browser.selectedKeys.count)") { browser.merge() }
                        .controlSize(.small)
                        .help("Join the selected incidents into one")
                }
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 8)
    }
}

struct IncidentList: View {
    @Bindable var browser: RecordingsBrowser

    var body: some View {
        if browser.cameras.isEmpty {
            ContentUnavailableView(
                "No Cameras Chosen", systemImage: "video.slash",
                description: Text("Choose the cameras to search below."))
        } else if browser.incidents.isEmpty {
            ContentUnavailableView(
                browser.dayIncidents.isEmpty ? "No Incidents" : "Nothing Matches", systemImage: "film",
                description: Text(browser.dayIncidents.isEmpty ? "Nothing was recorded or found on this day yet." : "Change the filters to see more."))
        } else {
            // A stack, not a List: incidents come and go as the index fills
            // and edits apply.
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(browser.incidents) { i in
                        let selected = browser.selection.contains(i.key)
                        IncidentRow(browser: browser, incident: i)
                            .padding(.horizontal, 6).padding(.vertical, 5)
                            .background(
                                RoundedRectangle(cornerRadius: 6)
                                    .fill(selected ? Color.accentColor.opacity(0.22) : Color.clear))
                            .contentShape(Rectangle())
                            .onTapGesture { browser.click(i.key, extend: NSEvent.modifierFlags.contains(.command)) }
                            .contextMenu { IncidentMenu(browser: browser, incident: i) }
                            .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
                        Divider().padding(.leading, 6 + IncidentRowStrip.width + 8)
                    }
                }
                .padding(.horizontal, 4).padding(.vertical, 4)
            }
        }
    }
}

/// An incident's context menu (the row's; the inspector has the same
/// actions as buttons).
struct IncidentMenu: View {
    let browser: RecordingsBrowser
    let incident: Incident

    var body: some View {
        Button("Play") { browser.click(incident.key, extend: false) }
        if let at = browser.splitSecond(incident) {
            Button("Split Here (\(DeviceTime.time(at)))") { browser.splitHere(incident) }
        }
        Menu("Remove from Incident") {
            ForEach(incident.cameras, id: \.camera) { c in
                Button(browser.cameraName(c.camera)) { browser.remove(c.camera, from: incident.key) }
            }
        }
        .disabled(incident.cameras.count < 2)
        Divider()
        Button(incident.locked ? "Unlock" : "Lock") { browser.setLocked(incident.key, !incident.locked) }
        Button("Download") { browser.download(incident.key) }
        if incident.edited { Button("Revert Edits") { browser.revert(incident.key) } }
    }
}

struct IncidentRow: View {
    let browser: RecordingsBrowser
    let incident: Incident

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            ThumbnailStrip(model: browser.model, keys: incident.coverKeys)
                .frame(width: IncidentRowStrip.width, height: IncidentRowStrip.thumbnail.height, alignment: .leading)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(incident.title).font(.callout.weight(.medium)).lineLimit(1)
                    if incident.locked { Image(systemName: "lock.fill").font(.caption2).help("Locked: kept") }
                    if !incident.downloads.isEmpty {
                        Image(systemName: "arrow.down.circle").font(.caption2).foregroundStyle(.secondary)
                            .help("Downloaded or downloading")
                    }
                }
                HStack(spacing: 4) {
                    Text(DeviceTime.span(incident.start, incident.end)).font(.caption.monospacedDigit())
                    if incident.growing { Text("ongoing").font(.caption2).foregroundStyle(.secondary) }
                    KindBadges(kinds: incident.kinds)
                    SoundBadge(events: incident.events)
                }
                Text(incident.cameras.map { browser.cameraName($0.camera) }.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A row's covers: a fixed width whatever the number of cameras, the
/// thumbnails apart, "+N" for cameras beyond those shown.
struct ThumbnailStrip: View {
    let model: RecordingsModel
    let keys: [ThumbnailKey]

    var body: some View {
        let slots = IncidentRowStrip.slots(keys.count)
        HStack(spacing: IncidentRowStrip.spacing) {
            ForEach(Array(keys.prefix(slots.shown).enumerated()), id: \.offset) { n, key in
                ThumbnailView(model: model, key: key)
                    .frame(width: IncidentRowStrip.thumbnail.width, height: IncidentRowStrip.thumbnail.height)
                    .overlay(alignment: .bottomTrailing) {
                        if n == slots.shown - 1, slots.more > 0 {
                            Text("+\(slots.more)").font(.caption2.weight(.semibold)).foregroundStyle(.white)
                                .padding(.horizontal, 3).background(Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 3))
                                .padding(2)
                        }
                    }
            }
        }
    }
}

struct ThumbnailView: View {
    let model: RecordingsModel
    let key: ThumbnailKey

    var body: some View {
        ZStack {
            Color.black.opacity(0.85)
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
        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(Color.primary.opacity(0.15)))
        // Asked after the row appears, not while the list builds it.
        .task(id: key) { model.requestThumbnail(key) }
    }
}

// MARK: The inspector

struct IncidentInspector: View {
    let browser: RecordingsBrowser

    var body: some View {
        if let incident = browser.selected {
            IncidentDetail(browser: browser, incident: incident)
        } else {
            ContentUnavailableView(
                browser.selection.count > 1 ? "\(browser.selectedKeys.count) Incidents" : "No Incident",
                systemImage: "film",
                description: Text(browser.selection.count > 1 ? "Merge them with the button above the list." : "Select an incident to see and change it."))
        }
    }
}

struct IncidentDetail: View {
    let browser: RecordingsBrowser
    let incident: Incident
    @State private var name = ""

    var body: some View {
        let model = browser.model
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 6) {
                    TextField("Name", text: $name, prompt: Text(incident.title))
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { browser.rename(incident.key, name) }
                    HStack {
                        Button("Rename") { browser.rename(incident.key, name) }
                            .disabled(name == (incident.name ?? ""))
                        Spacer()
                    }
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text(DeviceTime.span(incident.start, incident.end) + " · " + DeviceTime.format(incident.start, "yyyy-MM-dd"))
                        .font(.callout.monospacedDigit())
                    KindBadges(kinds: incident.kinds)
                    if let group = incident.group { Text("Group: \(group)").font(.caption).foregroundStyle(.secondary) }
                }
                HStack(spacing: 8) {
                    Toggle(isOn: Binding(get: { incident.locked }, set: { browser.setLocked(incident.key, $0) })) {
                        Label("Locked", systemImage: incident.locked ? "lock.fill" : "lock.open")
                    }
                    .toggleStyle(.button)
                    .help("A locked incident is kept: its video is downloaded and nothing about it expires")
                    Button { browser.download(incident.key) } label: { Label("Download", systemImage: "arrow.down.circle") }
                    Spacer(minLength: 0)
                }
                if incident.edited {
                    Button("Revert Edits") { browser.revert(incident.key) }
                        .help("Undo every change made to this incident (names, merges, splits, cameras)")
                }

                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    let at = browser.splitSecond(incident)
                    Button { browser.splitHere(incident) } label: {
                        Label(at.map { "Split Here (\(DeviceTime.time($0)))" } ?? "Split Here", systemImage: "scissors")
                    }
                    .disabled(at == nil)
                    .help(SplitHelp.text)
                    Text(at == nil ? "Move the playhead inside this incident to split it there." : SplitHelp.short)
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }

                Divider()
                Text("Cameras").font(.subheadline.weight(.semibold))
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(zip(incident.cameras, incident.coverKeys)), id: \.1) { cam, key in
                        HStack(alignment: .center, spacing: 8) {
                            ThumbnailView(model: model, key: key).frame(width: 96, height: 54)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(browser.cameraName(cam.camera)).font(.callout).lineLimit(2)
                                HStack(spacing: 4) {
                                    if cam.added { Text("added").font(.caption2).foregroundStyle(.secondary) }
                                    if browser.player.cameras.contains(cam.camera) {
                                        Image(systemName: "play.rectangle").font(.caption2).foregroundStyle(.secondary)
                                            .help("In the player")
                                    }
                                }
                                Button(role: .destructive) { browser.remove(cam.camera, from: incident.key) } label: {
                                    Label("Remove from Incident", systemImage: "minus.circle")
                                }
                                .buttonStyle(.borderless)
                                .font(.caption)
                                .disabled(incident.cameras.count < 2)
                                .help(incident.cameras.count < 2
                                    ? "An incident keeps at least one camera"
                                    : "This camera's events are not part of this incident (for example a coincidence); Revert Edits brings it back")
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, 6)
                        Divider()
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
                    .help("Add a camera's recording of this time to the incident")
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear { name = incident.name ?? "" }
        .onChange(of: incident.key) { name = incident.name ?? "" }
    }
}

/// What Split Here does, in words.
enum SplitHelp {
    static let short = "Divides the incident in two at the playhead: what happened before stays here, what follows becomes a new incident."
    static let text =
        "Split Here divides this incident into two at the playhead. Events before it stay in this incident; events from it on become a new incident with the same cameras. Use it when two separate happenings were grouped together. Revert Edits undoes it."
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
                Text(d.rangeText.map { "Range \($0)" } ?? d.folderName).font(.body.weight(.medium)).lineLimit(1).truncationMode(.middle)
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
