import AppKit
import SwiftUI
import VideoKit

// NVRViewDemo [--renderer metal|layer] [--tiles N] [--fps F]
//             [--selftest SECONDS [--until-frames N] [--window behind|front|offscreen]]
//             file.h264|file.h265 ...
// Plays Annex-B files in a loop, one per tile (files are reused round-robin).
// With --selftest, plays for that long, checks that every tile is showing a
// changing picture, prints a report, and exits 0 or 1. Its window opens
// behind all other windows (it still draws there, at a throttled rate) and
// never takes focus or shows a Dock icon. Off screen it cannot draw at all.
// With --until-frames N, the run ends as soon as every tile has shown 2N
// frames instead of after SECONDS, which then only guards against a hang.
//
// Renderers:
//   metal (default)  a CoreBridge over FakeCore (a Swift double of the C core)
//                    runs a TileDecoder (VideoToolbox) per tile; one
//                    GridRenderer (Metal) pass draws the grid.
//   layer            one AVSampleBufferDisplayLayer per tile. Double-click a
//                    tile to show it alone; double-click again to go back.

/// Where the self-test puts its window.
enum WindowPlacement: String {
    /// In front of other windows, without taking focus.
    case front
    /// Behind every other window.
    case behind
    /// Borderless, outside every screen. Nothing draws there; kept to show
    /// that a self-test fails loudly when the window cannot draw.
    case offscreen
}

enum RendererKind: String {
    case layer
    case metal
}

struct Options {
    var renderer = RendererKind.metal
    var tiles = 4
    var fps = 20.0
    var selfTestSeconds: Double?
    var selfTestFrames: Int?
    var placement = WindowPlacement.behind
    var files: [URL] = []

    static func parse(_ args: [String]) -> Options? {
        var o = Options()
        var it = args.dropFirst().makeIterator()
        while let a = it.next() {
            switch a {
            case "--renderer": guard let v = it.next().flatMap(RendererKind.init) else { return nil }; o.renderer = v
            case "--tiles": guard let v = it.next().flatMap(Int.init), v > 0 else { return nil }; o.tiles = v
            case "--fps": guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.fps = v
            case "--window": guard let v = it.next().flatMap(WindowPlacement.init) else { return nil }; o.placement = v
            case "--selftest":
                guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.selfTestSeconds = v
            case "--until-frames":
                guard let v = it.next().flatMap(Int.init), v > 0 else { return nil }; o.selfTestFrames = v
            default: o.files.append(URL(fileURLWithPath: a))
            }
        }
        return o.files.isEmpty ? nil : o
    }
}

/// Feeds one file to one renderer from a background task, as the core will.
final class FilePlayer: Sendable {
    let renderer: ChannelRenderer
    private let task: Task<Void, Never>

    @MainActor
    init?(file: URL, fps: Double, label: String) {
        let codec: VideoCodec = file.pathExtension == "h265" || file.pathExtension == "hevc" ? .h265 : .h264
        guard let data = try? Data(contentsOf: file) else { return nil }
        let units = AccessUnitSplitter.split(data, codec: codec)
        guard !units.isEmpty else { return nil }
        let renderer = ChannelRenderer(label: label)
        self.renderer = renderer
        task = Task.detached {
            let clock = ContinuousClock()
            let period = Duration.seconds(1.0 / fps)
            var next = clock.now
            var n = 0
            while !Task.isCancelled {
                let unit = units[n % units.count]
                renderer.submit(VideoFrame(
                    codec: codec, isKeyframe: unit.isKeyframe,
                    ptsMicros: Int64(Double(n) * 1_000_000 / fps),
                    annexB: data.subdata(in: unit.range)))
                n += 1
                next += period
                try? await clock.sleep(until: next)
            }
        }
    }

    deinit { task.cancel() }
}

struct LayerView: NSViewRepresentable {
    let layer: CALayer

    final class Host: NSView {
        var content: CALayer?
        override func layout() {
            super.layout()
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            content?.frame = bounds
            CATransaction.commit()
        }
    }

    func makeNSView(context: Context) -> Host {
        let v = Host()
        v.wantsLayer = true
        v.layer?.backgroundColor = NSColor.black.cgColor
        v.content = layer
        v.layer?.addSublayer(layer)  // re-parents if the layer was on another tile
        return v
    }

    func updateNSView(_ v: Host, context: Context) {
        if layer.superlayer !== v.layer { v.layer?.addSublayer(layer) }
        v.content = layer
        v.needsLayout = true
    }
}

struct TileView: View {
    let player: FilePlayer
    let index: Int

    var body: some View {
        LayerView(layer: player.renderer.displayLayer)
            .overlay(alignment: .topLeading) {
                TimelineView(.periodic(from: .now, by: 1)) { _ in
                    let s = player.renderer.stats
                    Text("#\(index)  in \(s.framesReceived)  shown \(s.framesEnqueued)  gated \(s.droppedWaitingForKeyframe)  dropped \(s.droppedQueueOverflow + s.droppedDisplayBusy)  fmt \(s.formatChanges)  fail \(s.decodeFailures)")
                        .font(.system(size: 10, design: .monospaced))
                        .padding(3)
                        .background(.black.opacity(0.6))
                        .foregroundStyle(.white)
                }
            }
    }
}

