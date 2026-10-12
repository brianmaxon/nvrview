import CoreGraphics
import Foundation

/// A viewer window as the app reopens it on the next launch: what it showed
/// and where it was. Kept in the app's own state file (never in the core's
/// store) and holding nothing secret: layout and group ids, an unsaved
/// arrangement's tree, and a frame.
public struct SavedWindow: Codable, Equatable, Sendable {
    public enum Content: Codable, Equatable, Sendable {
        case layout(id: String)
        case group(id: String)
        case scratch(name: String, root: LayoutNode)
    }

    public var content: Content
    /// The window's frame in screen coordinates (points, origin bottom left).
    public var frame: CGRect
    public var fullScreen: Bool
    /// The user's choice for the sidebar (not the transient full-screen
    /// state). Files written before this field existed decode as open.
    public var sidebarCollapsed: Bool
    /// Where the user moved the window's alert insets (docs/UX.md "Alert
    /// insets"); nil: the default corner. Left out of the file while nil;
    /// files without it (or with a value that cannot be read) load as nil.
    public var insetAnchor: InsetAnchor?
    /// The window's Fill Tiles switch (docs/UX.md "Fill Tiles"). Left out
    /// while off.
    public var fill: Bool
    /// Each camera's zoom or crop position and own Fill choice in this
    /// window (`TileViews`), the one changed longest ago first. Left out
    /// while empty; entries that cannot be read are skipped, values are
    /// clamped, and at most `TileViews.limit` are kept.
    public var tileViews: [SavedTileView]

    public init(
        content: Content, frame: CGRect, fullScreen: Bool = false, sidebarCollapsed: Bool = false,
        insetAnchor: InsetAnchor? = nil, fill: Bool = false, tileViews: [SavedTileView] = []
    ) {
        self.content = content
        self.frame = frame
        self.fullScreen = fullScreen
        self.sidebarCollapsed = sidebarCollapsed
        self.insetAnchor = insetAnchor
        self.fill = fill
        self.tileViews = tileViews
    }

    private enum CodingKeys: String, CodingKey {
        case content, frame, fullScreen, sidebarCollapsed, insetAnchor, fill, tileViews
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(content, forKey: .content)
        try c.encode(frame, forKey: .frame)
        try c.encode(fullScreen, forKey: .fullScreen)
        try c.encode(sidebarCollapsed, forKey: .sidebarCollapsed)
        try c.encodeIfPresent(insetAnchor, forKey: .insetAnchor)
        if fill { try c.encode(true, forKey: .fill) }
        if !tileViews.isEmpty { try c.encode(tileViews, forKey: .tileViews) }
    }

    /// The window's picture settings as saved (clamped and bounded).
    public var views: TileViews<CameraRef> { TileViews(fill: fill, savedEntries: tileViews) }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        content = try c.decode(Content.self, forKey: .content)
        frame = try c.decode(CGRect.self, forKey: .frame)
        fullScreen = try c.decode(Bool.self, forKey: .fullScreen)
        sidebarCollapsed = try c.decodeIfPresent(Bool.self, forKey: .sidebarCollapsed) ?? false
        insetAnchor = (try? c.decodeIfPresent(InsetAnchor.self, forKey: .insetAnchor)) ?? nil
        fill = ((try? c.decodeIfPresent(Bool.self, forKey: .fill)) ?? nil) ?? false
        let entries = ((try? c.decodeIfPresent([Lossy<SavedTileView>].self, forKey: .tileViews)) ?? nil) ?? []
        // Read through the model: clamped, without empty entries or
        // repeats, and bounded.
        tileViews = TileViews(fill: fill, savedEntries: entries.compactMap(\.value)).savedEntries
    }

    /// Where to put a restored window: its saved frame if enough of it is on
    /// one of `screens` (visible frames) to grab its title bar, else the
    /// same size (shrunk to fit) centered on the first screen; nil without
    /// screens. Never smaller than `minSize`.
    public static func placement(of frame: CGRect, screens: [CGRect], minSize: CGSize) -> CGRect? {
        guard let main = screens.first else { return nil }
        var f = frame.standardized
        f.size.width = max(f.width, minSize.width)
        f.size.height = max(f.height, minSize.height)
        // The title bar: the top 28 points, of which at least 80 points
        // wide must be on a screen.
        let titleBar = CGRect(x: f.minX, y: f.maxY - 28, width: f.width, height: 28)
        if f.width.isFinite, f.height.isFinite,
            screens.contains(where: { s in
                let i = s.intersection(titleBar)
                return !i.isNull && i.width >= 80 && i.height >= 10
            })
        {
            return f
        }
        let w = min(f.width, main.width), h = min(f.height, main.height)
        return CGRect(x: (main.midX - w / 2).rounded(), y: (main.midY - h / 2).rounded(), width: w, height: h)
    }
}
