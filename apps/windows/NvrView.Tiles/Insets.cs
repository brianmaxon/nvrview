// Where a window's call cards sit over its stage (docs/UX.md "Alert insets",
// "Doorbell calls"): the rules of the macOS insets (apps/macos NVRCore
// AlertInsets.swift, InsetGeometry), with the same test vectors. A card is a
// header, the doorbell's live picture (one more tile of the window's own
// grid, drawn last) and the call's controls; when the doorbell fills the
// stage its controls show over the stage instead. Pure: the app measures the
// cards and draws the pictures.

namespace NvrView.Tiles;

/// <summary>One inset's frame on the stage, top-left origin, in DIPs.</summary>
public readonly record struct InsetFrame(double X, double Y, double Width, double Height)
{
    public double Right => X + Width;

    public double Bottom => Y + Height;

    public double MidX => X + Width / 2;

    public bool Intersects(InsetFrame o) => X < o.Right && o.X < Right && Y < o.Bottom && o.Y < Bottom;
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
    public static double Width(double stage) => Math.Min(Math.Max(stage * WidthFraction, MinWidth), Math.Max(stage - 2 * Margin, 0));

    /// <summary>The height of a picture shown <paramref name="width"/> wide: its own aspect, kept between 0.3 and 1.2, 16:9 while unknown.</summary>
    public static double PictureHeight(double width, int pictureWidth, int pictureHeight)
    {
        double aspect = pictureWidth > 0 && pictureHeight > 0 ? pictureHeight / (double)pictureWidth : DefaultAspect;
        return width * Math.Clamp(aspect, 0.3, 1.2);
    }

    /// <summary>
    /// The frames of the first <paramref name="count"/> insets of the given heights (header,
    /// picture and any call controls), newest first, in a stage of the given size. Insets that do
    /// not fit are left out (the caller counts them in "+N more"); the first always gets a frame.
    /// </summary>
    public static IReadOnlyList<InsetFrame> Frames(int count, double width, IReadOnlyList<double> heights, ChromeCorner corner, double stageWidth, double stageHeight)
    {
        var frames = new List<InsetFrame>();
        double x = corner.IsLeft() ? Margin : stageWidth - Margin - width;
        double used = Margin;
        foreach (double h in heights.Take(count))
        {
            if (used + h + Margin > stageHeight && frames.Count > 0)
            {
                break;
            }
            double y = corner.IsTop() ? used : stageHeight - used - h;
            frames.Add(new InsetFrame(x, y, width, h));
            used += h + Gap;
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
