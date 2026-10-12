import CoreGraphics

/// A whole-pixel rectangle of a decoded buffer, origin at the top left.
public struct PixelRegion: Equatable, Sendable {
    public var x: Int
    public var y: Int
    public var width: Int
    public var height: Int

    public init(x: Int, y: Int, width: Int, height: Int) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public var rect: CGRect { CGRect(x: x, y: y, width: width, height: height) }
    public var size: CGSize { CGSize(width: width, height: height) }
}

/// What part of a decoded picture a saved picture holds (docs/CAPTURE.md
/// "Save picture"): the whole visible picture, or the region a zoomed (or
/// cropped) tile shows now, at the source's resolution and never enlarged.
/// Pure.
public enum PictureRegion {
    /// The whole picture: `layoutSize` pixels at `offset` in the buffer (a
    /// buffer at its coded size carries padding outside the visible rectangle).
    public static func whole(layoutSize: CGSize, offset: CGPoint) -> PixelRegion {
        PixelRegion(
            x: Int(offset.x.rounded()), y: Int(offset.y.rounded()), width: max(Int(layoutSize.width), 0),
            height: max(Int(layoutSize.height), 0))
    }

    /// The region of the picture a tile `tile` points big shows at `zoom`
    /// (and `fill`), in buffer pixels: the rectangle the renderer samples,
    /// rounded to whole pixels and kept inside the picture. At 1x without a
    /// crop it is the whole picture. Nil when there is no picture.
    public static func visible(
        layoutSize: CGSize, offset: CGPoint, tile: CGSize, fill: Bool, zoom: TileZoom
    ) -> PixelRegion? {
        guard layoutSize.width >= 1, layoutSize.height >= 1, tile.width > 0, tile.height > 0 else { return nil }
        // The placement at one pixel per point is all that matters: the
        // source rectangle depends on the shapes and the zoom, not the scale.
        let placed = TilePlacement.place(
            rect: CGRect(origin: .zero, size: tile), scaleX: 1, scaleY: 1, picture: layoutSize, offset: offset,
            fill: fill, zoom: zoom)
        let whole = whole(layoutSize: layoutSize, offset: offset)
        guard let source = placed.source else { return whole }
        let minX = max(Int(source.minX.rounded()), whole.x), minY = max(Int(source.minY.rounded()), whole.y)
        let maxX = min(Int(source.maxX.rounded()), whole.x + whole.width)
        let maxY = min(Int(source.maxY.rounded()), whole.y + whole.height)
        guard maxX > minX, maxY > minY else { return whole }
        return PixelRegion(x: minX, y: minY, width: maxX - minX, height: maxY - minY)
    }
}
