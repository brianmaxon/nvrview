// Where a window's insets sit over its stage (docs/UX.md "Alert insets", "Doorbell calls"): the
// rules of the macOS insets (apps/macos NVRCore AlertInsets.swift, InsetGeometry), with the same
// test vectors. An inset is a header, a camera's live picture (one more tile of the window's own
// grid, drawn last) and, for a call, the call's controls; when the doorbell fills the stage its
// controls show over the stage instead. The order and the swap are AlertInsets.cs. Pure: the app
// measures the cards and draws the pictures.

using System.Text.Json.Nodes;

namespace NvrView.Tiles;

/// <summary>One inset's frame on the stage, top-left origin, in DIPs.</summary>
public readonly record struct InsetFrame(double X, double Y, double Width, double Height)
{
    public double Right => X + Width;

    public double Bottom => Y + Height;

    public double MidX => X + Width / 2;

    public bool Intersects(InsetFrame o) => X < o.Right && o.X < Right && Y < o.Bottom && o.Y < Bottom;
}

/// <summary>
/// Where a window's inset column sits: its position as a fraction of the free space inside the
/// stage's margins, each in 0..1. (0, 0) is the top left corner and (1, 1) the bottom right.
/// Resizing the window or going full screen keeps the fractions, so a column in a corner stays in it.
/// </summary>
public readonly record struct InsetAnchor
{
    /// <summary>Clamped to 0..1 (anything not finite counts as 0).</summary>
    public InsetAnchor(double x, double y)
    {
        X = double.IsFinite(x) ? Math.Min(Math.Max(x, 0), 1) : 0;
        Y = double.IsFinite(y) ? Math.Min(Math.Max(y, 0), 1) : 0;
    }

    public double X { get; }

    public double Y { get; }

    /// <summary>A corner's anchor (the default placement).</summary>
    public static InsetAnchor FromCorner(ChromeCorner corner) => corner switch
    {
        ChromeCorner.TopLeft => new(0, 0),
        ChromeCorner.TopRight => new(1, 0),
        ChromeCorner.BottomLeft => new(0, 1),
        _ => new(1, 1),
    };

    /// <summary>The column grows upward from its bottom edge (it sits nearer the bottom); at the exact middle it grows downward.</summary>
    public bool GrowsUpward => Y > 0.5;

    /// <summary>The saved form: <c>{"x":..,"y":..}</c>.</summary>
    public string ToJson() => new JsonObject { ["x"] = X, ["y"] = Y }.ToJsonString();

    /// <summary>A saved value, through the clamping constructor; null when it is not an object with numeric x and y.</summary>
    public static InsetAnchor? FromJson(string? json)
    {
        try
        {
            if (json is not null && JsonNode.Parse(json) is JsonObject o && o["x"] is JsonValue x && o["y"] is JsonValue y
                && x.TryGetValue(out double dx) && y.TryGetValue(out double dy))
            {
                return new InsetAnchor(dx, dy);
            }
        }
        catch (System.Text.Json.JsonException)
        {
        }
        return null;
    }
}

/// <summary>Dragging the inset column: where it lands as an anchor, each axis snapping to its edge within <see cref="SnapDistance"/>.</summary>
public static class InsetPlacement
{
    /// <summary>Distance from an edge (inside the margins) within which an axis snaps to that edge.</summary>
    public const double SnapDistance = 24;

    /// <summary>How far a press on an inset's header moves before it is a drag.</summary>
    public const double HeaderSlop = 2;

    /// <summary>How far a press on an inset's picture moves before it is a drag (a shorter move is a click).</summary>
    public const double PictureSlop = 6;

    /// <summary>Whether a press that has moved (<paramref name="dx"/>, <paramref name="dy"/>) has become a drag.</summary>
    public static bool IsDrag(double dx, double dy, bool onHeader) =>
        Math.Sqrt(dx * dx + dy * dy) >= (onHeader ? HeaderSlop : PictureSlop);

    /// <summary>The anchor for a column whose top left corner was dragged to (<paramref name="x"/>, <paramref name="y"/>) in a stage (top-left origin).</summary>
    public static InsetAnchor Anchor(double x, double y, double columnWidth, double columnHeight, double stageWidth, double stageHeight) =>
        new(Fraction(x, columnWidth, stageWidth), Fraction(y, columnHeight, stageHeight));

    /// <summary>
    /// One axis: the offset from the near margin over the free space; 0 or 1 within the snap
    /// distance of either end (the nearer end when both are, the start on a tie); 0 when there is
    /// no free space.
    /// </summary>
    public static double Fraction(double position, double size, double stage)
    {
        double free = stage - 2 * InsetGeometry.Margin - size;
        if (free <= 0)
        {
            return 0;
        }
        double fromStart = position - InsetGeometry.Margin;
        double fromEnd = free - fromStart;
        if (Math.Min(fromStart, fromEnd) <= SnapDistance)
        {
            return fromStart <= fromEnd ? 0 : 1;
        }
        return fromStart / free;
    }

