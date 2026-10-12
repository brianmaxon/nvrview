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
        let laneHeight = RecordingsPanes.laneHeight, rowHeight = RecordingsPanes.rowHeight
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
                                        LoudnessStrip(lane: browser.loudness(of: lane.camera), day: browser.day, zoom: zoom, start: start)
                                            .frame(width: viewport, height: rowHeight - laneHeight)
                                            .help(LoudnessStrip.help(browser.loudness(of: lane.camera), listening: browser.model.isListening(lane.camera)))
                                    }
                                    LoudnessLegend().frame(height: RecordingsPanes.legendHeight)
                                }
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
enum TimelineInput: Equatable {
    case zoom(factor: Double, eased: Bool)
    /// Points of the lanes' width (positive: later in the day).
    case pan(points: Double)
    /// Line notches (positive: later in the day).
    case panLines(Double)

    init?(_ e: NSEvent) {
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
    func timelineInput(_ input: TimelineInput, at p: Double, width: Double) {
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
    func zoomTimeline(by factor: Double) {
        let next = timeline.zoomed(by: factor, at: 0.5)
        withAnimation(.easeOut(duration: 0.15)) { timeline = next }
    }
}

/// Catches the lanes' wheel, scroll, and pinch events with a local event
/// monitor while its window has them over the visible lanes, so clicks
/// still reach the lanes (it is never hit) and vertical two-finger scrolls
/// still scroll them.
struct TimelineInputCatcher: NSViewRepresentable {
    let browser: RecordingsBrowser

    func makeNSView(context: Context) -> CatcherView {
        let v = CatcherView()
        v.browser = browser
        return v
    }

    func updateNSView(_ v: CatcherView, context: Context) { v.browser = browser }

    final class CatcherView: NSView {
        weak var browser: RecordingsBrowser?
        private var monitor: Any?

        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
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
        func handle(_ e: NSEvent) -> Bool {
            guard e.window === window else { return false }
            return handle(e, at: convert(e.locationInWindow, from: nil))
        }

        /// Handles an event at `p` in this view's coordinates (the self-test
        /// gives the point).
        func handle(_ e: NSEvent, at p: NSPoint) -> Bool {
            guard let browser, bounds.width > 0, visibleRect.contains(p), let input = TimelineInput(e) else { return false }
            browser.timelineInput(input, at: Double(p.x / bounds.width), width: Double(bounds.width))
            return true
        }
    }
}

/// Where a second of the shown day is across the lanes: the lanes draw only
/// the visible part of the day (never a canvas the size of the zoomed day).
struct TimelineMap {
    var day: Int64
    var zoom: Double
    var start: Double
    /// The lanes' width in points.
    var width: CGFloat

    var pointsPerSecond: CGFloat { width * CGFloat(zoom) / CGFloat(DeviceTime.day) }

    func x(_ second: Int64) -> CGFloat {
        (CGFloat(second - day) / CGFloat(DeviceTime.day) - CGFloat(start)) * CGFloat(zoom) * width
    }

    func x(ms: Int64) -> CGFloat {
        (CGFloat(ms - day * 1000) / CGFloat(DeviceTime.day * 1000) - CGFloat(start)) * CGFloat(zoom) * width
    }

    func second(atX x: CGFloat) -> Int64 {
        let f = Double(start) + Double(x / max(width, 1)) / zoom
        return day + Int64((min(max(f, 0), 1) * Double(DeviceTime.day)).rounded(.down))
    }
}

/// The player's position across the lanes.
struct Playhead: View, @preconcurrency Animatable {
    let browser: RecordingsBrowser
    var zoom: Double
    var start: Double

    // Zoom and start animate together (a wheel notch eases).
    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    var body: some View {
        let player = browser.player
        Canvas { ctx, size in
            guard player.playback != nil, player.day == browser.day else { return }
            let x = TimelineMap(day: browser.day, zoom: zoom, start: start, width: size.width).x(ms: player.positionMs)
            guard x >= -2, x <= size.width + 2 else { return }
            ctx.fill(Path(CGRect(x: x - 0.75, y: 0, width: 1.5, height: size.height)), with: .color(.red))
        }
    }
}

struct HourRuler: View, @preconcurrency Animatable {
    let day: Int64
    var zoom: Double
    var start: Double

    // Zoom and start animate together (a wheel notch eases).
    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    /// Tick steps in minutes, finest first; the first at least 56 points
    /// apart is used.
    private static let steps = [5, 10, 15, 30, 60, 120, 180, 360]

    var body: some View {
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
}

struct LaneView: View, @preconcurrency Animatable {
    let browser: RecordingsBrowser
    let lane: RecordingsBrowser.Lane
    var zoom: Double
    var start: Double

    // Zoom and start animate together (a wheel notch eases).
    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    var body: some View {
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
}

extension Color {
    /// The timeline's color for a sound (other than an alarm) and for loudness.
    static let soundLane = Color(red: 0.86, green: 0.36, blue: 0.62)
    static let loudness = Color.teal
}

/// A lane's loudness under it (`nv_recordings_loudness`): the loudest
/// second of each bucket light, its average dark, from -80 dBFS (nothing)
/// to 0 (full height). Blank where no audio was analyzed, which is not
/// silence.
struct LoudnessStrip: View, @preconcurrency Animatable {
    let lane: LoudnessLane?
    let day: Int64
    var zoom: Double
    var start: Double

    // Zoom and start animate together (a wheel notch eases).
    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(zoom, start) }
        set {
            zoom = newValue.first
            start = newValue.second
        }
    }

    var body: some View {
        Canvas { ctx, size in
            ctx.fill(Path(CGRect(x: 0, y: 0, width: size.width, height: size.height)), with: .color(.secondary.opacity(0.05)))
            guard let lane, lane.bucket > 0 else { return }
            let map = TimelineMap(day: day, zoom: zoom, start: start, width: size.width)
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

    static func help(_ lane: LoudnessLane?, listening: Bool) -> String {
        if lane?.heard == true { return "Loudness: light is the loudest second, dark the average" }
        return listening
            ? "No sound analyzed on this day yet"
            : "No sound analyzed: sound is analyzed while the camera is on screen or listens for sounds, or for a downloaded day with Analyze Sounds"
    }
}

/// What the loudness strips show.
struct LoudnessLegend: View {
    var body: some View {
        HStack(spacing: 4) {
            Text("Loudness:")
            RoundedRectangle(cornerRadius: 1).fill(Color.loudness.opacity(0.35)).frame(width: 10, height: 7)
            Text("loudest")
            RoundedRectangle(cornerRadius: 1).fill(Color.loudness.opacity(0.85)).frame(width: 10, height: 7)
            Text("average")
            Text("· blank: not analyzed (not silence)")
            RoundedRectangle(cornerRadius: 1).fill(Color.soundLane).frame(width: 10, height: 7).padding(.leading, 6)
            Text("a sound")
            Spacer(minLength: 0)
        }
        .font(.system(size: 9))
        .foregroundStyle(.secondary)
        .padding(.top, 2)
    }
}
