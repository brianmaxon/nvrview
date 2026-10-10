import CoreGraphics
import VideoKit

/// Where a tile's picture sits relative to the visible rectangle the core's
/// layout speaks in.
///
/// The core's `source` rectangles are in pixels of the VISIBLE picture
/// (`visible_rect` inside `coded_size`). VideoToolbox normally hands back a
/// buffer of just the visible size (the format description already carries the
/// SPS cropping), in which case source rectangles apply as they are. If a
/// buffer ever arrives at the full coded size instead, the visible rectangle's
/// origin is added. A buffer of any other size (a late picture from before a
/// resize) is laid out at its own size, uncropped.
public struct PictureMapping: Equatable, Sendable {
    /// The size to give the layout for this tile (zero: no picture).
    public var layoutSize: PixelSize
    /// Added to the layout's source rectangle to get pixels of the buffer.
    public var offset: (x: UInt32, y: UInt32)

    public static func == (a: PictureMapping, b: PictureMapping) -> Bool {
        a.layoutSize == b.layoutSize && a.offset == b.offset
    }

    public init(layoutSize: PixelSize, x: UInt32, y: UInt32) {
        self.layoutSize = layoutSize
        offset = (x, y)
    }

    public init(snapshot: TileSnapshot, pictureSize: PixelSize?) {
        let visible = snapshot.visibleRect.size
        guard let picture = pictureSize, picture != .zero else {
            layoutSize = .zero
            offset = (0, 0)
            return
        }
        if picture == visible || snapshot.color == nil {
            layoutSize = picture
            offset = (0, 0)
        } else if picture == snapshot.codedSize {
            layoutSize = visible
            offset = (snapshot.visibleRect.x, snapshot.visibleRect.y)
        } else {
            layoutSize = picture
            offset = (0, 0)
        }
    }
}

/// Turns the core's grid layout and per-tile configuration into the
/// `GridTile`s a `GridRenderer` draws. Use from one thread (the one that
/// drives the renderer).
public final class CoreGrid {
    private let bridge: CoreBridge
    public var gap: UInt32
    public var fill: Bool

    private struct Key: Equatable {
        var viewport: PixelSize
        var tiles: [ObjectIdentifier]
        var versions: [UInt64]
        var sizes: [PixelSize]
    }
    private var last: Key?

    public init(bridge: CoreBridge, gap: UInt32 = 4, fill: Bool = false) {
        self.bridge = bridge
        self.gap = gap
        self.fill = fill
    }

    /// Recomputes the renderer's tiles only if the viewport, a tile's
    /// configuration, or a picture size changed since the last call (assigning
    /// `renderer.tiles` makes the renderer re-layout). Returns whether it did.
    @discardableResult
    public func update(renderer: GridRenderer, tiles: [CoreTile], viewport: CGSize) -> Bool {
        let view = PixelSize(width: UInt32(max(viewport.width, 0)), height: UInt32(max(viewport.height, 0)))
        var snapshots: [TileSnapshot] = []
        var pictures: [PixelSize] = []
        for tile in tiles {
            snapshots.append(tile.snapshot)
            let p = tile.latestPicture()
            pictures.append(p.map { PixelSize(width: UInt32($0.width), height: UInt32($0.height)) } ?? .zero)
        }
        let key = Key(
            viewport: view, tiles: tiles.map(ObjectIdentifier.init), versions: snapshots.map(\.version),
            sizes: pictures)
        if key == last, renderer.tiles.count == tiles.count { return false }
        // Do not remember a layout of tiles that have no picture yet being
        // final: the picture size is part of the key, so it re-lays out when
        // the first picture arrives.
        last = key
        renderer.tiles = Self.gridTiles(
            bridge: bridge, tiles: tiles, snapshots: snapshots, pictures: pictures, viewport: view,
            gap: gap, fill: fill)
        return true
    }

    /// The pure part, for tests.
    public static func gridTiles(
        bridge: CoreBridge, tiles: [CoreTile], snapshots: [TileSnapshot], pictures: [PixelSize],
        viewport: PixelSize, gap: UInt32, fill: Bool
    ) -> [GridTile] {
        let mappings = zip(snapshots, pictures).map { PictureMapping(snapshot: $0, pictureSize: $1) }
        let layouts = bridge.gridLayout(
            viewport: viewport, gap: gap, pictureSizes: mappings.map(\.layoutSize), fill: fill)
        guard layouts.count == tiles.count else { return [] }
        return tiles.indices.map { i in
            let layout = layouts[i]
            let mapping = mappings[i]
            let matrix = snapshots[i].color.flatMap { YCbCrToRGB(rowMajor: $0.ycbcrToRGB) }
                ?? YCbCrToRGB(r: [1, 0, 0, 0], g: [0, 1, 0, 0], b: [0, 0, 1, 0])
            let source: CGRect? =
                mapping.layoutSize == .zero
                ? nil
                : CGRect(
                    x: Int(layout.source.x) + Int(mapping.offset.x),
                    y: Int(layout.source.y) + Int(mapping.offset.y),
                    width: Int(layout.source.width), height: Int(layout.source.height))
            return GridTile(
                picture: tiles[i], color: matrix,
                destination: CGRect(
                    x: Int(layout.dest.x), y: Int(layout.dest.y), width: Int(layout.dest.width),
                    height: Int(layout.dest.height)),
                source: source)
        }
    }
}