    /// <summary>The column's top left corner for an anchor (the inverse of <see cref="Anchor"/> away from the edges).</summary>
    public static (double X, double Y) Origin(InsetAnchor anchor, double columnWidth, double columnHeight, double stageWidth, double stageHeight)
    {
        double m = InsetGeometry.Margin;
        return (m + anchor.X * (stageWidth - 2 * m - columnWidth), m + anchor.Y * (stageHeight - 2 * m - columnHeight));
    }
}

/// <summary>The insets' geometry on a stage: about 30% of its width, stacked from a corner, newest nearest it.</summary>
public static class InsetGeometry
{
    public const double WidthFraction = 0.3;
    public const double MinWidth = 220;
    public const double Margin = 12;
    public const double Gap = 8;
    public const double HeaderHeight = 22;

    /// <summary>The picture's height for its width when the picture's size is not known yet (16:9).</summary>
    public const double DefaultAspect = 0.5625;

    /// <summary>The widest the call controls over the stage get.</summary>
    public const double StagePanelMaxWidth = 480;

    /// <summary>The corner the insets stack from: the user's choice (Settings), else opposite the corner indicators.</summary>
    public static ChromeCorner Corner(ChromeCorner? choice, ChromeCorner indicators) => choice ?? indicators.Opposite();

    /// <summary>The inset width for a stage width.</summary>
    public static double Width(double stage) => Width(stage, 1);

    /// <summary>How many columns of insets fit side by side (at least one, at most <paramref name="columns"/>), each at least <paramref name="minWidth"/> wide (a card with call controls needs more than a plain inset).</summary>
    public static int FittingColumns(double stage, int columns, double minWidth = MinWidth)
    {
        int fit = (int)Math.Floor((stage - 2 * Margin + Gap) / (minWidth + Gap));
        return Math.Max(1, Math.Min(columns, fit));
    }

    /// <summary>
    /// The inset width for a grid of up to <paramref name="columns"/> columns: about 30% of the
    /// stage's width, narrower when the columns need the room, never under <see cref="MinWidth"/>
    /// (columns that do not fit are dropped instead; see <see cref="FittingColumns"/>). One column
    /// is the original single stack.
    /// </summary>
    public static double Width(double stage, int columns, double minWidth = MinWidth)
    {
        int used = FittingColumns(stage, columns, minWidth);
        double available = Math.Max((stage - 2 * Margin - Gap * (used - 1)) / used, 0);
        return Math.Min(Math.Max(Math.Min(stage * WidthFraction, available), minWidth), available);
    }

    /// <summary>The height of a picture shown <paramref name="width"/> wide: its own aspect, kept between 0.3 and 1.2, 16:9 while unknown.</summary>
    public static double PictureHeight(double width, int pictureWidth, int pictureHeight)
    {
        double aspect = pictureWidth > 0 && pictureHeight > 0 ? pictureHeight / (double)pictureWidth : DefaultAspect;
        return width * Math.Clamp(aspect, 0.3, 1.2);
    }

    /// <summary>
    /// The frames of the first <paramref name="count"/> insets of the given heights (header,
    /// picture and any call controls), newest first, in a stage of the given size, stacked from
    /// <paramref name="corner"/>. Insets that do not fit are left out (the caller counts them in
    /// "+N more"); the first always gets a frame.
    /// </summary>
    public static IReadOnlyList<InsetFrame> Frames(int count, double width, IReadOnlyList<double> heights, ChromeCorner corner, double stageWidth, double stageHeight) =>
        Frames(count, width, heights, InsetAnchor.FromCorner(corner), stageWidth, stageHeight);

    /// <summary>
    /// The same for a column placed anywhere: <paramref name="anchor"/> is the column's position
    /// as a fraction of the free space inside the margins. Below the middle (<c>Y &gt; 0.5</c>)
    /// the newest inset is at the column's bottom and the others stack upward; otherwise the
    /// newest is at its top. Insets that do not fit the stage's height (inside the margins) are
    /// left out; the newest is always drawn, even when it alone is taller.
    /// </summary>
    public static IReadOnlyList<InsetFrame> Frames(int count, double width, IReadOnlyList<double> heights, InsetAnchor anchor, double stageWidth, double stageHeight) =>
        Grid([.. heights.Take(count)], width, Math.Max(count, 1), 1, anchor, stageWidth, stageHeight);

