import AppKit
import NVRCore
import SwiftUI
import VideoKit

/// Draws a follow cut or a grid with the Metal grid renderer: at every
/// display refresh it reads the edit's clock, asks the files for the
/// pictures of that time (decoded off the main thread, keyframes only while
/// scrubbing and from 2×), and gives the renderer the edit's tiles for the
/// view's drawable. It redraws only when a picture or the layout changed.
@MainActor
final class EditStageView: NSView {
    let grid: MetalGridView?
    weak var player: EditPlayer?
    /// The edit's area in points (top-left origin), inside the view.
    var area: CGRect = .zero

    private struct RenderKey: Equatable {
        var pictures: [ObjectIdentifier]
        var destinations: [CGRect]
        var sources: [CGRect?]
    }

    private var lastKey: RenderKey?

    override init(frame: NSRect) {
        grid = (try? GridRenderer()).map(MetalGridView.init(renderer:))
        super.init(frame: frame)
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        if let grid {
            grid.frame = bounds
            grid.autoresizingMask = [.width, .height]
            addSubview(grid)
            grid.prepare = { [weak self] renderer, drawable in
                MainActor.assumeIsolated { self?.prepare(renderer: renderer, drawable: drawable) }
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }

    var framesDrawn: Int { grid?.framesDrawn ?? 0 }

    private func prepare(renderer: GridRenderer, drawable: CGSize) {
        let points = bounds
        guard points.width > 0, points.height > 0 else { return }
        var tiles: [GridTile] = []
        if let player, let session = player.session {
            let t = player.framePosition()
            session.request(at: t, keyframesOnly: player.keyframesOnly)
            let sx = drawable.width / points.width, sy = drawable.height / points.height
            let a = area.isEmpty ? points : area
            let pixels = CGRect(x: a.minX * sx, y: a.minY * sy, width: a.width * sx, height: a.height * sy)
            tiles = session.tiles(at: t, in: pixels)
        }
        let key = RenderKey(
            pictures: tiles.map { ObjectIdentifier($0.picture) }, destinations: tiles.map(\.destination), sources: tiles.map(\.source))
        if key != lastKey {
            lastKey = key
            renderer.tiles = tiles
        }
    }
}

struct EditStage: NSViewRepresentable {
    let player: EditPlayer
    let area: CGRect
    var onView: (EditStageView) -> Void = { _ in }

    func makeNSView(context: Context) -> EditStageView {
        let v = EditStageView(frame: .zero)
        v.player = player
        onView(v)
        return v
    }

    func updateNSView(_ view: EditStageView, context: Context) {
        view.player = player
        view.area = area
    }
}

/// The follow cut or grid in the player: the pictures, and over them the
/// shown camera's name (and the device time) in the follow cut, or each
/// cell's camera in a grid, with the camera whose sound plays marked.
struct EditArea: View {
    let browser: RecordingsBrowser
    let player: EditPlayer
    var onStage: (EditStageView) -> Void

    var body: some View {
        GeometryReader { geo in
            let bounds = CGRect(origin: .zero, size: geo.size).insetBy(dx: 4, dy: 4)
            ZStack(alignment: .topLeading) {
                EditStage(player: player, area: bounds, onView: onStage)
                if let plan = player.plan {
                    overlay(plan, bounds: bounds)
                }
                if player.loading || player.message != nil {
                    VStack(spacing: 8) {
                        if player.loading { ProgressView().controlSize(.small) }
                        Text(player.message ?? "Opening the downloaded files…").font(.callout)
                    }
                    .foregroundStyle(.white.opacity(0.8))
                    .frame(width: geo.size.width, height: geo.size.height)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
            .background(Color.black)
            .clipped()
        }
        .focusable()
        .focusEffectDisabled()
        .onKeyPress(.space) {
            player.togglePlay()
            return .handled
        }
    }

    @ViewBuilder
    private func overlay(_ plan: EditPlan, bounds: CGRect) -> some View {
        let t = player.positionMs
        let audible = player.audibleSource
        switch plan.kind {
        case .follow:
            let shot = EditTimeline.shot(plan, at: t)
            let showsTime = plan.labels.isEmpty || plan.labels.contains { $0.time }
            ZStack(alignment: .topLeading) {
                if let shot, let source = plan.source(shot.source) {
                    EditLabel(
                        name: browser.cameraName(source.camera), time: showsTime ? TransportState.clock(t) : nil,
                        audible: audible == shot.source)
                } else {
                    Text("No camera has video at this time").font(.callout).foregroundStyle(.white.opacity(0.8))
                        .frame(width: bounds.width, height: bounds.height)
                }
            }
            .offset(x: bounds.minX, y: bounds.minY)
        case .grid:
            let rects = EditComposition.cellRects(plan, in: bounds)
            ForEach(Array(plan.cells.enumerated()), id: \.offset) { i, cell in
                if i < rects.count, let source = plan.source(cell.source) {
                    let present = EditTimeline.cellPosition(plan, cell: i, at: t) != nil
                    ZStack(alignment: .topLeading) {
                        Rectangle().fill(Color.clear).contentShape(Rectangle())
                        EditLabel(name: browser.cameraName(source.camera), time: nil, audible: audible == cell.source)
                        if !present {
                            Text("No recording at this time").font(.callout).foregroundStyle(.white.opacity(0.8))
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                        }
                    }
                    .frame(width: rects[i].width, height: rects[i].height)
                    .offset(x: rects[i].minX, y: rects[i].minY)
                    .onTapGesture { player.gridSound = i }
                    .help("Click to hear this camera")
                }
            }
        }
    }
}

struct EditLabel: View {
    let name: String
    let time: String?
    let audible: Bool

    var body: some View {
        HStack(spacing: 4) {
            Text(name).font(.caption.weight(.medium)).lineLimit(1)
            if let time { Text(time).font(.caption.monospacedDigit()) }
            if audible { Image(systemName: "speaker.wave.2.fill").font(.caption2).help("Its sound plays") }
        }
        .foregroundStyle(.white)
        .padding(.horizontal, 6).padding(.vertical, 3)
        .background(Color.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 4))
        .padding(6)
    }
}
