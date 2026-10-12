import AppKit
import NVRCore
import SwiftUI
import VideoKit

/// Draws the player's cameras with the app's Metal grid renderer, the one
/// live view uses: each cell shows its `CoreTile`'s newest decoded picture
/// (VideoToolbox), fitted to the cell with the picture's own geometry and
/// color (`PictureMapping`, `TilePlacement`), so recorded and live pictures
/// look the same. The SwiftUI layer above handles clicks and labels.
@MainActor
final class PlayerStageView: NSView {
    struct Item: Equatable {
        let tile: CoreTile
        /// The cell in points, top-left origin.
        var rect: CGRect
        static func == (a: Item, b: Item) -> Bool { a.tile === b.tile && a.rect == b.rect }
    }

    let grid: MetalGridView?
    var items: [Item] = [] {
        didSet { if items != oldValue { lastKey = nil } }
    }

    private struct RenderKey: Equatable {
        var tiles: [ObjectIdentifier]
        var rects: [CGRect]
        var versions: [UInt64]
        var sizes: [CGSize]
    }

    private var lastKey: RenderKey?

    override init(frame: NSRect) {
        // A view without Metal (a machine without a GPU) stays black.
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

    /// Frames drawn so far (the self-test checks pictures reach the screen).
    var framesDrawn: Int { grid?.framesDrawn ?? 0 }

    private func prepare(renderer: GridRenderer, drawable: CGSize) {
        let points = bounds
        guard points.width > 0, points.height > 0 else { return }
        let scaleX = drawable.width / points.width, scaleY = drawable.height / points.height
        var key = RenderKey(tiles: [], rects: [], versions: [], sizes: [])
        var tiles: [GridTile] = []
        for item in items {
            let snapshot = item.tile.snapshot
            let picture = item.tile.latestPicture()
            let size = picture.map { PixelSize(width: UInt32($0.width), height: UInt32($0.height)) }
            let mapping = PictureMapping(snapshot: snapshot, pictureSize: size)
            let pictureSize = CGSize(width: Int(mapping.layoutSize.width), height: Int(mapping.layoutSize.height))
            let matrix = snapshot.color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            let placed = TilePlacement.place(
                rect: item.rect, scaleX: scaleX, scaleY: scaleY, picture: pictureSize,
                offset: CGPoint(x: CGFloat(mapping.offset.x), y: CGFloat(mapping.offset.y)), fill: false)
            tiles.append(GridTile(picture: item.tile, color: matrix, destination: placed.destination, source: placed.source))
            key.tiles.append(ObjectIdentifier(item.tile))
            key.rects.append(item.rect.pixels(scaleX: scaleX, scaleY: scaleY))
            key.versions.append(snapshot.version)
            key.sizes.append(pictureSize)
        }
        if key != lastKey || renderer.tiles.count != tiles.count {
            lastKey = key
            renderer.tiles = tiles
        }
    }
}

/// The player's pictures in SwiftUI.
struct PlayerStage: NSViewRepresentable {
    let items: [PlayerStageView.Item]
    /// Hands the view to the window (the self-test reads it).
    var onView: (PlayerStageView) -> Void = { _ in }

    func makeNSView(context: Context) -> PlayerStageView {
        let v = PlayerStageView(frame: .zero)
        onView(v)
        return v
    }

    func updateNSView(_ view: PlayerStageView, context: Context) {
        view.items = items
    }
}
