// The tile's hover header and its menus (docs/UX.md "Tile header", docs/CAPTURE.md "The header
// model"): which buttons fit a tile of a given width, in which order, what moves into the "…" menu,
// and the right-click menu built from the same list. Pure, with no view code, so both apps lay out
// headers the same way. The vectors in fixtures/synthetic/tile-header.json are shared with the Mac
// app's NVRCore (TileHeader.swift).

namespace NvrView.Tiles;

/// <summary>Everything a header or its menus can hold. <see cref="TileHeaderItems.Id"/> gives the stable identifier the shared vectors use.</summary>
public enum TileHeaderItem
{
    /// <summary>"● REC 1:23": an indicator, never in the overflow.</summary>
    RecordingPill,
    /// <summary>The camera's name: truncated before it is dropped.</summary>
    Title,
    /// <summary>An active alert's Dismiss: an indicator, never in the overflow.</summary>
    DismissAlert,
    Speaker,
    Record,
    SavePicture,
    Conversation,
    PanTilt,
    Presets,
    Lens,
    ResetZoom,
    Quality,
    Fisheye,
    /// <summary>The "…" button.</summary>
    More,
    // The long tail: menus only.
    Pop,
    AllowTalkBack,
    Entrance,
    CallsOpenIn,
    SavePictureAs,
    SaveZoomedArea,
    SaveOriginalFisheye,
    RecordMain,
    RecordSub,
    /// <summary>Menus only: brings a filled picture's moved crop back to the middle.</summary>
    ResetPosition,
    /// <summary>The tile's own Fill choice (a checked item).</summary>
    FillTile,
}

public static class TileHeaderItems
{
    private static readonly string[] Ids =
    [
        "recording-pill", "title", "dismiss-alert", "speaker", "record", "save-picture", "conversation",
        "pan-tilt", "presets", "lens", "reset-zoom", "quality", "fisheye", "more", "pop", "allow-talk-back",
        "entrance", "calls-open-in", "save-picture-as", "save-zoomed-area", "save-original-fisheye",
        "record-main", "record-sub", "reset-position", "fill-tile",
    ];

    /// <summary>The item's stable identifier ("save-picture").</summary>
    public static string Id(this TileHeaderItem item) => Ids[(int)item];

    /// <summary>The item with that identifier, or null.</summary>
    public static TileHeaderItem? FromId(string id)
    {
        int i = Array.IndexOf(Ids, id);
        return i < 0 ? null : (TileHeaderItem)i;
    }
}

/// <summary>Where the camera's doorbell call is, if one is open.</summary>
public enum TileHeaderCall
{
    /// <summary>No call for this camera.</summary>
    None,
    /// <summary>A call shown in the viewer windows' insets: "Pop Out Call".</summary>
    Inset,
    /// <summary>This tile is the call window's own tile: "Pop In Call", and Record is the call recording.</summary>
    Window,
}

/// <summary>What a tile's header depends on: the camera's capabilities and the tile's state.</summary>
public sealed record TileHeaderTile
{
    /// <summary>The camera has audio (a Speaker button).</summary>
    public bool Audio { get; init; }
    /// <summary><c>ipcAudioTalk</c> 1: talk-back, so Start Conversation is in the header.</summary>
    public bool Talk { get; init; }
    /// <summary>A doorbell: Start Conversation in the header, and "Calls open in".</summary>
    public bool Doorbell { get; init; }
    /// <summary><c>ptzType</c> 3.</summary>
    public bool PanTilt { get; init; }
    /// <summary><c>ptzPreset</c> bit 0.</summary>
    public bool Presets { get; init; }
    /// <summary><c>ptzType</c> 1 or 3, and the camera reported lens ranges.</summary>
    public bool Lens { get; init; }
    public bool Fisheye { get; init; }
    /// <summary>The tile shows a picture; false under a no-picture veil ("Connecting", "Reconnecting").</summary>
    public bool Picture { get; init; } = true;
    /// <summary>The camera is being recorded (the pill; Record becomes Stop).</summary>
    public bool Recording { get; init; }
    /// <summary>Zoomed past 1x, or a moved virtual camera.</summary>
    public bool Zoomed { get; init; }
    /// <summary>At 1x, a filled picture's crop moved off center.</summary>
    public bool Panned { get; init; }
    /// <summary>An active alert on the tile.</summary>
    public bool Alert { get; init; }
    /// <summary>The tile is audible (Speaker says Stop Listening).</summary>
    public bool Listening { get; init; }
    /// <summary>The camera's call is open (End Conversation).</summary>
    public bool Conversation { get; init; }
    public TileHeaderCall Call { get; init; } = TileHeaderCall.None;
    /// <summary>A recording player's tile (docs/UX.md "Recordings"): only what applies to a picture is offered (Save Picture,
    /// Reset Zoom, Fisheye View, Fill Tile, Reset Position, the Save items); no Record, Speaker, Quality, talk-back, PTZ or call items.</summary>
    public bool Playback { get; init; }
}