    /// <summary>
    /// The frames of insets of the given heights in a grid of up to <paramref name="rows"/> insets
    /// per column and <paramref name="columns"/> columns: column by column, the first column at the
    /// anchor's horizontal edge (the right edge when the anchor is right of the middle, so the grid
    /// grows leftward). In each column the order is as in <see cref="Frames(int, double, IReadOnlyList{double}, InsetAnchor, double, double)"/>.
    /// Returns the frames of the insets that fit, a prefix of <paramref name="heights"/>: an inset
    /// that does not fit the stage's height starts the next column, and one that finds no column
    /// left ends the grid (the caller counts the rest in "+N more"). The first inset is always
    /// drawn, even when it alone is taller than the stage.
    /// </summary>
    public static IReadOnlyList<InsetFrame> Grid(IReadOnlyList<double> heights, double width, int rows, int columns, InsetAnchor anchor, double stageWidth, double stageHeight)
    {
        int maxColumns = FittingColumns(stageWidth, columns);
        var perColumn = new List<List<double>>();
        double used = Margin;
        foreach (double h in heights)
        {
            if (perColumn.Count > 0 && perColumn[^1].Count < Math.Max(rows, 1) && used + h + Margin <= stageHeight)
            {
                perColumn[^1].Add(h);
                used += h + Gap;
            }
            else if (perColumn.Count < maxColumns)
            {
                perColumn.Add([h]);
                used = Margin + h + Gap;
            }
            else
            {
                break;
            }
        }
        var frames = new List<InsetFrame>();
        if (perColumn.Count == 0)
        {
            return frames;
        }
        static double Height(List<double> column) => column.Sum() + Gap * (column.Count - 1);
        double tallest = perColumn.Max(Height);
        double gridWidth = perColumn.Count * width + Gap * (perColumn.Count - 1);
        double left = Margin + anchor.X * (stageWidth - 2 * Margin - gridWidth);
        double top = Margin + anchor.Y * (stageHeight - 2 * Margin - tallest);
        bool upward = anchor.GrowsUpward;
        bool fromRight = anchor.X > 0.5;
        for (int j = 0; j < perColumn.Count; j++)
        {
            double step = j * (width + Gap);
            double x = fromRight ? left + gridWidth - width - step : left + step;
            double offset = 0;
            foreach (double h in perColumn[j])
            {
                double y = upward ? top + tallest - offset - h : top + offset;
                frames.Add(new InsetFrame(x, y, width, h));
                offset += h + Gap;
            }
        }
        return frames;
    }

    /// <summary>
    /// Where the call controls over the stage go (the doorbell fills it): centered along the
    /// bottom, at most <see cref="StagePanelMaxWidth"/> wide, beside the insets' column
    /// (<paramref name="avoid"/>) on the side with room when they would overlap it.
    /// </summary>
    public static InsetFrame StagePanel(double stageWidth, double stageHeight, double panelHeight, InsetFrame? avoid)
    {
        double width = Math.Max(Math.Min(stageWidth - 2 * Margin, StagePanelMaxWidth), 0);
        double x = (stageWidth - width) / 2;
        double y = stageHeight - panelHeight - Margin;
        if (avoid is { } a && a.Intersects(new InsetFrame(x, y, width, panelHeight)))
        {
            x = a.MidX < stageWidth / 2 ? Math.Max(a.Right + Margin, x) : Math.Min(a.X - Margin - width, x);
            x = Math.Min(Math.Max(x, Margin), Math.Max(stageWidth - Margin - width, Margin));
        }
        return new InsetFrame(x, y, width, panelHeight);
    }

    /// <summary>The smallest frame holding all of <paramref name="frames"/>, null for none.</summary>
    public static InsetFrame? Union(IEnumerable<InsetFrame> frames)
    {
        InsetFrame? u = null;
        foreach (InsetFrame f in frames)
        {
            u = u is not { } a ? f : new InsetFrame(
                Math.Min(a.X, f.X), Math.Min(a.Y, f.Y),
                Math.Max(a.Right, f.Right) - Math.Min(a.X, f.X), Math.Max(a.Bottom, f.Bottom) - Math.Min(a.Y, f.Y));
        }
        return u;
    }
}

/// <summary>Which calls a window shows as cards and which over its stage.</summary>
public static class CallInsets
{
    /// <summary>
    /// The camera that fills the stage, if one does: the focused tile's, else the only tile's
    /// (<paramref name="leaves"/>: the layout's cameras, null for an empty tile).
    /// </summary>
    public static CameraRef? Filling(CameraRef? focused, IReadOnlyList<CameraRef?> leaves) =>
        focused ?? (leaves.Count == 1 ? leaves[0] : null);

    /// <summary>
    /// The call whose controls show over the stage instead of a card: the one of the camera that
    /// fills it (any lens of its channel), null when that camera has no call.
    /// </summary>
    public static CameraRef? OverStage(IEnumerable<CameraRef> calls, CameraRef? filling) =>
        filling is { } f && calls.Contains(f.ChannelKey) ? f.ChannelKey : null;
}