struct GridView: View {
    let players: [FilePlayer]
    @State private var focused: Int?

    var body: some View {
        if let focused, players.indices.contains(focused) {
            TileView(player: players[focused], index: focused)
                .onTapGesture(count: 2) { self.focused = nil }
        } else {
            let columns = max(1, Int(Double(players.count).squareRoot().rounded(.up)))
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 2), count: columns), spacing: 2) {
                ForEach(players.indices, id: \.self) { i in
                    TileView(player: players[i], index: i)
                        .aspectRatio(4.0 / 3.0, contentMode: .fit)
                        .onTapGesture(count: 2) { focused = i }
                }
            }
        }
    }
}

// An unbundled executable gets no window from a SwiftUI `WindowGroup`, so the
// demo uses the AppKit lifecycle and hosts the SwiftUI grid in its own window.
@MainActor
final class DemoDelegate: NSObject, NSApplicationDelegate {
    let makeContent: () -> NSView
    let startSelfTest: ((NSWindow) -> Void)?
    let onTerminate: () -> Void
    var placement = WindowPlacement.front
    var window: NSWindow?

    init(content: @escaping () -> NSView, selfTest: ((NSWindow) -> Void)?, onTerminate: @escaping () -> Void = {}) {
        makeContent = content
        startSelfTest = selfTest
        self.onTerminate = onTerminate
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 720),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "NVRView Demo"
        window.contentMinSize = NSSize(width: 480, height: 360)
        window.contentView = makeContent()
        window.setContentSize(NSSize(width: 960, height: 720))
        window.center()
        self.window = window
        if let startSelfTest {
            // Never take focus from whatever the user is doing.
            switch placement {
            case .front:
                window.orderFrontRegardless()
            case .behind:
                window.orderBack(nil)
            case .offscreen:
                window.styleMask = [.borderless]
                let all = NSScreen.screens.reduce(NSRect.zero) { $0.union($1.frame) }
                window.setFrameOrigin(NSPoint(x: all.maxX + 2000, y: all.maxY + 2000))
                window.orderBack(nil)
            }
            startSelfTest(window)
        } else {
            window.makeKeyAndOrderFront(nil)
            NSApplication.shared.activate()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        onTerminate()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

@MainActor
func runDemo() {
    guard let options = Options.parse(CommandLine.arguments) else {
        FileHandle.standardError.write(Data(
            "usage: NVRViewDemo [--renderer metal|layer] [--tiles N] [--fps F] [--selftest SECONDS [--until-frames N] [--window behind|front|offscreen]] file.h264|file.h265 ...\n".utf8))
        exit(2)
    }
    let app = NSApplication.shared
    // A self-test gets no Dock icon or menu bar.
    app.setActivationPolicy(options.selfTestSeconds == nil ? .regular : .accessory)
    let delegate: DemoDelegate
    switch options.renderer {
    case .layer: delegate = layerDemo(options)
    case .metal: delegate = metalDemo(options)
    }
    delegate.placement = options.placement
    app.delegate = delegate
    withExtendedLifetime(delegate) { app.run() }
}

@MainActor
func layerDemo(_ options: Options) -> DemoDelegate {
    var players: [FilePlayer] = []
    for i in 0..<options.tiles {
        let file = options.files[i % options.files.count]
        guard let p = FilePlayer(file: file, fps: options.fps, label: "tile\(i)") else {
            FileHandle.standardError.write(Data("cannot read \(file.lastPathComponent)\n".utf8))
            exit(1)
        }
        players.append(p)
    }
    let selfTest = options.selfTestSeconds.map { seconds in
        SelfTest(
            players: players,
            names: players.indices.map { options.files[$0 % options.files.count].lastPathComponent },
            seconds: seconds, untilFrames: options.selfTestFrames)
    }
    return DemoDelegate(
        content: {
            // The lazy grid has almost no ideal size; keep the hosting view from
            // shrinking the window to it.
            let host = NSHostingView(rootView: GridView(players: players).frame(minWidth: 480, minHeight: 360))
            host.sizingOptions = []
            return host
        },
        selfTest: selfTest.map { test in { test.start(window: $0) } })
}

@MainActor
func metalDemo(_ options: Options) -> DemoDelegate {
    guard let core = DemoCore(files: options.files, tiles: options.tiles, fps: options.fps) else {
        FileHandle.standardError.write(Data("cannot start the core on those files\n".utf8))
        exit(1)
    }
    let grid: MetalGrid
    do {
        grid = try MetalGrid(core: core)
    } catch {
        FileHandle.standardError.write(Data("cannot set up Metal: \(error)\n".utf8))
        exit(1)
    }
    let selfTest = options.selfTestSeconds.map { MetalSelfTest(grid: grid, seconds: $0, untilFrames: options.selfTestFrames) }
    return DemoDelegate(
        content: { grid.view },
        selfTest: selfTest.map { test in { test.start(window: $0) } },
        onTerminate: { core.shutdown() })
}

MainActor.assumeIsolated { runDemo() }