/// <summary>Widths in DIPs (points on macOS).</summary>
public sealed record TileHeaderMetrics
{
    /// <summary>An icon button, "…" included.</summary>
    public double Icon { get; init; } = 26;
    /// <summary>The title's least width before buttons give way.</summary>
    public double TitleMin { get; init; } = 48;
    /// <summary>The recording pill.</summary>
    public double Pill { get; init; } = 64;
    /// <summary>The header's inset at each end.</summary>
    public double Padding { get; init; } = 8;
    /// <summary>A width change smaller than this from the width that last changed the result keeps the result.</summary>
    public double Hysteresis { get; init; } = 8;
    /// <summary>Below this tile width only the pill, the title and "…" show.</summary>
    public double Narrow { get; init; } = 120;

    public static readonly TileHeaderMetrics Standard = new();
}

/// <summary>One menu item: the item, its English label and its key hints ("" when there is none).</summary>
public sealed record TileHeaderMenuEntry(TileHeaderItem Item, string Label, string MacShortcut = "", string WindowsShortcut = "");

/// <summary>The header of one tile at one width.</summary>
public sealed class TileHeaderLayout
{
    /// <summary>The width at which the visible items last changed (hysteresis).</summary>
    public double Anchor { get; }
    public TileHeaderTile Tile { get; }
    public double TitleWidth { get; }
    public TileHeaderMetrics Metrics { get; }
    /// <summary>The items shown, in display order: the pill, the title, Dismiss, the buttons by priority, then "…" when anything is hidden.</summary>
    public IReadOnlyList<TileHeaderItem> Visible { get; }
    /// <summary>The header buttons that did not fit, by priority.</summary>
    public IReadOnlyList<TileHeaderItem> Overflow { get; }
    /// <summary>The title has less room than its natural width.</summary>
    public bool TitleTruncated { get; }

    public bool ShowsMore => Visible.Contains(TileHeaderItem.More);

    /// <summary>The right-click menu: every action, in the same order as the header.</summary>
    public IReadOnlyList<TileHeaderMenuEntry> ContextMenu => Menu(Tile);

    /// <summary>The "…" menu: the right-click menu without what the header shows.</summary>
    public IReadOnlyList<TileHeaderMenuEntry> MoreMenu => [.. ContextMenu.Where(e => !Visible.Contains(e.Item))];

    private TileHeaderLayout(double anchor, double width, double titleWidth, TileHeaderTile tile, TileHeaderMetrics metrics,
        IReadOnlyList<TileHeaderItem> visible, IReadOnlyList<TileHeaderItem> overflow)
    {
        Anchor = anchor;
        TitleWidth = titleWidth;
        Tile = tile;
        Metrics = metrics;
        Visible = visible;
        Overflow = overflow;
        double others = visible.Sum(i => i switch
        {
            TileHeaderItem.Title => 0,
            TileHeaderItem.RecordingPill => metrics.Pill,
            _ => metrics.Icon,
        });
        TitleTruncated = Math.Max(0, width - 2 * metrics.Padding - others) < titleWidth;
    }

