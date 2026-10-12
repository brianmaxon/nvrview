import AppKit
import NVRCore
import SwiftUI

// The middle of the Recordings window (docs/UX.md "Recordings"): the player
// with up to four cameras, its transport and scrubber, and the day's
// timeline lanes with the playhead.

struct PlayerColumn: View {
    @Bindable var browser: RecordingsBrowser
    var onStage: (PlayerStageView) -> Void = { _ in }

    var body: some View {
        GeometryReader { geo in
            let lanes = RecordingsPanes.lanesHeight(height: geo.size.height, lanes: browser.lanes.count)
            VStack(spacing: 0) {
                PlayerArea(browser: browser, player: browser.player, onStage: onStage)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                Divider()
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

struct PlayerArea: View {
    let browser: RecordingsBrowser
    let player: RecordingsPlayer
    var onStage: (PlayerStageView) -> Void
    @FocusState private var focused: Bool

    var body: some View {
        GeometryReader { geo in
            let bounds = CGRect(origin: .zero, size: geo.size).insetBy(dx: 4, dy: 4)
            let cells = PlayerGrid.cells(count: player.cells.count, mode: player.mode, in: bounds, gap: 4)
            let items: [PlayerStageView.Item] = zip(player.cells, cells).compactMap { c, r in
                r.map { PlayerStageView.Item(tile: c.tile, rect: $0) }
            }
            ZStack(alignment: .topLeading) {
                PlayerStage(items: items, onView: onStage)
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
            .frame(width: geo.size.width, height: geo.size.height)
            .background(Color.black)
            .clipped()
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress(.space) {
            player.togglePlay()
            return .handled
        }
    }
}

/// One camera's cell over the picture: its name and state, a click to make
/// it large, a double click to show it alone, and its menu.
struct PlayerCellOverlay: View {
    let browser: RecordingsBrowser
    let player: RecordingsPlayer
    let index: Int
    let cell: RecordingsPlayer.Cell
    let rect: CGRect

    var body: some View {
        let large: Bool = {
            switch PlayerGrid.normalized(player.mode, count: player.cells.count) {
            case .emphasis(let i), .single(let i): i == index
            case .grid: false
            }
        }()
        let incident = browser.playingIncident
        ZStack(alignment: .topLeading) {
            Rectangle().fill(Color.clear).contentShape(Rectangle())
            HStack(spacing: 4) {
                Text(browser.cameraName(cell.camera)).font(.caption.weight(.medium)).lineLimit(1)
                if player.audibleCamera == cell.camera {
                    Image(systemName: "speaker.wave.2.fill").font(.caption2).help("Its sound plays")
                }
                if let state = Self.stateText(cell) {
                    Text(state).font(.caption2).foregroundStyle(.white.opacity(0.8))
                }
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background(Color.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 4))
            .padding(6)
            if cell.state == "no_recording" {
                Text("No recording at this time").font(.callout).foregroundStyle(.white.opacity(0.8))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 2).strokeBorder(large ? Color.accentColor : Color.clear, lineWidth: 2))
        .onTapGesture(count: 2) { player.doubleClick(index) }
        .onTapGesture { player.click(index) }
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
            if let incident, incident.involves(cell.camera) {
                Button("Remove from Incident") { browser.remove(cell.camera, from: incident.key) }
                    .disabled(incident.cameras.count < 2)
            }
        }
        .help("Click to make it large, double-click to show it alone")
    }

    static func stateText(_ c: RecordingsPlayer.Cell) -> String? {
        switch c.state {
        case "", "buffering": c.tile.latestPicture() == nil ? "Loading" : nil
        case "late": "Catching up"
        case "ended": "End of recording"
        case "failed":
            c.reason == "device_in_use" ? "Playing in another window" : "Unavailable"
        default: nil
        }
    }
}

// MARK: Transport

struct TransportBar: View {
    @Bindable var browser: RecordingsBrowser
    @Bindable var player: RecordingsPlayer

    var body: some View {
        let t = player.transport
        let enabled = player.playback != nil
        VStack(spacing: 6) {
            HStack(spacing: 8) {
                Scrubber(browser: browser, player: player)
                if player.playback != nil, browser.playingIncident != nil {
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
                Button { player.skip(by: -TransportState.skipMs) } label: { Image(systemName: "gobackward.10") }
                    .help("Back 10 seconds")
                Button { player.togglePlay() } label: {
                    Image(systemName: t.showsPlay ? "play.fill" : "pause.fill").frame(width: 18)
                }
                .help(t.showsPlay ? "Play (Space)" : "Pause (Space)")
                Button { player.skip(by: TransportState.skipMs) } label: { Image(systemName: "goforward.10") }
                    .help("Forward 10 seconds")
                Text(TransportState.clock(t.positionMs)).font(.callout.monospacedDigit())
                    .help("The device's own time at the playhead")
                if t.state == .buffering { ProgressView().controlSize(.small).help("Loading") }
                if t.state == .ended { Text("End").font(.caption).foregroundStyle(.secondary) }
                Menu(PlaybackSpeed.label(player.rate)) {
                    ForEach(PlaybackSpeed.all, id: \.self) { r in
                        Button {
                            player.setRate(r)
                        } label: {
                            if r == player.rate { Label(PlaybackSpeed.label(r), systemImage: "checkmark") } else { Text(PlaybackSpeed.label(r)) }
                        }
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("Playback speed")
                if let hint = PlaybackSpeed.hint(player.rate) {
                    Text(hint).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                        .help("From 2× up the device sends keyframes only and no sound; below 1× there is no sound either")
                }
                Spacer(minLength: 4)
                if let incident = browser.playingIncident {
                    let at = browser.splitSecond(incident)
                    Button { browser.splitHere(incident) } label: { Label("Split Here", systemImage: "scissors") }
                        .disabled(at == nil)
                        .help(SplitHelp.text)
                }
                Button { player.muted.toggle() } label: {
                    Image(systemName: player.muted ? "speaker.slash.fill" : "speaker.wave.2.fill").frame(width: 18)
                }
                .help(player.muted ? "Unmute" : "Mute")
                Slider(value: Binding(get: { Double(player.volume) }, set: { player.volume = Float($0) }), in: 0...1)
                    .frame(width: 80)
                    .controlSize(.small)
                    .disabled(player.muted)
                    .help("Volume")
                if player.cells.count > 1 {
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
            .buttonStyle(.borderless)
            .disabled(!enabled)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
    }
}

/// The scrubber across the incident (with its margins) or the day: the
/// events marked by kind, the playhead, drag to scrub (keyframes while
/// dragging, the exact picture on release).
struct Scrubber: View {
    let browser: RecordingsBrowser
    let player: RecordingsPlayer
    @State private var lastSent = Date.distantPast

    var body: some View {
        GeometryReader { geo in
            let range = browser.scrubRange
            let events = scrubEvents
            Canvas { ctx, size in
                let track = CGRect(x: 0, y: size.height / 2 - 4, width: size.width, height: 8)
                ctx.fill(Path(roundedRect: track, cornerRadius: 4), with: .color(.secondary.opacity(0.18)))
                guard let range else { return }
                for e in events {
                    let x0 = TransportState.fraction(e.start * 1000, from: range.start, to: range.end) * size.width
                    let x1 = TransportState.fraction((e.end + 1) * 1000, from: range.start, to: range.end) * size.width
                    let r = CGRect(x: x0, y: track.minY, width: max(3, x1 - x0), height: track.height)
                    ctx.fill(Path(roundedRect: r, cornerRadius: 2), with: .color(IndicatorKind(detectionKind: e.kind).laneColor))
                }
                let x = TransportState.fraction(player.positionMs, from: range.start, to: range.end) * size.width
                ctx.fill(Path(CGRect(x: x - 1, y: 2, width: 2, height: size.height - 4)), with: .color(.primary))
                ctx.fill(Path(ellipseIn: CGRect(x: x - 5, y: size.height / 2 - 5, width: 10, height: 10)), with: .color(.primary))
            }
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { v in
                        guard let range else { return }
                        let ms = TransportState.time(atFraction: v.location.x / geo.size.width, from: range.start, to: range.end)
                        // A seek at most every 0.1 s while dragging (the core
                        // coalesces them too); keyframes are enough there.
                        if Date().timeIntervalSince(lastSent) >= 0.1 {
                            lastSent = Date()
                            player.seek(to: ms, exact: false)
                        }
                    }
                    .onEnded { v in
                        guard let range else { return }
                        lastSent = .distantPast
                        player.seek(to: TransportState.time(atFraction: v.location.x / geo.size.width, from: range.start, to: range.end))
                    })
            .help(range.map { "\(TransportState.clock($0.start))–\(TransportState.clock($0.end))" } ?? "")
        }
    }

    /// The events the scrubber marks: the incident's, or the day's of the
    /// cameras playing.
    private var scrubEvents: [RecordedEvent] {
        if browser.scrubScope == .incident, let i = browser.playingIncident { return i.events }
        let cams = Set(player.cameras)
        return browser.lanes.filter { cams.contains($0.camera) }.flatMap(\.events)
    }
}

// MARK: Timeline

struct TimelineLanes: View {
    @Bindable var browser: RecordingsBrowser
    private let labelWidth: CGFloat = 130

    var body: some View {
        let laneHeight = RecordingsPanes.laneHeight
        VStack(spacing: 0) {
            if browser.lanes.isEmpty {
                Text("Choose the cameras to search to see their recordings here.")
                    .font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                GeometryReader { geo in
                    let width = max(geo.size.width - labelWidth - 30, 100) * browser.zoom
                    ScrollView(.vertical) {
                        HStack(alignment: .top, spacing: 0) {
                            VStack(alignment: .leading, spacing: 0) {
                                HStack(spacing: 2) {
                                    Button { browser.zoom = max(1, browser.zoom / 2) } label: { Image(systemName: "minus.magnifyingglass") }
                                        .disabled(browser.zoom <= 1)
                                        .help("Zoom out")
                                    Button { browser.zoom = min(48, browser.zoom * 2) } label: { Image(systemName: "plus.magnifyingglass") }
                                        .disabled(browser.zoom >= 48)
                                        .help("Zoom in")
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
                                    }
                                    .frame(width: labelWidth - 8, height: laneHeight, alignment: .leading)
                                }
                            }
                            .padding(.leading, 8)
                            ScrollView(.horizontal) {
                                ZStack(alignment: .topLeading) {
                                    VStack(alignment: .leading, spacing: 0) {
                                        HourRuler(width: width).frame(width: width, height: RecordingsPanes.rulerHeight)
                                        ForEach(browser.lanes) { lane in
                                            LaneView(browser: browser, lane: lane, width: width)
                                                .frame(width: width, height: laneHeight)
                                        }
                                    }
                                    Playhead(browser: browser, width: width)
                                        .frame(
                                            width: width,
                                            height: RecordingsPanes.rulerHeight + CGFloat(browser.lanes.count) * laneHeight)
                                        .allowsHitTesting(false)
                                }
                                .padding(.trailing, 12)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The player's position across the lanes.
struct Playhead: View {
    let browser: RecordingsBrowser
    let width: CGFloat

    var body: some View {
        let player = browser.player
        Canvas { ctx, size in
            guard player.playback != nil, player.day == browser.day else { return }
            let x = TransportState.fraction(player.positionMs, from: browser.day * 1000, to: (browser.day + DeviceTime.day) * 1000)
                * size.width
            ctx.fill(Path(CGRect(x: x - 0.75, y: 0, width: 1.5, height: size.height)), with: .color(.red))
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
            ctx.fill(Path(CGRect(x: 0, y: 5, width: size.width, height: size.height - 10)), with: .color(.secondary.opacity(0.08)))
            for s in lane.spans {
                let x = CGFloat(s.start - day) * scale, w = max(1, CGFloat(s.end - s.start) * scale)
                ctx.fill(Path(CGRect(x: x, y: 8, width: w, height: size.height - 16)), with: .color(.secondary.opacity(0.35)))
            }
            for e in lane.events {
                let kind = IndicatorKind(detectionKind: e.kind)
                let x = CGFloat(e.start - day) * scale, w = max(3, CGFloat(e.end - e.start) * scale)
                let rect = CGRect(x: x, y: 3, width: w, height: size.height - 6)
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
            let t = day + Int64(max(0, point.x) / scale)
            // A generous target: the nearest event within 6 points.
            let slack = Int64(6 / scale)
            let near = lane.events.min { distance($0, t) < distance($1, t) }.flatMap { distance($0, t) <= slack ? $0 : nil }
            browser.laneClick(lane.camera, at: t, near: near)
        }
        .help(lane.events.isEmpty ? "\(lane.name): click to play from there" : "\(lane.name): \(lane.events.count) events; click to play from there")
    }

    private func distance(_ e: RecordedEvent, _ t: Int64) -> Int64 {
        t < e.start ? e.start - t : (t > e.end ? t - e.end : 0)
    }
}
