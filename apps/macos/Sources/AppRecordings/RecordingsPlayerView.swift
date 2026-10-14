import AppKit
import NVRCore
import SwiftUI

// The middle of the Recordings window (docs/UX.md "Recordings"): the player
// with up to four cameras, its transport and scrubber, and the day's
// timeline lanes with the playhead.

package struct PlayerColumn: View {
    @Bindable package var browser: RecordingsBrowser
    package var onStage: (PlayerStageView) -> Void = { _ in }

    package var body: some View {
        GeometryReader { geo in
            let lanes = RecordingsPanes.lanesHeight(
                height: geo.size.height, lanes: browser.lanes.count, loudness: browser.loudnessHeight)
            VStack(spacing: 0) {
                Group {
                    if browser.playerView == .cameras {
                        PlayerArea(browser: browser, player: browser.player, onStage: onStage)
                    } else {
                        EditArea(browser: browser, player: browser.editPlayer, onStage: { [browser] in browser.editStage = $0 })
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                Divider()
                if let job = browser.export.job, !(browser.showInspector && browser.inspectorTab == .export) {
                    ExportStrip(job: job, export: browser.export)
                }
                TransportBar(browser: browser, player: browser.player)
                    .frame(height: RecordingsPanes.transportHeight)
                Divider()
                TimelineLanes(browser: browser)
                    .frame(height: lanes)
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
    }
}

// MARK: The player

package struct PlayerArea: View {
    package let browser: RecordingsBrowser
    package let player: RecordingsPlayer
    package var onStage: (PlayerStageView) -> Void
    @FocusState private var focused: Bool

    package var body: some View {
        GeometryReader { geo in
            let bounds = CGRect(origin: .zero, size: geo.size).insetBy(dx: 4, dy: 4)
            let cells = PlayerGrid.cells(count: player.cells.count, mode: player.mode, in: bounds, gap: 4)
            let items: [PlayerStageView.Item] = zip(player.cells, cells).compactMap { c, r in
                r.map { PlayerStageView.Item(tile: c.tile, rect: $0, camera: c.camera) }
            }
            ZStack(alignment: .topLeading) {
                PlayerStage(items: items, player: player, onView: onStage)
                if player.cells.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "play.rectangle").font(.system(size: 34))
                        Text(browser.cameras.isEmpty ? "Choose the cameras to search" : "Select an incident to play it")
                            .font(.headline)
                        Text("Or click a camera's lane below to play it from there.")
                            .font(.callout)
                    }
                    .foregroundStyle(.white.opacity(0.75))
                    .frame(width: geo.size.width, height: geo.size.height)
                }
                ForEach(Array(player.cells.enumerated()), id: \.element.id) { i, cell in
                    if i < cells.count, let rect = cells[i] {
                        PlayerCellOverlay(browser: browser, player: player, index: i, cell: cell, rect: rect)
                            .frame(width: rect.width, height: rect.height)
                            .offset(x: rect.minX, y: rect.minY)
                    }
                }
            }
            .overlay(alignment: .bottom) {
                if let toast = player.toast {
                    PlayerToastView(player: player, toast: toast).padding(.bottom, 14)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
            .background(Color.black)
            .clipped()
            .simultaneousGesture(TapGesture().onEnded { focused = true })
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress(.space) {
            player.togglePlay()
            return .handled
        }
        .onKeyPress("i") {
            browser.markIn()
            return .handled
        }
        .onKeyPress("o") {
            browser.markOut()
            return .handled
        }
        // The tile keys act on the cell under the pointer, else the large
        // one (docs/UX.md "Keyboard"): 0 resets the view, P saves the
        // picture, Shift+P asks where.
        .onKeyPress("0") {
            guard player.selectedIndex != nil else { return .ignored }
            player.resetSelectedView()
            return .handled
        }
        .onKeyPress(characters: CharacterSet(charactersIn: "pP"), phases: .down) { press in
            guard let i = player.selectedIndex else { return .ignored }
            player.perform(press.modifiers.contains(.shift) || press.characters == "P" ? .savePictureAs : .savePicture, at: i)
            return .handled
        }
    }

    package init(browser: RecordingsBrowser, player: RecordingsPlayer, onStage: @escaping (PlayerStageView) -> Void) {
        self.browser = browser
        self.player = player
        self.onStage = onStage
    }
}

/// The message over the player: a saved picture, with Show in Finder. It
/// goes away by itself.
package struct PlayerToastView: View {
    package let player: RecordingsPlayer
    package let toast: PlayerToast

    package var body: some View {
        HStack(spacing: 10) {
            Text(toast.text)
            if let title = toast.actionTitle, let url = toast.url {
                Button(title) { NSWorkspace.shared.activateFileViewerSelecting([url]) }
                    .buttonStyle(.link)
            }
        }
        .font(.callout)
        .foregroundStyle(.white)
        .padding(.horizontal, 12).padding(.vertical, 6)
        .background(Color.black.opacity(0.75), in: Capsule())
        .accessibilityElement(children: .combine)
        .task(id: toast.id) {
            try? await Task.sleep(for: .seconds(toast.url == nil ? 2.5 : 5))
            if player.toast?.id == toast.id { player.toast = nil }
        }
    }

    package init(player: RecordingsPlayer, toast: PlayerToast) {
        self.player = player
        self.toast = toast
    }
}

/// One camera's cell over the picture: its name and state, its picture
/// buttons (the tile header model, `TileHeaderLayout`, with the live tiles'
/// order and overflow), a click to make it large, a double click to show it
/// alone, a drag that moves a zoomed or filled picture, and its menu.
package struct PlayerCellOverlay: View {
    package let browser: RecordingsBrowser
    package let player: RecordingsPlayer
    package let index: Int
    package let cell: RecordingsPlayer.Cell
    package let rect: CGRect
    @State private var hovering = false
    @State private var lastDrag = CGSize.zero
    @State private var flash = 0.0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    package var body: some View {
        let large: Bool = {
            switch PlayerGrid.normalized(player.mode, count: player.cells.count) {
            case .emphasis(let i), .single(let i): i == index
            case .grid: false
            }
        }()
        let incident = browser.playingIncident
        let camera = cell.camera
        let title = browser.cameraName(camera)
        let tile = player.headerTile(index)
        let titleWidth = Double((title as NSString).size(withAttributes: [.font: NSFont.systemFont(ofSize: 11, weight: .medium)]).width) + 14
        let layout = TileHeaderLayout.compute(width: Double(rect.width), titleWidth: titleWidth, tile: tile)
        ZStack(alignment: .topLeading) {
            Rectangle().fill(Color.clear).contentShape(Rectangle())
            HStack(spacing: 6) {
                HStack(spacing: 4) {
                    Text(title).font(.caption.weight(.medium)).lineLimit(1)
                    if player.audibleCamera == camera {
                        Image(systemName: "speaker.wave.2.fill").font(.caption2).help("Its sound plays")
                    }
                    if let mode = Self.viewName(player, camera) {
                        Text(mode).font(.caption2).foregroundStyle(.white.opacity(0.8))
                    }
                    if hovering || player.streams.quality(camera) != .auto {
                        Text(player.streams.label(camera)).font(.caption2).foregroundStyle(.white.opacity(0.8))
                    }
                    if let state = Self.stateText(cell) {
                        Text(state).font(.caption2).foregroundStyle(.white.opacity(0.8))
                    }
                }
                .foregroundStyle(.white)
                .padding(.horizontal, 6).padding(.vertical, 3)
                .background(Color.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 4))
                Spacer(minLength: 0)
                if hovering {
                    HStack(spacing: 0) {
                        ForEach(layout.visible.filter { $0 != .title && $0 != .recordingPill }, id: \.rawValue) { item in
                            PlayerHeaderButton(player: player, index: index, item: item, layout: layout)
                        }
                    }
                    .padding(.horizontal, 2)
                    .background(Color.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 4))
                }
            }
            .padding(6)
            if let message = Self.message(cell) {
                Text(message).font(.callout).foregroundStyle(.white.opacity(0.8))
                    .multilineTextAlignment(.center).padding(.horizontal, 8)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 2).strokeBorder(large ? Color.accentColor : Color.clear, lineWidth: 2))
        .overlay(Color.white.opacity(flash).allowsHitTesting(false))
        .onTapGesture(count: 2) { player.doubleClick(index) }
        .onTapGesture { player.click(index) }
        .gesture(
            DragGesture(minimumDistance: 3)
                .onChanged { v in
                    let delta = CGVector(dx: v.translation.width - lastDrag.width, dy: v.translation.height - lastDrag.height)
                    lastDrag = v.translation
                    player.stage?.pan(camera: camera, by: delta)
                }
                .onEnded { _ in lastDrag = .zero })
        .pointerStyle(player.stage?.picturePans(camera) == true ? .grabIdle : nil)
        .onHover { inside in
            hovering = inside
            if inside { player.hovered = index } else if player.hovered == index { player.hovered = nil }
        }
        .onChange(of: player.flashCount) {
            guard player.flashCamera == camera, !reduceMotion else { return }
            flash = 0.55
            withAnimation(.easeOut(duration: 0.25)) { flash = 0 }
        }
        .contextMenu {
            if player.cells.count > 1 {
                if case .single(let i) = PlayerGrid.normalized(player.mode, count: player.cells.count), i == index {
                    Button("Show All Cameras") { player.mode = .grid }
                } else {
                    Button("Show Alone") { player.mode = .single(index) }
                    Button("Make Large") { player.mode = .emphasis(index) }
                }
                Divider()
            }
            PlayerTileMenu(player: player, index: index, entries: layout.contextMenu)
            if let incident, incident.involves(camera) {
                Divider()
                Button("Remove from Incident") { browser.remove(camera, from: incident.key) }
                    .disabled(incident.cameras.count < 2)
            }
        }
        .help("Click to make it large, double-click to show it alone")
    }

    /// The dewarped view's name, as a live tile's header names it.
    package static func viewName(_ player: RecordingsPlayer, _ camera: RecordingCamera) -> String? {
        let mode = player.tileViews.fisheyeSetting(camera).mode
        return mode.dewarps ? mode.title : nil
    }

    /// The message across a cell without a picture: no recording, or the
    /// device refused the play (with its status, FFI-RECORDINGS.md).
    package static func message(_ c: RecordingsPlayer.Cell) -> String? {
        switch c.state {
        case "no_recording": "No recording at this time"
        case "failed" where c.reason == "start_rejected":
            c.status.map { "The NVR refused this playback (status \($0))" }
        default: nil
        }
    }

    package static func stateText(_ c: RecordingsPlayer.Cell) -> String? {
        switch c.state {
        case "", "buffering": c.tile.latestPicture() == nil ? "Loading" : nil
        case "late": "Catching up"
        case "ended": "End of recording"
        case "failed":
            switch c.reason {
            case "device_in_use": "Playing in another window"
            case "start_rejected" where c.status != nil: "Refused"
            default: "Unavailable"
            }
        default: nil
        }
    }

    package init(browser: RecordingsBrowser, player: RecordingsPlayer, index: Int, cell: RecordingsPlayer.Cell, rect: CGRect) {
        self.browser = browser
        self.player = player
        self.index = index
        self.cell = cell
        self.rect = rect
    }
}

// MARK: Transport

package struct TransportBar: View {
    @Bindable package var browser: RecordingsBrowser
    @Bindable package var player: RecordingsPlayer

    package var body: some View {
        // The device playback's transport, or the follow cut's or grid's.
        let d = browser.driver
        let t = d.transport
        let cameras = browser.playerView == .cameras
        VStack(spacing: 6) {
            HStack(spacing: 8) {
                Scrubber(browser: browser, driver: d)
                if cameras, player.playback != nil, browser.playingIncident != nil {
                    Picker("", selection: $browser.scrubScope) {
                        ForEach(RecordingsBrowser.ScrubScope.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                    .controlSize(.small)
                    .help("The scrubber spans the incident or the whole day")
                }
            }
            .frame(height: 30)
            HStack(spacing: 10) {
                Group {
                    Button { d.skip(by: -TransportState.skipMs) } label: { Image(systemName: "gobackward.10") }
                        .help("Back 10 seconds")
                    Button { d.togglePlay() } label: {
                        Image(systemName: t.showsPlay ? "play.fill" : "pause.fill").frame(width: 18)
                    }
                    .help(t.showsPlay ? "Play (Space)" : "Pause (Space)")
                    Button { d.skip(by: TransportState.skipMs) } label: { Image(systemName: "goforward.10") }
                        .help("Forward 10 seconds")
                    Text(TransportState.clock(t.positionMs)).font(.callout.monospacedDigit())
                        .help("The device's own time at the playhead")
                    if t.state == .buffering { ProgressView().controlSize(.small).help("Loading") }
                    if t.state == .ended { Text("End").font(.caption).foregroundStyle(.secondary) }
                    Menu(PlaybackSpeed.label(d.rate)) {
                        ForEach(PlaybackSpeed.all, id: \.self) { r in
                            Button {
                                d.setRate(r)
                            } label: {
                                if r == d.rate { Label(PlaybackSpeed.label(r), systemImage: "checkmark") } else { Text(PlaybackSpeed.label(r)) }
                            }
                        }
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .help("Playback speed")
                    if let hint = PlaybackSpeed.hint(d.rate) {
                        Text(hint).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            .help("From 2× up only keyframes show and there is no sound; below 1× there is no sound either")
                    }
                    Spacer(minLength: 4)
                    if cameras, let incident = browser.playingIncident {
                        let at = browser.splitSecond(incident)
                        Button { browser.splitHere(incident) } label: { Label("Split Here", systemImage: "scissors") }
                            .disabled(at == nil)
                            .help(SplitHelp.text)
                    }
                    Button { d.muted.toggle() } label: {
                        Image(systemName: d.muted ? "speaker.slash.fill" : "speaker.wave.2.fill").frame(width: 18)
                    }
                    .help(d.muted ? "Unmute" : "Mute")
                    Slider(value: Binding(get: { Double(d.volume) }, set: { d.volume = Float($0) }), in: 0...1)
                        .frame(width: 80)
                        .controlSize(.small)
                        .disabled(d.muted)
                        .help("Volume")
                    if cameras, player.cells.count > 1 {
                        Button {
                            let i = PlayerGrid.audibleIndex(player.mode, count: player.cells.count) ?? 0
                            player.doubleClick(i)
                        } label: {
                            Image(systemName: {
                                if case .single = PlayerGrid.normalized(player.mode, count: player.cells.count) { return "square.grid.2x2" }
                                return "square"
                            }())
                        }
                        .help("One camera, or all cameras")
                    }
                }
                .disabled(!d.canPlay)
                PlayerViewMenu(browser: browser)
            }
            .buttonStyle(.borderless)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
    }
}

/// The player's view menu: the incident's cameras as the device plays them,
/// or, once the incident is downloaded, a follow cut or a 2-up or 4-up grid
/// made of its files.
package struct PlayerViewMenu: View {
    package let browser: RecordingsBrowser

    package var body: some View {
        let editable = browser.editable != nil
        Menu {
            ForEach(RecordingsBrowser.PlayerView.allCases) { v in
                Button {
                    browser.show(view: v)
                } label: {
                    let title = v == .cameras ? "Cameras (Device Playback)" : v.rawValue
                    if v == browser.playerView { Label(title, systemImage: "checkmark") } else { Text(title) }
                }
                .disabled(v != .cameras && !editable)
                if v == .cameras { Divider() }
            }
            if !editable {
                Divider()
                Text("Download the incident for a follow cut or a grid")
            }
        } label: {
            Label(browser.playerView.rawValue, systemImage: Self.symbol(browser.playerView))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("What the player shows: the cameras, or a follow cut or grid of the downloaded incident")
    }

    package static func symbol(_ v: RecordingsBrowser.PlayerView) -> String {
        switch v {
        case .cameras: "rectangle.grid.2x2"
        case .follow: "film.stack"
        case .grid2: "rectangle.split.2x1"
        case .grid4: "square.grid.2x2"
        }
    }

    package init(browser: RecordingsBrowser) {
        self.browser = browser
    }
}

/// The scrubber across the incident (with its margins) or the day, or the
/// follow cut's or grid's range: the events marked by kind, the follow cut's
/// cuts as white ticks, the playhead, drag to scrub (keyframes while
/// dragging, the exact picture on release).
package struct Scrubber: View {
    package let browser: RecordingsBrowser
    package let driver: any PlayerTransportDriver
    @State private var lastSent = Date.distantPast
    /// The point a press on the scrubber grabbed, if it did.
    @State private var grabbed: InOutPoints.Handle?
    @State private var pressed = false

    package var body: some View {
        GeometryReader { geo in
            let range = browser.scrubRange
            let events = scrubEvents
            let cuts = browser.playerView == .follow ? browser.editPlayer.cuts : []
            let position = driver.transport.positionMs
            let points = browser.points
            Canvas { ctx, size in
                let track = CGRect(x: 0, y: size.height / 2 - 4, width: size.width, height: 8)
                ctx.fill(Path(roundedRect: track, cornerRadius: 4), with: .color(.secondary.opacity(0.18)))
                guard let range else { return }
                if !points.isEmpty {
                    let x0 = points.inMs.map { TransportState.fraction($0, from: range.start, to: range.end) * size.width } ?? 0
                    let x1 = points.outMs.map { TransportState.fraction($0, from: range.start, to: range.end) * size.width } ?? size.width
                    ctx.fill(Path(CGRect(x: x0, y: track.minY - 2, width: max(x1 - x0, 1), height: track.height + 4)), with: .color(.accentColor.opacity(0.28)))
                    var flags: [(CGFloat, CGFloat)] = []
                    if points.inMs != nil { flags.append((x0, 6)) }
                    if points.outMs != nil { flags.append((x1, -6)) }
                    for (x, dx) in flags {
                        var flag = Path()
                        flag.move(to: CGPoint(x: x, y: 1))
                        flag.addLine(to: CGPoint(x: x, y: size.height - 1))
                        flag.addLine(to: CGPoint(x: x + dx, y: size.height - 1))
                        flag.addLine(to: CGPoint(x: x + dx, y: 1))
                        flag.closeSubpath()
                        ctx.fill(flag, with: .color(.accentColor))
                    }
                }
                for e in events {
                    let x0 = TransportState.fraction(e.start * 1000, from: range.start, to: range.end) * size.width
                    let x1 = TransportState.fraction((e.end + 1) * 1000, from: range.start, to: range.end) * size.width
                    let r = CGRect(x: x0, y: track.minY, width: max(3, x1 - x0), height: track.height)
                    ctx.fill(Path(roundedRect: r, cornerRadius: 2), with: .color(IndicatorKind(detectionKind: e.kind).laneColor))
                }
                for c in cuts {
                    let x = TransportState.fraction(c, from: range.start, to: range.end) * size.width
                    let tick = CGRect(x: x - 1, y: track.minY - 4, width: 2, height: track.height + 8)
                    ctx.fill(Path(tick), with: .color(.white))
                    ctx.stroke(Path(tick), with: .color(.black.opacity(0.5)), lineWidth: 0.5)
                }
                let x = TransportState.fraction(position, from: range.start, to: range.end) * size.width
                ctx.fill(Path(CGRect(x: x - 1, y: 2, width: 2, height: size.height - 4)), with: .color(.primary))
                ctx.fill(Path(ellipseIn: CGRect(x: x - 5, y: size.height / 2 - 5, width: 10, height: 10)), with: .color(.primary))
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { v in
                        guard let range else { return }
                        let ms = TransportState.time(atFraction: v.location.x / geo.size.width, from: range.start, to: range.end)
                        if !pressed {
                            pressed = true
                            grabbed = points.handle(
                                atFraction: v.startLocation.x / max(geo.size.width, 1), range: range.start...range.end,
                                tolerance: 8 / max(geo.size.width, 1))
                        }
                        if let grabbed {
                            browser.movePoint(grabbed, to: ms)
                            return
                        }
                        // A seek at most every 0.1 s while dragging (the core
                        // coalesces them too); keyframes are enough there.
                        if Date().timeIntervalSince(lastSent) >= 0.1 {
                            lastSent = Date()
                            driver.seek(to: ms, exact: false)
                        }
                    }
                    .onEnded { v in
                        defer {
                            pressed = false
                            grabbed = nil
                        }
                        guard let range, grabbed == nil else { return }
                        lastSent = .distantPast
                        driver.seek(to: TransportState.time(atFraction: v.location.x / geo.size.width, from: range.start, to: range.end), exact: true)
                    })
            .help(scrubHelp(range: range, cuts: cuts.count))
        }
    }

    private func scrubHelp(range: (start: Int64, end: Int64)?, cuts: Int) -> String {
        guard let range else { return "" }
        let span = "\(TransportState.clock(range.start))–\(TransportState.clock(range.end))"
        let marks = browser.points.isEmpty ? "" : "; in and out: " + browser.points.text(clock: TransportState.clock)
        return (cuts > 0 ? "\(span); white marks are the follow cut's cuts" : span) + marks
    }

    /// The events the scrubber marks: the incident's, or the day's of the
    /// cameras playing.
    private var scrubEvents: [RecordedEvent] {
        if browser.playerView != .cameras { return browser.editable?.incident.events ?? [] }
        if browser.scrubScope == .incident, let i = browser.playingIncident { return i.events }
        let cams = Set(browser.player.cameras)
        return browser.lanes.filter { cams.contains($0.camera) }.flatMap(\.events)
    }

    package init(browser: RecordingsBrowser, driver: any PlayerTransportDriver) {
        self.browser = browser
        self.driver = driver
    }
}

// MARK: Timeline

package struct TimelineLanes: View {
    @Bindable package var browser: RecordingsBrowser
    private let labelWidth: CGFloat = 130

    package var body: some View {
        let laneHeight = RecordingsPanes.laneHeight, rowHeight = RecordingsPanes.rowHeight(loudness: browser.loudnessHeight)
        VStack(spacing: 0) {
            if browser.lanes.isEmpty {
                Text("Choose the cameras to search to see their recordings here.")
                    .font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                GeometryReader { geo in
                    // The lanes draw `timeline`'s part of the day.
                    let viewport = max(geo.size.width - labelWidth - 20, 100)
                    let zoom = browser.timeline.zoom, start = browser.timeline.start
                    ScrollView(.vertical) {
                        HStack(alignment: .top, spacing: 0) {
                            VStack(alignment: .leading, spacing: 0) {
                                HStack(spacing: 2) {
                                    Button { browser.zoomTimeline(by: 0.5) } label: { Image(systemName: "minus.magnifyingglass") }
                                        .disabled(browser.timeline.zoom <= TimelineViewport.minZoom)
                                        .help("Zoom out (or the mouse wheel over the lanes)")
                                    Button { browser.zoomTimeline(by: 2) } label: { Image(systemName: "plus.magnifyingglass") }
                                        .disabled(browser.timeline.zoom >= TimelineViewport.maxZoom)
                                        .help("Zoom in (or the mouse wheel over the lanes)")
                                }
                                .buttonStyle(.borderless)
                                .controlSize(.small)
                                .frame(height: RecordingsPanes.rulerHeight)
                                ForEach(browser.lanes) { lane in
                                    HStack(spacing: 4) {
                                        if browser.player.cameras.contains(lane.camera) {
                                            Image(systemName: "play.fill").font(.system(size: 7)).foregroundStyle(Color.accentColor)
                                                .help("In the player")
                                        }
                                        Text(lane.name).font(.caption).lineLimit(1)
                                        if lane.failed {
                                            Image(systemName: "exclamationmark.triangle").font(.caption)
                                                .help("The device could not search this day; use Refresh")
                                        }
                                        if browser.model.isListening(lane.camera) {
                                            Image(systemName: "ear").font(.system(size: 8)).foregroundStyle(.secondary)
                                                .help("Listening for sounds")
                                        }
                                        if let analysis = browser.analysis(of: lane.camera) {
                                            ProgressView().controlSize(.mini).scaleEffect(0.7)
                                                .help(SoundAnalysis.progress(analysis) ?? "")
                                            Button { browser.cancelAnalysis(lane.camera) } label: {
                                                Image(systemName: "xmark.circle.fill").font(.system(size: 9))
                                            }
                                            .buttonStyle(.borderless)
                                            .help("Cancel analyzing this camera's sound")
                                            .accessibilityLabel("Cancel sound analysis")
                                        }
                                    }
                                    .contextMenu {
                                        Button("Analyze Sounds") { browser.analyzeSound(lane.camera) }
                                            .disabled(browser.model.isAnalyzing(lane.camera, day: browser.day))
                                    }
                                    .frame(width: labelWidth - 8, height: laneHeight, alignment: .leading)
                                    .padding(.bottom, rowHeight - laneHeight)
                                }
                            }
                            .padding(.leading, 8)
                            ZStack(alignment: .topLeading) {
                                VStack(alignment: .leading, spacing: 0) {
                                    HourRuler(day: browser.day, zoom: zoom, start: start)
                                        .frame(width: viewport, height: RecordingsPanes.rulerHeight)
                                    ForEach(browser.lanes) { lane in
                                        LaneView(browser: browser, lane: lane, zoom: zoom, start: start)
                                            .frame(width: viewport, height: laneHeight)
                                        LoudnessCell(browser: browser, lane: lane, zoom: zoom, start: start, width: viewport)
                                            .frame(width: viewport, height: rowHeight - laneHeight)
                                    }
                                    LoudnessLegend().frame(height: RecordingsPanes.legendHeight)
                                }
                                InOutBand(browser: browser, zoom: zoom, start: start)
                                    .frame(
                                        width: viewport,
                                        height: RecordingsPanes.rulerHeight + CGFloat(browser.lanes.count) * rowHeight)
                                    .allowsHitTesting(false)
                                Playhead(browser: browser, zoom: zoom, start: start)
                                    .frame(
                                        width: viewport,
                                        height: RecordingsPanes.rulerHeight + CGFloat(browser.lanes.count) * rowHeight)
                                    .allowsHitTesting(false)
                            }
                            .frame(width: viewport, alignment: .leading)
                            .clipped()
                            .background(TimelineInputCatcher(browser: browser))
                            .padding(.trailing, 12)
                        }
                    }
                    .onAppear { browser.lanesWidth = Double(viewport) }
                    .onChange(of: viewport) { _, w in browser.lanesWidth = Double(w) }
                }
            }
        }
    }
}

/// What a wheel, a two-finger scroll, or a pinch over the timeline lanes
/// does (docs/UX.md "Recordings", as a tile's digital zoom): a mouse wheel
/// (line-based) zooms around the pointer, each notch 1.25x, eased; Shift and
/// the wheel pans; precise horizontal scrolling (trackpad, Magic Mouse)
/// pans, and with Option held zooms; a pinch zooms. Precise vertical
/// scrolling is left to the lanes' own vertical scrolling.
package enum TimelineInput: Equatable {
    case zoom(factor: Double, eased: Bool)
    /// Points of the lanes' width (positive: later in the day).
    case pan(points: Double)
    /// Line notches (positive: later in the day).
    case panLines(Double)

    package init?(_ e: NSEvent) {
        switch e.type {
        case .magnify:
            self = .zoom(factor: 1 + Double(e.magnification), eased: false)
        case .scrollWheel:
            let dx = Double(e.scrollingDeltaX), dy = Double(e.scrollingDeltaY)
            if e.hasPreciseScrollingDeltas {
                if e.modifierFlags.contains(.option), dy != 0 {
                    self = .zoom(factor: exp(dy * 0.01), eased: false)
                } else if abs(dx) > abs(dy) {
                    self = .pan(points: -dx)
                } else {
                    return nil
                }
            } else if e.modifierFlags.contains(.shift) || (dy == 0 && dx != 0) {
                // Shift and the wheel (the system may already have made it
                // horizontal).
                let n = dx != 0 ? dx : dy
                guard n != 0 else { return nil }
                self = .panLines(-n)
            } else if dy != 0 {
                self = .zoom(factor: TimelineViewport.wheelFactor(notches: dy), eased: true)
            } else {
                return nil
            }
        default:
            return nil
        }
    }
}

extension RecordingsBrowser {
    /// Applies wheel, scroll, or pinch input at the pointer `p` (0 at the
    /// lanes' left edge, 1 at their right) over lanes `width` points wide.
    package func timelineInput(_ input: TimelineInput, at p: Double, width: Double) {
        switch input {
        case .zoom(let factor, let eased):
            let next = timeline.zoomed(by: factor, at: p)
            if eased {
                withAnimation(.easeOut(duration: 0.15)) { timeline = next }
            } else {
                timeline = next
            }
        case .pan(let points):
            timeline = timeline.panned(by: points / max(width, 1))
        case .panLines(let n):
            let next = timeline.panned(by: n * TimelineViewport.lineStep)
            withAnimation(.easeOut(duration: 0.15)) { timeline = next }
        }
    }

    /// The zoom buttons: around the middle of what is shown.
    package func zoomTimeline(by factor: Double) {
        let next = timeline.zoomed(by: factor, at: 0.5)
        withAnimation(.easeOut(duration: 0.15)) { timeline = next }
    }
}

/// Catches the lanes' wheel, scroll, and pinch events with a local event
/// monitor while its window has them over the visible lanes, so clicks
/// still reach the lanes (it is never hit) and vertical two-finger scrolls
/// still scroll them.
package struct TimelineInputCatcher: NSViewRepresentable {
    package let browser: RecordingsBrowser

    package func makeNSView(context: Context) -> CatcherView {
        let v = CatcherView()
        v.browser = browser
        return v
    }

    package func updateNSView(_ v: CatcherView, context: Context) { v.browser = browser }

    package final class CatcherView: NSView {
        package weak var browser: RecordingsBrowser?
        private var monitor: Any?

        package override func hitTest(_ point: NSPoint) -> NSView? { nil }

        package override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify]) { [weak self] e in
                // Local monitors run on the main thread.
                nonisolated(unsafe) let event = e
                let used = MainActor.assumeIsolated { self?.handle(event) == true }
                return used ? nil : e
            }
        }

        /// Handles an event over the visible lanes; false leaves it to the
        /// rest of the window.
        package func handle(_ e: NSEvent) -> Bool {
            guard e.window === window else { return false }
            return handle(e, at: convert(e.locationInWindow, from: nil))
        }

        /// Handles an event at `p` in this view's coordinates (the self-test
        /// gives the point).
        package func handle(_ e: NSEvent, at p: NSPoint) -> Bool {
            guard let browser, bounds.width > 0, visibleRect.contains(p), let input = TimelineInput(e) else { return false }
            browser.timelineInput(input, at: Double(p.x / bounds.width), width: Double(bounds.width))
            return true
        }
    }

    package init(browser: RecordingsBrowser) {
        self.browser = browser
    }
}

/// Where a second of the shown day is across the lanes: the lanes draw only
/// the visible part of the day (never a canvas the size of the zoomed day).
package struct TimelineMap {
    package var day: Int64
    package var zoom: Double
    package var start: Double
    /// The lanes' width in points.
    package var width: CGFloat

    package var pointsPerSecond: CGFloat { width * CGFloat(zoom) / CGFloat(DeviceTime.day) }

    package func x(_ second: Int64) -> CGFloat {
        (CGFloat(second - day) / CGFloat(DeviceTime.day) - CGFloat(start)) * CGFloat(zoom) * width
    }

    package func x(ms: Int64) -> CGFloat {
        (CGFloat(ms - day * 1000) / CGFloat(DeviceTime.day * 1000) - CGFloat(start)) * CGFloat(zoom) * width
    }

    package func second(atX x: CGFloat) -> Int64 {
        let f = Double(start) + Double(x / max(width, 1)) / zoom
        return day + Int64((min(max(f, 0), 1) * Double(DeviceTime.day)).rounded(.down))
    }

    package init(day: Int64, zoom: Double, start: Double, width: CGFloat) {
        self.day = day
        self.zoom = zoom
        self.start = start
        self.width = width
    }
}

/// The player's position across the lanes.
package struct Playhead: View, @preconcurrency Animatable {
    package let browser: RecordingsBrowser
    package var zoom: Double
    package var start: Double

    // Zoom and start animate together (a wheel notch eases).
    package var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    package var body: some View {
        let player = browser.player
        let edit = browser.playerView != .cameras
        let position = edit ? browser.editPlayer.positionMs : player.positionMs
        Canvas { ctx, size in
            guard edit || (player.playback != nil && player.day == browser.day) else { return }
            let x = TimelineMap(day: browser.day, zoom: zoom, start: start, width: size.width).x(ms: position)
            guard x >= -2, x <= size.width + 2 else { return }
            ctx.fill(Path(CGRect(x: x - 0.75, y: 0, width: 1.5, height: size.height)), with: .color(.red))
        }
    }

    package init(browser: RecordingsBrowser, zoom: Double, start: Double) {
        self.browser = browser
        self.zoom = zoom
        self.start = start
    }
}

package struct HourRuler: View, @preconcurrency Animatable {
    package let day: Int64
    package var zoom: Double
    package var start: Double

    // Zoom and start animate together (a wheel notch eases).
    package var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    /// Tick steps in minutes, finest first; the first at least 56 points
    /// apart is used.
    private static let steps = [5, 10, 15, 30, 60, 120, 180, 360]

    package var body: some View {
        Canvas { ctx, size in
            let map = TimelineMap(day: day, zoom: zoom, start: start, width: size.width)
            let step = Self.steps.first { CGFloat($0 * 60) * map.pointsPerSecond >= 56 } ?? 360
            let first = map.second(atX: 0) - day
            var m = Int(first / 60) / step * step
            while m <= 24 * 60 {
                let x = map.x(day + Int64(m) * 60)
                if x > size.width { break }
                if x >= -60 {
                    ctx.fill(Path(CGRect(x: x, y: size.height - 5, width: 1, height: 5)), with: .color(.secondary))
                    if m < 24 * 60 {
                        ctx.draw(
                            Text(String(format: "%02d:%02d", m / 60, m % 60)).font(.caption2).foregroundStyle(.secondary),
                            at: CGPoint(x: x + 2, y: 0), anchor: .topLeading)
                    }
                }
                m += step
            }
        }
    }

    package init(day: Int64, zoom: Double, start: Double) {
        self.day = day
        self.zoom = zoom
        self.start = start
    }
}

package struct LaneView: View, @preconcurrency Animatable {
    package let browser: RecordingsBrowser
    package let lane: RecordingsBrowser.Lane
    package var zoom: Double
    package var start: Double

    // Zoom and start animate together (a wheel notch eases).
    package var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    package var body: some View {
        let day = browser.day
        let selected = Set(browser.selected.map { i in i.events.filter { $0.camera == lane.camera } } ?? [])
        Canvas { ctx, size in
            let map = TimelineMap(day: day, zoom: zoom, start: start, width: size.width)
            ctx.fill(Path(CGRect(x: 0, y: 5, width: size.width, height: size.height - 10)), with: .color(.secondary.opacity(0.08)))
            for s in lane.spans {
                let x = map.x(s.start), w = max(1, map.x(s.end) - x)
                guard x + w >= 0, x <= size.width else { continue }
                ctx.fill(Path(CGRect(x: x, y: 8, width: w, height: size.height - 16)), with: .color(.secondary.opacity(0.35)))
            }
            for e in lane.events {
                let kind = IndicatorKind(detectionKind: e.kind)
                // A sound the core's detector or the app's labels found:
                // alarms keep their kind's color, other sounds have their own.
                let sound = SoundKinds.isSound(e) && kind == .other
                let x = map.x(e.start), w = max(3, map.x(e.end) - x)
                guard x + w >= 0, x <= size.width else { continue }
                let rect = CGRect(x: x, y: 3, width: w, height: size.height - 6)
                ctx.fill(Path(roundedRect: rect, cornerRadius: 2), with: .color(sound ? Color.soundLane : kind.laneColor))
                if selected.contains(e) {
                    ctx.stroke(Path(roundedRect: rect.insetBy(dx: -1.5, dy: -1.5), cornerRadius: 3), with: .color(.primary), lineWidth: 2)
                }
                let symbolID: AnyHashable = sound ? AnyHashable("sound") : AnyHashable(kind)
                if w >= 14, let symbol = ctx.resolveSymbol(id: symbolID) {
                    ctx.draw(symbol, at: CGPoint(x: x + w / 2, y: size.height / 2))
                }
            }
        } symbols: {
            ForEach(Array(Set(lane.events.map { IndicatorKind(detectionKind: $0.kind) })), id: \.self) { k in
                Image(systemName: k.symbolName).font(.system(size: 9)).foregroundStyle(.white).tag(k)
            }
            Image(systemName: "waveform").font(.system(size: 9)).foregroundStyle(.white).tag("sound")
        }
        .contentShape(Rectangle())
        .overlay {
            GeometryReader { geo in
                Color.clear.contentShape(Rectangle()).onTapGesture { point in
                    let map = TimelineMap(day: day, zoom: zoom, start: start, width: geo.size.width)
                    let t = map.second(atX: point.x)
                    // A generous target: the nearest event within 6 points.
                    let slack = Int64(6 / map.pointsPerSecond)
                    let near = lane.events.min { distance($0, t) < distance($1, t) }.flatMap { distance($0, t) <= slack ? $0 : nil }
                    browser.laneClick(lane.camera, at: t, near: near)
                }
            }
        }
        .help(Self.help(lane))
    }

    private static func help(_ lane: RecordingsBrowser.Lane) -> String {
        guard !lane.events.isEmpty else { return "\(lane.name): click to play from there" }
        let sounds = SoundKinds.names(lane.events)
        let heard = sounds.isEmpty ? "" : " (sounds: \(sounds.joined(separator: ", ")))"
        return "\(lane.name): \(lane.events.count) events\(heard); click to play from there"
    }

    private func distance(_ e: RecordedEvent, _ t: Int64) -> Int64 {
        t < e.start ? e.start - t : (t > e.end ? t - e.end : 0)
    }

    package init(browser: RecordingsBrowser, lane: RecordingsBrowser.Lane, zoom: Double, start: Double) {
        self.browser = browser
        self.lane = lane
        self.zoom = zoom
        self.start = start
    }
}

extension Color {
    /// The timeline's color for a sound (other than an alarm) and for loudness.
    package static let soundLane = Color(red: 0.86, green: 0.36, blue: 0.62)
    package static let loudness = Color.teal
}

/// A lane's loudness under it (`nv_recordings_loudness`): the loudest
/// second of each bucket light, its average dark, from -80 dBFS (nothing)
/// to 0 (full height). Blank where no audio was analyzed, which is not
/// silence.
package struct LoudnessStrip: View, @preconcurrency Animatable {
    package let lane: LoudnessLane?
    package let day: Int64
    package var zoom: Double
    package var start: Double
    /// The gap under the pointer, drawn stronger.
    package var hovered: LoudnessGap?

    // Zoom and start animate together (a wheel notch eases).
    package var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    package var body: some View {
        Canvas { ctx, size in
            ctx.fill(Path(CGRect(x: 0, y: 0, width: size.width, height: size.height)), with: .color(.secondary.opacity(0.05)))
            guard let lane, lane.bucket > 0 else { return }
            let map = TimelineMap(day: day, zoom: zoom, start: start, width: size.width)
            // Empty stretches say why: a label where it fits, a tint where
            // "Analyze this gap" applies.
            for g in lane.gaps {
                let x0 = max(map.x(g.start), 0), x1 = min(map.x(g.end + 1), size.width)
                guard x1 > x0 else { continue }
                if g.canAnalyze {
                    ctx.fill(Path(CGRect(x: x0, y: 0, width: x1 - x0, height: size.height)),
                        with: .color(Color.accentColor.opacity(g == hovered ? 0.18 : 0.08)))
                }
                let text = ctx.resolve(Text(g.label).font(.system(size: 9)).foregroundColor(.secondary))
                let tw = text.measure(in: size).width
                if size.height >= 14, x1 - x0 >= tw + 8 {
                    ctx.draw(text, at: CGPoint(x: (x0 + x1) / 2, y: size.height / 2), anchor: .center)
                }
            }
            let w = max(map.pointsPerSecond * CGFloat(lane.bucket), 1)
            let first = max(0, Int((map.second(atX: 0) - lane.start) / lane.bucket))
            guard first < lane.peak.count else { return }
            for i in first..<lane.peak.count {
                let x = map.x(lane.start + Int64(i) * lane.bucket)
                if x > size.width { break }
                guard let p = lane.peak[i] else { continue }
                let hp = max(1, size.height * LoudnessScale.level(p))
                ctx.fill(Path(CGRect(x: x, y: size.height - hp, width: w, height: hp)), with: .color(Color.loudness.opacity(0.35)))
                if i < lane.rms.count, let r = lane.rms[i] {
                    let hr = size.height * LoudnessScale.level(r)
                    ctx.fill(Path(CGRect(x: x, y: size.height - hr, width: w, height: hr)), with: .color(Color.loudness.opacity(0.85)))
                }
            }
        }
    }

    package static func help(_ lane: LoudnessLane?, listening: Bool, gap: LoudnessGap? = nil) -> String {
        if let gap { return gap.help }
        if lane?.heard == true { return "Loudness: light is the loudest second, dark the average" }
        return listening
            ? "No sound analyzed on this day yet"
            : "No sound analyzed: sound is analyzed while the camera is on screen or listens for sounds, or for a downloaded day with Analyze Sounds"
    }

    package init(lane: LoudnessLane?, day: Int64, zoom: Double, start: Double, hovered: LoudnessGap? = nil) {
        self.lane = lane
        self.day = day
        self.zoom = zoom
        self.start = start
        self.hovered = hovered
    }
}

/// One lane's loudness strip with its gap hover text, the "Analyze this
/// gap" menu, and a handle on its lower edge to drag the strips' height.
package struct LoudnessCell: View {
    @Bindable package var browser: RecordingsBrowser
    package let lane: RecordingsBrowser.Lane
    package let zoom: Double
    package let start: Double
    package let width: CGFloat
    /// Where the pointer is (kept after it leaves, so a context menu knows
    /// where it opened) and the height when a drag began.
    @State private var hoverX: CGFloat?
    @State private var lastX: CGFloat?
    @State private var dragFrom: CGFloat?

    private func gap(at x: CGFloat?, _ loud: LoudnessLane?) -> LoudnessGap? {
        guard let x, let loud else { return nil }
        return loud.gap(at: TimelineMap(day: browser.day, zoom: zoom, start: start, width: width).second(atX: x))
    }

    package var body: some View {
        let loud = browser.loudness(of: lane.camera)
        let under = gap(at: hoverX, loud)
        LoudnessStrip(lane: loud, day: browser.day, zoom: zoom, start: start, hovered: under)
            .onContinuousHover { phase in
                switch phase {
                case .active(let p):
                    hoverX = p.x
                    lastX = p.x
                case .ended: hoverX = nil
                }
            }
            .help(LoudnessStrip.help(loud, listening: browser.model.isListening(lane.camera), gap: under))
            .contextMenu {
                if let g = gap(at: lastX, loud) {
                    Button("Analyze this gap") { browser.analyzeGap(g, of: lane.camera) }
                        .disabled(!g.canAnalyze || browser.model.isAnalyzing(LoudnessScale.microphone(of: lane.camera), day: browser.day))
                    Text(g.help)
                } else {
                    Text("Right-click an empty stretch to analyze it")
                }
            }
            .overlay(alignment: .bottom) {
                Rectangle().fill(Color.clear).frame(height: 5).contentShape(Rectangle())
                    .onHover { inside in if inside { NSCursor.resizeUpDown.push() } else { NSCursor.pop() } }
                    .gesture(
                        DragGesture(minimumDistance: 1, coordinateSpace: .global)
                            .onChanged { v in
                                if dragFrom == nil { dragFrom = browser.loudnessHeight }
                                browser.loudnessHeight = RecordingsPanes.clampLoudness(Double((dragFrom ?? 0) + v.translation.height))
                            }
                            .onEnded { _ in dragFrom = nil }
                    )
                    .help("Drag to change the height of the loudness lanes")
                    .accessibilityLabel("Loudness lane height")
            }
    }
}

/// What the loudness strips show.
package struct LoudnessLegend: View {
    package var body: some View {
        HStack(spacing: 4) {
            Text("Loudness:")
            RoundedRectangle(cornerRadius: 1).fill(Color.loudness.opacity(0.35)).frame(width: 10, height: 7)
            Text("loudest")
            RoundedRectangle(cornerRadius: 1).fill(Color.loudness.opacity(0.85)).frame(width: 10, height: 7)
            Text("average")
            Text("· blank: not analyzed, not recorded, or no audio (not silence)")
            RoundedRectangle(cornerRadius: 1).fill(Color.soundLane).frame(width: 10, height: 7).padding(.leading, 6)
            Text("a sound")
            Spacer(minLength: 0)
        }
        .font(.system(size: 9))
        .foregroundStyle(.secondary)
        .padding(.top, 2)
    }
}


// MARK: Export

/// The in and out points over the timeline lanes.
package struct InOutBand: View {
    package let browser: RecordingsBrowser
    package let zoom: Double
    package let start: Double

    package var body: some View {
        let points = browser.points
        Canvas { ctx, size in
            guard browser.player.playback != nil || browser.playerView != .cameras, !points.isEmpty else { return }
            let map = TimelineMap(day: browser.day, zoom: zoom, start: start, width: size.width)
            let x0 = points.inMs.map { map.x(ms: $0) } ?? 0, x1 = points.outMs.map { map.x(ms: $0) } ?? size.width
            ctx.fill(Path(CGRect(x: x0, y: 0, width: max(x1 - x0, 1), height: size.height)), with: .color(.accentColor.opacity(0.12)))
            var edges: [CGFloat] = []
            if points.inMs != nil { edges.append(x0) }
            if points.outMs != nil { edges.append(x1) }
            for x in edges {
                ctx.fill(Path(CGRect(x: x - 0.75, y: 0, width: 1.5, height: size.height)), with: .color(.accentColor))
            }
        }
    }

    package init(browser: RecordingsBrowser, zoom: Double, start: Double) {
        self.browser = browser
        self.zoom = zoom
        self.start = start
    }
}

extension RecordingsBrowser {
    /// The cameras a trim can name: the playing ones, or the edit's.
    package var trimCameras: [RecordingCamera] {
        playerView == .cameras ? player.cameras : (editPlayer.plan?.sources.map(\.camera) ?? [])
    }
}

/// A running or just-ended export: progress, cancel, and what was written.
package struct ExportStrip: View {
    package let job: ExportJob
    package let export: ExportModel

    package var body: some View {
        HStack(spacing: 8) {
            if job.running {
                Text(job.title).font(.caption)
                ProgressView(value: job.fraction).progressViewStyle(.linear).frame(maxWidth: 260)
                Button("Cancel") { job.cancel() }.buttonStyle(.link).font(.caption)
            } else {
                Text(job.result ?? "Done").font(.caption).lineLimit(1)
                if !job.files.isEmpty {
                    Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting(job.files) }.buttonStyle(.link).font(.caption)
                }
                Button("Dismiss") { export.dismissJob() }.buttonStyle(.link).font(.caption)
            }
            Spacer()
        }
        .padding(.horizontal, 12).padding(.vertical, 4)
        .background(.quaternary.opacity(0.4))
    }

    package init(job: ExportJob, export: ExportModel) {
        self.job = job
        self.export = export
    }
}

/// One header button of a player cell: Save Picture, Reset Zoom, the Fisheye
/// View menu, or "…" with what did not fit.
package struct PlayerHeaderButton: View {
    package let player: RecordingsPlayer
    package let index: Int
    package let item: TileHeaderItem
    package let layout: TileHeaderLayout

    package var body: some View {
        switch item {
        case .quality:
            Menu {
                PlayerQualityMenu(player: player, index: index)
            } label: {
                Image(systemName: Self.symbol(.quality))
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
            .frame(width: 26, height: 22)
            .help("Quality")
            .accessibilityLabel("Quality")
        case .fisheye:
            Menu {
                PlayerFisheyeMenu(player: player, index: index)
            } label: {
                Image(systemName: "pano")
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
            .frame(width: 26, height: 22)
            .help("Fisheye View")
            .accessibilityLabel("Fisheye View")
        case .more:
            Menu {
                PlayerTileMenu(player: player, index: index, entries: layout.moreMenu)
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
            .frame(width: 26, height: 22)
            .help("More")
            .accessibilityLabel("More")
        default:
            let label = TileHeaderLayout.menu(for: layout.tile).first { $0.item == item }?.label ?? ""
            Button { player.perform(item, at: index) } label: {
                Image(systemName: Self.symbol(item)).frame(width: 26, height: 22)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.white)
            .help(label)
            .accessibilityLabel(label)
        }
    }

    package static func symbol(_ item: TileHeaderItem) -> String {
        switch item {
        case .savePicture: "camera"
        case .resetZoom: "1.magnifyingglass"
        case .quality: "slider.horizontal.3"
        default: "questionmark"
        }
    }

    package init(player: RecordingsPlayer, index: Int, item: TileHeaderItem, layout: TileHeaderLayout) {
        self.player = player
        self.index = index
        self.item = item
        self.layout = layout
    }
}

/// The Quality menu of a player cell: Auto, High (main stream), Low
/// (sub-stream), as the live tiles' Quality menu.
package struct PlayerQualityMenu: View {
    package let player: RecordingsPlayer
    package let index: Int

    package var body: some View {
        if player.cells.indices.contains(index) {
            let camera = player.cells[index].camera
            let current = player.streams.quality(camera)
            ForEach(PlayerQuality.allCases, id: \.rawValue) { q in
                Toggle(q.title, isOn: Binding(get: { q == current }, set: { _ in player.setQuality(q, for: camera) }))
                    .help(q.detail)
            }
            Divider()
            Text("Playing the " + player.streams.label(camera))
        }
    }

    package init(player: RecordingsPlayer, index: Int) {
        self.player = player
        self.index = index
    }
}

/// The Fisheye View menu of a player cell: the view, then the camera's mount.
package struct PlayerFisheyeMenu: View {
    package let player: RecordingsPlayer
    package let index: Int

    package var body: some View {
        if player.cells.indices.contains(index) {
            let camera = player.cells[index].camera
            let current = player.tileViews.fisheyeSetting(camera).mode
            ForEach(FisheyeViewMode.allCases, id: \.rawValue) { mode in
                Toggle(mode.title, isOn: Binding(get: { mode == current }, set: { _ in player.setFisheyeMode(mode, for: camera) }))
            }
            Divider()
            Text("Mount")
            let mount = player.mount(of: camera)
            ForEach(FisheyeMount.allCases, id: \.rawValue) { m in
                Toggle(m.title, isOn: Binding(get: { m == mount }, set: { _ in player.setFisheyeMount(m, for: camera) }))
            }
        }
    }

    package init(player: RecordingsPlayer, index: Int) {
        self.player = player
        self.index = index
    }
}

/// The picture items of a player cell's menu (the right-click menu, or the
/// "…" menu): the live tiles' list and wording (`TileHeaderLayout.menu`).
package struct PlayerTileMenu: View {
    package let player: RecordingsPlayer
    package let index: Int
    package let entries: [TileHeaderMenuEntry]

    package var body: some View {
        ForEach(entries, id: \.item.rawValue) { entry in
            switch entry.item {
            case .quality:
                Menu(entry.label) { PlayerQualityMenu(player: player, index: index) }
            case .fisheye:
                Menu(entry.label) { PlayerFisheyeMenu(player: player, index: index) }
            case .fillTile:
                if player.cells.indices.contains(index) {
                    let camera = player.cells[index].camera
                    Toggle(entry.label, isOn: Binding(
                        get: { player.tileViews.fills(camera) },
                        set: { player.setFill($0, for: camera) }))
                }
            default:
                Button(entry.label) { player.perform(entry.item, at: index) }
            }
        }
    }

    package init(player: RecordingsPlayer, index: Int, entries: [TileHeaderMenuEntry]) {
        self.player = player
        self.index = index
        self.entries = entries
    }
}