    /// <summary>Lays out the header for a tile <paramref name="width"/> wide whose title needs <paramref name="titleWidth"/>. Pass the previous result for the same tile so small width changes do not reflow the buttons.</summary>
    public static TileHeaderLayout Compute(double width, double titleWidth, TileHeaderTile tile, TileHeaderMetrics? metrics = null,
        TileHeaderLayout? previous = null)
    {
        metrics ??= TileHeaderMetrics.Standard;
        (List<TileHeaderItem> visible, List<TileHeaderItem> overflow) = Place(width, tile, metrics);
        if (previous is not null && previous.Tile == tile && previous.TitleWidth == titleWidth && previous.Metrics == metrics
            && (Math.Abs(width - previous.Anchor) < metrics.Hysteresis || visible.SequenceEqual(previous.Visible)))
        {
            // Same items as before: the anchor stays; only the title's room follows the width.
            return new TileHeaderLayout(previous.Anchor, width, titleWidth, tile, metrics, previous.Visible, previous.Overflow);
        }
        return new TileHeaderLayout(width, width, titleWidth, tile, metrics, visible, overflow);
    }

    /// <summary>What a recording player's tile offers: the picture's own actions and its Quality (Auto, High, Low).</summary>
    public static readonly IReadOnlySet<TileHeaderItem> PlaybackItems = new HashSet<TileHeaderItem>
    {
        TileHeaderItem.SavePicture, TileHeaderItem.ResetZoom, TileHeaderItem.ResetPosition, TileHeaderItem.Quality, TileHeaderItem.Fisheye,
        TileHeaderItem.FillTile, TileHeaderItem.SavePictureAs, TileHeaderItem.SaveZoomedArea, TileHeaderItem.SaveOriginalFisheye,
    };

    /// <summary>The full menu for a tile, in header order, then the long tail.</summary>
    public static IReadOnlyList<TileHeaderMenuEntry> Menu(TileHeaderTile tile)
    {
        IReadOnlyList<TileHeaderMenuEntry> all = FullMenu(tile);
        return tile.Playback ? all.Where(e => PlaybackItems.Contains(e.Item)).ToList() : all;
    }

    private static List<TileHeaderMenuEntry> FullMenu(TileHeaderTile tile)
    {
        bool inWindow = tile.Call == TileHeaderCall.Window;
        var m = new List<TileHeaderMenuEntry>();
        if (tile.Alert)
        {
            m.Add(new(TileHeaderItem.DismissAlert, "Dismiss Alert", "↩", "Enter"));
        }
        if (tile.Audio)
        {
            m.Add(new(TileHeaderItem.Speaker, tile.Listening ? "Stop Listening" : "Listen", "M", "M"));
        }
        m.Add(inWindow
            ? new(TileHeaderItem.Record, tile.Recording ? "Stop Recording Call" : "Record Call", "⇧⌘R", "Ctrl+Shift+R")
            : new(TileHeaderItem.Record, tile.Recording ? "Stop Recording" : "Start Recording", "R", "R"));
        if (tile.Picture)
        {
            m.Add(inWindow
                ? new(TileHeaderItem.SavePicture, "Save Picture", "⇧⌘P", "Ctrl+Shift+P")
                : new(TileHeaderItem.SavePicture, "Save Picture", "P", "P"));
        }
        m.Add(new(TileHeaderItem.Conversation, tile.Conversation ? "End Conversation" : "Start Conversation", "T", "T"));
        if (tile.PanTilt)
        {
            m.Add(new(TileHeaderItem.PanTilt, "Pan and Tilt…", "⌥ arrows", "Alt+arrows"));
        }
        if (tile.Presets)
        {
            m.Add(new(TileHeaderItem.Presets, "Presets", "⌥1–9", "Alt+1–9"));
        }
        if (tile.Lens)
        {
            m.Add(new(TileHeaderItem.Lens, "Zoom and Focus…", "⌥= ⌥-", "Alt+= Alt+-"));
        }
        if (tile.Zoomed)
        {
            m.Add(new(TileHeaderItem.ResetZoom, "Reset Zoom", "0", "0"));
        }
        m.Add(new(TileHeaderItem.Quality, "Quality"));
        if (tile.Fisheye)
        {
            m.Add(new(TileHeaderItem.Fisheye, "Fisheye View"));
        }
        if (tile.Panned && !tile.Zoomed)
        {
            m.Add(new(TileHeaderItem.ResetPosition, "Reset Position", "0", "0"));
        }
        if (!inWindow)
        {
            m.Add(new(TileHeaderItem.FillTile, "Fill Tile"));
        }
        if (tile.Call != TileHeaderCall.None)
        {
            m.Add(new(TileHeaderItem.Pop, inWindow ? "Pop In Call" : "Pop Out Call"));
        }
        m.Add(new(TileHeaderItem.AllowTalkBack, "Allow talk-back"));
        m.Add(new(TileHeaderItem.Entrance, "Entrance"));
        if (tile.Doorbell)
        {
            m.Add(new(TileHeaderItem.CallsOpenIn, "Calls open in"));
        }
        if (tile.Picture)
        {
            m.Add(new(TileHeaderItem.SavePictureAs, "Save Picture As…", "⇧P", "Shift+P"));
            if (tile.Zoomed)
            {
                m.Add(new(TileHeaderItem.SaveZoomedArea, "Save Zoomed Area"));
            }
            if (tile.Fisheye)
            {
                m.Add(new(TileHeaderItem.SaveOriginalFisheye, "Save Original Fisheye Picture"));
            }
        }
        if (!tile.Recording && !inWindow)
        {
            m.Add(new(TileHeaderItem.RecordMain, "Record Main Stream"));
            m.Add(new(TileHeaderItem.RecordSub, "Record Sub-stream"));
        }
        return m;
    }

