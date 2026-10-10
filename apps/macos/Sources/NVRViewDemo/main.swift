import AppKit
import SwiftUI
import VideoKit

// NVRViewDemo [--tiles N] [--fps F] [--selftest SECONDS] file.h264|file.h265 ...
// Plays Annex-B files in a loop, one per tile (files are reused round-robin).
// Double-click a tile to show it alone; double-click again to go back.
// With --selftest, plays for that long without taking focus, checks that every
// tile is showing a changing picture, prints a report, and exits 0 or 1.

struct Options {
    var tiles = 4
    var fps = 20.0
    var selfTestSeconds: Double?
    var files: [URL] = []

    static func parse(_ args: [String]) -> Options? {
        var o = Options()
        var it = args.dropFirst().makeIterator()
        while let a = it.next() {
            switch a {
            case "--tiles": guard let v = it.next().flatMap(Int.init), v > 0 else { return nil }; o.tiles = v
            case "--fps": guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.fps = v
            case "--selftest":
                guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.selfTestSeconds = v
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
    let players: [FilePlayer]
    let selfTest: SelfTest?
    var window: NSWindow?

    init(players: [FilePlayer], selfTest: SelfTest?) {
        self.players = players
        self.selfTest = selfTest
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
        // The lazy grid has almost no ideal size; keep the hosting view from
        // shrinking the window to it.
        let host = NSHostingView(rootView: GridView(players: players).frame(minWidth: 480, minHeight: 360))
        host.sizingOptions = []
        window.contentView = host
        window.setContentSize(NSSize(width: 960, height: 720))
        window.center()
        self.window = window
        if let selfTest {
            // Show the window without stealing focus from whatever the user is doing.
            window.orderFrontRegardless()
            selfTest.start(window: window)
        } else {
            window.makeKeyAndOrderFront(nil)
            NSApplication.shared.activate()
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

@MainActor
func runDemo() {
    guard let options = Options.parse(CommandLine.arguments) else {
        FileHandle.standardError.write(Data("usage: NVRViewDemo [--tiles N] [--fps F] [--selftest SECONDS] file.h264|file.h265 ...\n".utf8))
        exit(2)
    }
    var players: [FilePlayer] = []
    for i in 0..<options.tiles {
        let file = options.files[i % options.files.count]
        guard let p = FilePlayer(file: file, fps: options.fps, label: "tile\(i)") else {
            FileHandle.standardError.write(Data("cannot read \(file.lastPathComponent)\n".utf8))
            exit(1)
        }
        players.append(p)
    }
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    let selfTest = options.selfTestSeconds.map { seconds in
        SelfTest(
            players: players,
            names: players.indices.map { options.files[$0 % options.files.count].lastPathComponent },
            seconds: seconds)
    }
    let delegate = DemoDelegate(players: players, selfTest: selfTest)
    app.delegate = delegate
    withExtendedLifetime(delegate) { app.run() }
}

MainActor.assumeIsolated { runDemo() }
