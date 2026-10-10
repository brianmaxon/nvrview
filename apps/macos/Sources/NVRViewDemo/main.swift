import AppKit
import SwiftUI
import VideoKit

// NVRViewDemo [--tiles N] [--fps F] file.h264|file.h265 ...
// Plays Annex-B files in a loop, one per tile (files are reused round-robin).
// Double-click a tile to show it alone; double-click again to go back.

struct Options {
    var tiles = 4
    var fps = 20.0
    var files: [URL] = []

    static func parse(_ args: [String]) -> Options? {
        var o = Options()
        var it = args.dropFirst().makeIterator()
        while let a = it.next() {
            switch a {
            case "--tiles": guard let v = it.next().flatMap(Int.init), v > 0 else { return nil }; o.tiles = v
            case "--fps": guard let v = it.next().flatMap(Double.init), v > 0 else { return nil }; o.fps = v
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

struct DemoApp: App {
    let players: [FilePlayer]

    @MainActor
    init() {
        guard let options = Options.parse(CommandLine.arguments) else {
            FileHandle.standardError.write(Data("usage: NVRViewDemo [--tiles N] [--fps F] file.h264|file.h265 ...\n".utf8))
            exit(2)
        }
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate()
        var players: [FilePlayer] = []
        for i in 0..<options.tiles {
            let file = options.files[i % options.files.count]
            guard let p = FilePlayer(file: file, fps: options.fps, label: "tile\(i)") else {
                FileHandle.standardError.write(Data("cannot read \(file.lastPathComponent)\n".utf8))
                exit(1)
            }
            players.append(p)
        }
        self.players = players
    }

    var body: some Scene {
        WindowGroup("NVRView Demo") {
            GridView(players: players)
                .frame(minWidth: 480, minHeight: 360)
        }
    }
}

DemoApp.main()