    /// <summary>The header buttons that apply to a tile, by priority (the pill, the title and Dismiss excluded).</summary>
    public static IReadOnlyList<TileHeaderItem> Buttons(TileHeaderTile tile)
    {
        IReadOnlyList<TileHeaderItem> all = FullButtons(tile);
        return tile.Playback ? all.Where(PlaybackItems.Contains).ToList() : all;
    }

    private static List<TileHeaderItem> FullButtons(TileHeaderTile tile)
    {
        var b = new List<TileHeaderItem>();
        if (tile.Audio)
        {
            b.Add(TileHeaderItem.Speaker);
        }
        b.Add(TileHeaderItem.Record);
        if (tile.Picture)
        {
            b.Add(TileHeaderItem.SavePicture);
        }
        if (tile.Talk || tile.Doorbell)
        {
            b.Add(TileHeaderItem.Conversation);
        }
        if (tile.PanTilt)
        {
            b.Add(TileHeaderItem.PanTilt);
        }
        if (tile.Presets)
        {
            b.Add(TileHeaderItem.Presets);
        }
        if (tile.Lens)
        {
            b.Add(TileHeaderItem.Lens);
        }
        if (tile.Zoomed)
        {
            b.Add(TileHeaderItem.ResetZoom);
        }
        b.Add(TileHeaderItem.Quality);
        if (tile.Fisheye)
        {
            b.Add(TileHeaderItem.Fisheye);
        }
        return b;
    }

    private static (List<TileHeaderItem> Visible, List<TileHeaderItem> Overflow) Place(double width, TileHeaderTile tile,
        TileHeaderMetrics metrics)
    {
        List<TileHeaderItem> buttons = [.. Buttons(tile)];
        List<TileHeaderItem> titleAndMore = buttons.Count == 0 ? [TileHeaderItem.Title] : [TileHeaderItem.Title, TileHeaderItem.More];
        // Under a no-picture veil: the title and "…" only (Retry is in the veil).
        if (!tile.Picture)
        {
            return (titleAndMore, buttons);
        }
        List<TileHeaderItem> pill = tile.Recording ? [TileHeaderItem.RecordingPill] : [];
        if (width < metrics.Narrow)
        {
            return ([.. pill, .. titleAndMore], buttons);
        }
        List<TileHeaderItem> dismiss = tile.Alert ? [TileHeaderItem.DismissAlert] : [];
        double fixedWidth = (tile.Recording ? metrics.Pill : 0) + (tile.Alert ? metrics.Icon : 0) + metrics.TitleMin;
        double room = width - 2 * metrics.Padding - fixedWidth;
        if (buttons.Count * metrics.Icon <= room)
        {
            return ([.. pill, TileHeaderItem.Title, .. dismiss, .. buttons], []);
        }
        room -= metrics.Icon; // the "…" comes first once anything is hidden
        int shown = 0;
        while (shown < buttons.Count && room >= metrics.Icon)
        {
            room -= metrics.Icon;
            shown++;
        }
        return ([.. pill, TileHeaderItem.Title, .. dismiss, .. buttons.Take(shown), TileHeaderItem.More], [.. buttons.Skip(shown)]);
    }
}
