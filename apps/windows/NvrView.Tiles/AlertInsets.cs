// Alert insets (docs/UX.md "Alert insets"): when something important happens on a camera the
// window is not showing, its live video appears in an inset over the stage. The rules of the
// macOS insets (apps/macos NVRCore AlertInsets.swift: InsetKind, InsetTrigger, InsetItem,
// InsetGridSetting, InsetBoard), with the same test vectors. A pure model per window, tested
// headless with the time passed in (the viewer clock, seconds) and dates for the header.

using System.Globalization;

namespace NvrView.Tiles;

/// <summary>What an inset is for (see <see cref="InsetKind"/>).</summary>
public enum InsetReason
{
    /// <summary>A doorbell press (the device's <c>visitor</c> alarm).</summary>
    Doorbell,

    /// <summary>A conversation the user started on a camera's tile (no press): the same call as a doorbell's.</summary>
    Conversation,

    /// <summary>A smoke or CO alarm heard in the camera's audio (<c>smoke_alarm</c>, <c>co_alarm</c> from <c>local_audio</c>).</summary>
    SoundAlarm,

    /// <summary>An object detection on a camera that asks for it (a person on a camera marked as an entrance).</summary>
    Detection,

    /// <summary>Not an event: the view the stage showed before a swap.</summary>
    View,

    /// <summary>A smart-alert check of a camera the window does not show: its lifetime is the check's, not the inset clock's.</summary>
    Verification,
}

/// <summary>Why an inset opened: a reason and, for sound alarms and detections, the device's kind name.</summary>
public readonly record struct InsetKind(InsetReason Reason, string Detail = "")
{
    public static InsetKind Doorbell => new(InsetReason.Doorbell);

    public static InsetKind Conversation => new(InsetReason.Conversation);

    public static InsetKind View => new(InsetReason.View);

    public static InsetKind Verification => new(InsetReason.Verification);

    public static InsetKind SoundAlarm(string kind) => new(InsetReason.SoundAlarm, kind);

    public static InsetKind Detection(string kind) => new(InsetReason.Detection, kind);

    /// <summary>Red for smoke and CO, a calmer accent for the rest.</summary>
    public bool IsCritical => Reason == InsetReason.SoundAlarm;

    /// <summary>The inset carries a call (captions, Talk, typed reply).</summary>
    public bool IsCall => Reason is InsetReason.Doorbell or InsetReason.Conversation;

    /// <summary>Doorbell and detection insets close by themselves after the event; sound alarms stay until dismissed; a check's inset goes with its check.</summary>
    public bool ClosesByItself => !IsCritical && Reason is not (InsetReason.View or InsetReason.Verification);

    /// <summary>The header's reason.</summary>
    public string Text => Reason switch
    {
        InsetReason.Doorbell => "Doorbell",
        InsetReason.Conversation => "Conversation",
        InsetReason.SoundAlarm => $"{DetectionTracker.Label(Detail)} (sound)",
        InsetReason.Detection => DetectionTracker.Label(Detail),
        InsetReason.View => "Back",
        _ => "Smart alert check",
    };
}

/// <summary>Which events open an inset (the defaults of docs/UX.md): a doorbell press, smoke and CO sound alarms, and people on cameras marked as entrances; further kinds per camera.</summary>
public static class InsetTrigger
{
    /// <summary>The person kinds a camera marked as an entrance opens an inset for.</summary>
    public static readonly IReadOnlySet<string> EntranceKinds = new HashSet<string> { "people", "person" };

    /// <summary>The inset an event (start or stop) belongs to, or null if it opens none. <paramref name="extraKinds"/> are the object kinds the user turned on for the camera.</summary>
    public static InsetKind? For(DetectionEvent e, bool entrance, IReadOnlySet<string>? extraKinds = null)
    {
        if (e.Alarm)
        {
            return e.Kind == "visitor" ? InsetKind.Doorbell : null;
        }
        if (e.Source == DetectionSource.LocalAudio)
        {
            return AlertPolicy.Kinds.Contains(e.Kind) ? InsetKind.SoundAlarm(e.Kind) : null;
        }
        if (e.Kind == DetectionEvent.MotionKind)
        {
            return null;
        }
        if (entrance && EntranceKinds.Contains(e.Kind))
        {
            return InsetKind.Detection(e.Kind);
        }
        return extraKinds is not null && extraKinds.Contains(e.Kind) ? InsetKind.Detection(e.Kind) : null;
    }
}

/// <summary>One inset: a camera, why, and since when.</summary>
public sealed record InsetItem(CameraRef Camera, InsetKind Kind, DateTime FirstAt, DateTime LastAt, int Count, bool Active, double StartedAt, double? EndedAt, double UsedAt)
{
    /// <summary>Whether the pointer is over it (hovering holds it open).</summary>
    public bool Hovered { get; init; }

    /// <summary>The view the stage showed before a swap (its camera in the inset), rather than an event.</summary>
    public bool IsReturn { get; init; }

    /// <summary>A return inset of a stage that showed several tiles: it draws the whole layout in miniature (<see cref="Camera"/> is only its identity).</summary>
    public bool ShowsLayout { get; init; }

    /// <summary>The call is shown in its own window: the item is kept for the call's whole life but not drawn, counted, leased, swapped or closed by the clock here.</summary>
    public bool Detached { get; init; }

    /// <summary>The header's line: reason, first time (and count and last time).</summary>
    public string Header(DateTime now)
    {
        string Stamp(DateTime d) => d.Date == now.Date
            ? d.ToString("T", CultureInfo.CurrentCulture)
            : d.ToString("g", CultureInfo.CurrentCulture);
        string text = $"{Kind.Text} · {Stamp(FirstAt)}";
        if (Count > 1)
        {
            text += $" · {Count} times, last {Stamp(LastAt)}";
        }
        return text;
    }
}

/// <summary>
/// How many insets a window draws at once, as a grid: <see cref="Rows"/> insets stacked in a
/// column, up to <see cref="Columns"/> columns. The rest wait in "+N more". The default, 3 rows in
/// 1 column, is the original stack of three.
/// </summary>
public readonly record struct InsetGridSetting
{
    public const int MinRows = 1;
    public const int MaxRows = 4;
    public const int MinColumns = 1;
    public const int MaxColumns = 6;

    public static readonly InsetGridSetting Standard = new(3, 1);

    /// <summary>Clamped to the ranges.</summary>
    public InsetGridSetting(int rows, int columns)
    {
        Rows = Math.Clamp(rows, MinRows, MaxRows);
        Columns = Math.Clamp(columns, MinColumns, MaxColumns);
    }

    public int Rows { get; }

    public int Columns { get; }

    /// <summary>How many insets are drawn at most.</summary>
    public int Capacity => Rows * Columns;

    /// <summary>The saved form, <c>{"rows":..,"columns":..}</c>.</summary>
    public string ToJson() => new System.Text.Json.Nodes.JsonObject { ["rows"] = Rows, ["columns"] = Columns }.ToJsonString();

    /// <summary>A saved value, through the clamping constructor; null when it is not an object with integer rows and columns.</summary>
    public static InsetGridSetting? FromJson(string? json)
    {
        try
        {
            if (json is not null && System.Text.Json.Nodes.JsonNode.Parse(json) is System.Text.Json.Nodes.JsonObject o
                && o["rows"] is System.Text.Json.Nodes.JsonValue r && o["columns"] is System.Text.Json.Nodes.JsonValue c
                && r.TryGetValue(out int rows) && c.TryGetValue(out int columns))
            {
                return new InsetGridSetting(rows, columns);
            }
        }
        catch (System.Text.Json.JsonException)
        {
        }
        catch (FormatException)
        {
        }
        catch (InvalidOperationException)
        {
        }
        return null;
    }
}

/// <summary>
/// A window's insets, newest first, and the swap: clicking an inset puts its camera on the whole
/// stage and the view that was there into an inset (<see cref="InsetItem.IsReturn"/>), so one more
/// click swaps back.
///
/// The order is stable: an event on a camera that has an inset refreshes it where it is; a new
/// camera takes the top. At most <see cref="Capacity"/> insets are drawn; a newer one pushes an
/// older one into "+N more" only once the older has been on screen <see cref="MinimumShownTime"/>,
/// otherwise it waits its turn.
/// </summary>
public sealed class InsetBoard
{
    /// <summary>How many insets are drawn by default; the rest show as "+N more".</summary>
    public static readonly int MaxShown = InsetGridSetting.Standard.Capacity;

    /// <summary>An inset stays on screen at least this long (viewer clock, seconds) before a newer one can push it out.</summary>
    public const double MinimumShownTime = 10;

    /// <summary>Doorbell and detection insets close this long after the event ended and they were last used, unless hovered.</summary>
    public const double AutoCloseDelay = 60;

    /// <summary>An event whose stop never came counts as ended this long after its last start (a lost stop must not keep an inset open for good).</summary>
    public const double LostStopAfter = 120;

    /// <summary>The camera on the whole stage after a swap, and its inset's state.</summary>
    public sealed class SwapState(InsetItem item, CameraRef? displaced, bool displacedLayout)
    {
        public InsetItem Item { get; set; } = item;

        /// <summary>The camera of the view that was on the stage (shown in the return inset), if it had one.</summary>
        public CameraRef? Displaced { get; } = displaced;

        /// <summary>The stage showed a layout (several tiles, nothing focused): the return inset draws all of it, not just <see cref="Displaced"/>.</summary>
        public bool DisplacedLayout { get; } = displacedLayout;

        public CameraRef Shown => Item.Camera;
    }

    private readonly List<InsetItem> _items = [];

    // The cameras drawn now, and since when (viewer clock).
    private readonly Dictionary<CameraRef, double> _drawn = [];
    private double _lastNow;

    public InsetBoard(int capacity = 0)
    {
        Capacity = Math.Max(capacity > 0 ? capacity : MaxShown, 1);
    }

    /// <summary>Newest first; a return inset included.</summary>
    public IReadOnlyList<InsetItem> Items => _items;

    public SwapState? Swap { get; private set; }

    /// <summary>How many insets may be drawn at once.</summary>
    public int Capacity { get; private set; }

    /// <summary>The items drawn in this window (all but the detached ones).</summary>
    public IReadOnlyList<InsetItem> Attached => [.. _items.Where(i => !i.Detached)];

    /// <summary>The insets drawn, top of the stack first.</summary>
    public IReadOnlyList<InsetItem> Shown => [.. Attached.Where(i => _drawn.ContainsKey(i.Camera))];

    /// <summary>The insets waiting for room ("+N more"), top of the stack first.</summary>
    public IReadOnlyList<InsetItem> Waiting => [.. Attached.Where(i => !_drawn.ContainsKey(i.Camera))];

    /// <summary>How many more there are than are drawn.</summary>
    public int More => Waiting.Count;

    /// <summary>Nothing on screen: no inset drawn and no swap (detached items do not count).</summary>
    public bool IsEmpty => Attached.Count == 0 && Swap is null;

    /// <summary>The calls shown in their own windows.</summary>
    public IReadOnlyList<CameraRef> DetachedCameras => [.. _items.Where(i => i.Detached).Select(i => i.Camera)];

    /// <summary>A line that changes whenever anything the window draws changes (compare before and after a call).</summary>
    public string Signature()
    {
        string Of(InsetItem i) =>
            $"{i.Camera}|{i.Kind}|{i.Count}|{i.Active}|{i.IsReturn}|{i.ShowsLayout}|{i.Detached}|{i.Hovered}|{(_drawn.ContainsKey(i.Camera) ? 1 : 0)}";
        string swap = Swap is { } s ? $"{Of(s.Item)}>{s.Displaced}>{s.DisplacedLayout}" : "";
        return string.Join(";", _items.Select(Of)) + "#" + swap + "#" + Capacity;
    }

    public InsetItem? ItemFor(CameraRef camera)
    {
        if (Swap?.Shown == camera)
        {
            return Swap.Item;
        }
        return _items.FirstOrDefault(i => i.Camera == camera && !i.IsReturn);
    }

    private int IndexOfEvent(CameraRef camera) => _items.FindIndex(i => i.Camera == camera && !i.IsReturn);

    /// <summary>
    /// An event started. <paramref name="visible"/>: the window shows the camera already (it gets
    /// its tile alert instead). A repeat on a camera that has an inset updates it. <paramref name="detached"/>:
    /// a call that opens in its own window; its item is recorded whether or not the camera is
    /// visible, and a repeat keeps an item's presentation. Returns whether anything changed.
    /// </summary>
    public bool Started(InsetKind kind, CameraRef camera, bool visible, double now, DateTime at, bool detached = false)
    {
        if (Swap is { } s && s.Shown == camera)
        {
            s.Item = Repeated(s.Item, kind, now, at);
            return true;
        }
        int i = IndexOfEvent(camera);
        if (i >= 0)
        {
            // Refreshed where it is: no churn among the insets.
            _items[i] = Repeated(_items[i], kind, now, at);
            Settle(now);
            return true;
        }
        if (visible && !detached)
        {
            return false;
        }
        _items.Insert(0, new InsetItem(camera, kind, at, at, 1, true, now, null, now) { Detached = detached });
        Settle(now);
        return true;
    }

    private static InsetItem Repeated(InsetItem item, InsetKind kind, double now, DateTime at) =>
        item with
        {
            Count = item.Count + 1,
            LastAt = at,
            Active = true,
            StartedAt = now,
            EndedAt = null,
            // A sound alarm outranks the rest on the same camera.
            Kind = kind.IsCritical || !item.Kind.IsCritical ? kind : item.Kind,
        };

    /// <summary>
    /// A camera waiting in "+N more" was chosen: it takes the top and a place among the drawn
    /// ones; the lowest drawn inset goes to the waiting list. Returns whether anything changed.
    /// </summary>
    public bool BringIn(CameraRef camera, double now)
    {
        int i = _items.FindIndex(x => x.Camera == camera && !x.IsReturn && !x.Detached);
        if (_drawn.ContainsKey(camera) || i < 0)
        {
            return false;
        }
        InsetItem item = _items[i];
        _items.RemoveAt(i);
        _items.Insert(0, item with { UsedAt = Math.Max(item.UsedAt, now) });
        Settle(now, camera);
        return true;
    }

    /// <summary>Changes how many insets are drawn; those over the new capacity go to the waiting list (lowest first), regardless of how long they were up.</summary>
    public void SetCapacity(int capacity, double? now = null)
    {
        Capacity = Math.Max(capacity, 1);
        Settle(now);
    }

    // Decides which items are drawn. Kept ones stay; free places go to the waiting items, newest
    // first; a waiting item newer than a drawn one takes its place only if that has been up
    // MinimumShownTime. A return inset is always drawn, and `force` (a chip click) too.
    private void Settle(double? now, CameraRef? force = null)
    {
        if (now is { } n)
        {
            _lastNow = Math.Max(_lastNow, n);
        }
        double t = _lastNow;
        List<CameraRef> order = [.. Attached.Select(i => i.Camera)];
        foreach (CameraRef gone in _drawn.Keys.Where(c => !order.Contains(c)).ToArray())
        {
            _drawn.Remove(gone);
        }
        int Rank(CameraRef c)
        {
            int r = order.IndexOf(c);
            return r < 0 ? int.MaxValue : r;
        }
        bool IsReturn(CameraRef c) => _items.Any(i => i.Camera == c && i.IsReturn);
        // The drawn item to give up: the lowest in the stack that may go.
        CameraRef? Victim(bool ignoringTime, int below)
        {
            CameraRef? best = null;
            foreach ((CameraRef c, double since) in _drawn)
            {
                if (!IsReturn(c) && Rank(c) > below && (ignoringTime || t - since >= MinimumShownTime) && (best is null || Rank(c) > Rank(best.Value)))
                {
                    best = c;
                }
            }
            return best;
        }
        // Over capacity (it was reduced): the lowest ones go.
        while (_drawn.Count > Capacity && Victim(true, -1) is { } over)
        {
            _drawn.Remove(over);
        }
        foreach (CameraRef camera in order)
        {
            if (_drawn.ContainsKey(camera))
            {
                continue;
            }
            bool must = IsReturn(camera) || (force is { } f && f == camera);
            if (_drawn.Count < Capacity)
            {
                _drawn[camera] = t;
            }
            else if (must && Victim(true, -1) is { } out1)
            {
                _drawn.Remove(out1);
                _drawn[camera] = t;
            }
            else if (Victim(false, Rank(camera)) is { } out2)
            {
                _drawn.Remove(out2);
                _drawn[camera] = t;
            }
        }
    }

    // The first time something waiting may take a place: when the lowest drawn inset below a
    // waiting one has been up long enough.
    private double? SettleDeadline
    {
        get
        {
            List<CameraRef> order = [.. Attached.Select(i => i.Camera)];
            int firstWaiting = order.FindIndex(c => !_drawn.ContainsKey(c));
            if (firstWaiting < 0)
            {
                return null;
            }
            double? earliest = null;
            foreach ((CameraRef c, double since) in _drawn)
            {
                int r = order.IndexOf(c);
                if (r <= firstWaiting || _items.Any(i => i.Camera == c && i.IsReturn))
                {
                    continue;
                }
                earliest = Math.Min(earliest ?? double.PositiveInfinity, since + MinimumShownTime);
            }
            return earliest;
        }
    }

    /// <summary>
    /// Pop Out (<c>true</c>) or Pop In (<c>false</c>) a call's item. Popping out the swapped-in
    /// camera ends the swap first (the stage shows its own view again). Popping in puts the inset
    /// back on top with a fresh minute. Returns whether anything changed.
    /// </summary>
    public bool SetDetached(CameraRef camera, bool detached, double now)
    {
        if (detached && Swap is { } s && s.Shown == camera)
        {
            SwapBack(now);
        }
        int i = IndexOfEvent(camera);
        if (i < 0 || _items[i].Detached == detached)
        {
            return false;
        }
        InsetItem item = _items[i];
        _items.RemoveAt(i);
        _items.Insert(0, item with { Detached = detached, Hovered = false, UsedAt = Math.Max(item.UsedAt, now) });
        Settle(now);
        return true;
    }

    /// <summary>An event stopped: the inset's close clock starts (it stays meanwhile).</summary>
    public bool Stopped(InsetKind kind, CameraRef camera, double now)
    {
        if (Swap is { } s && s.Shown == camera && s.Item.Kind == kind && s.Item.Active)
        {
            s.Item = s.Item with { Active = false, EndedAt = now };
            return true;
        }
        int i = _items.FindIndex(x => x.Camera == camera && !x.IsReturn && x.Kind == kind);
        if (i < 0 || !_items[i].Active)
        {
            return false;
        }
        _items[i] = _items[i] with { Active = false, EndedAt = now };
        return true;
    }

    /// <summary>When an inset that closes by itself closes, on the viewer clock (null while hovered or for sound alarms).</summary>
    public static double? ClosesAt(InsetItem item)
    {
        if (!item.Kind.ClosesByItself || item.IsReturn || item.Hovered || item.Detached)
        {
            return null;
        }
        double ended = item.EndedAt ?? (item.StartedAt + LostStopAfter);
        return Math.Max(ended, item.UsedAt) + AutoCloseDelay;
    }

    /// <summary>Closes what is due at <paramref name="now"/>. A swapped-in camera that closes ends the swap (the stage shows its own view again). Returns what closed.</summary>
    public IReadOnlyList<InsetItem> Tick(double now)
    {
        var closed = new List<InsetItem>();
        if (Swap is { } s && ClosesAt(s.Item) is { } at && now >= at)
        {
            closed.Add(s.Item);
            EndSwap();
        }
        InsetItem[] due = [.. _items.Where(i => ClosesAt(i) is { } c && now >= c)];
        _items.RemoveAll(i => due.Contains(i));
        Settle(now);
        closed.AddRange(due);
        return closed;
    }

    /// <summary>The next time <see cref="Tick"/> has something to do.</summary>
    public double? Deadline
    {
        get
        {
            var all = new List<double>();
            foreach (InsetItem i in _items)
            {
                if (ClosesAt(i) is { } c)
                {
                    all.Add(c);
                }
            }
            if (Swap is { } s && ClosesAt(s.Item) is { } sc)
            {
                all.Add(sc);
            }
            if (SettleDeadline is { } d)
            {
                all.Add(d);
            }
            return all.Count == 0 ? null : all.Min();
        }
    }

    /// <summary>
    /// Esc or the close control: the inset goes. Dismissing the swapped-in camera ends the swap;
    /// dismissing the return inset swaps back. Returns the dismissed event's inset (null for a
    /// return inset).
    /// </summary>
    public InsetItem? Dismiss(CameraRef camera)
    {
        try
        {
            if (Swap is { } s && s.Shown == camera)
            {
                EndSwap();
                return s.Item;
            }
            int i = IndexOfEvent(camera);
            if (i >= 0)
            {
                InsetItem item = _items[i];
                _items.RemoveAt(i);
                return item;
            }
            if (_items.Any(x => x.Camera == camera && x.IsReturn))
            {
                SwapBack(null);
            }
            return null;
        }
        finally
        {
            Settle(null);
        }
    }

    /// <summary>Esc: the swapped-in camera if there is one, else the top inset.</summary>
    public InsetItem? DismissTop()
    {
        try
        {
            if (Swap is { } s)
            {
                return Dismiss(s.Shown);
            }
            if (Shown.Count == 0)
            {
                return null;
            }
            InsetItem top = Shown[0];
            if (top.IsReturn)
            {
                SwapBack(null);
                return null;
            }
            return Dismiss(top.Camera);
        }
        finally
        {
            Settle(null);
        }
    }

    /// <summary>
    /// A click on an inset (or Tab on the top one): its camera takes the stage and
    /// <paramref name="displaced"/> (the camera the stage showed, if any) goes into the return
    /// inset. A click on the return inset swaps back. Clicking another event's inset while swapped
    /// puts that one on the stage and the swapped-in one back into its inset.
    /// <paramref name="displacedLayout"/>: the stage showed a layout (several tiles, nothing
    /// focused), so the return inset draws all of it.
    /// </summary>
    public void SwapIn(CameraRef camera, CameraRef? displaced, double now, bool displacedLayout = false)
    {
        try
        {
            if (_items.Any(i => i.Camera == camera && i.IsReturn))
            {
                SwapBack(now);
                return;
            }
            int index = _items.FindIndex(i => i.Camera == camera && !i.IsReturn && !i.Detached);
            if (index < 0)
            {
                return;
            }
            InsetItem item = _items[index] with { UsedAt = now, Hovered = false };
            _items.RemoveAt(index);
            if (Swap is { } s)
            {
                _items.Insert(0, s.Item with { UsedAt = now });
                s.Item = item;
                return;
            }
            Swap = new SwapState(item, displaced, displacedLayout && displaced is not null);
            if (displaced is { } d)
            {
                _items.Insert(0, new InsetItem(d, InsetKind.View, item.FirstAt, item.LastAt, 0, false, now, now, now) { IsReturn = true, ShowsLayout = displacedLayout });
            }
        }
        finally
        {
            Settle(now);
        }
    }

    /// <summary>Tab: swaps the top inset in, or back when swapped.</summary>
    public void SwapTop(CameraRef? displaced, double now, bool displacedLayout = false)
    {
        if (Swap is not null)
        {
            SwapBack(now);
        }
        else if (Shown.Count > 0)
        {
            SwapIn(Shown[0].Camera, displaced, now, displacedLayout);
        }
    }

    /// <summary>Back to the stage's own view; the swapped-in camera returns to the top of the stack (when <paramref name="now"/> is given; null drops it).</summary>
    public void SwapBack(double? now)
    {
        try
        {
            if (Swap is not { } s)
            {
                return;
            }
            EndSwap();
            if (now is { } n)
            {
                _items.Insert(0, s.Item with { UsedAt = n });
            }
        }
        finally
        {
            Settle(now);
        }
    }

    private void EndSwap()
    {
        Swap = null;
        _items.RemoveAll(i => i.IsReturn);
    }

    /// <summary>The pointer entered or left an inset: hovering holds it open.</summary>
    public void Hover(CameraRef camera, bool inside, double now)
    {
        if (Swap is { } s && s.Shown == camera)
        {
            s.Item = s.Item with { Hovered = inside, UsedAt = Math.Max(s.Item.UsedAt, now) };
        }
        for (int i = 0; i < _items.Count; i++)
        {
            if (_items[i].Camera == camera)
            {
                _items[i] = _items[i] with { Hovered = inside, UsedAt = Math.Max(_items[i].UsedAt, now) };
            }
        }
    }

    /// <summary>Something was done with an inset (or its call): its close clock starts again.</summary>
    public void Use(CameraRef camera, double now)
    {
        if (Swap is { } s && s.Shown == camera && s.Item.UsedAt < now)
        {
            s.Item = s.Item with { UsedAt = now };
        }
        for (int i = 0; i < _items.Count; i++)
        {
            if (_items[i].Camera == camera && !_items[i].IsReturn)
            {
                _items[i] = _items[i] with { UsedAt = Math.Max(_items[i].UsedAt, now) };
            }
        }
    }

    /// <summary>Sound-alarm insets whose alert was dismissed elsewhere (another window's tile header) go too. Returns what went.</summary>
    public IReadOnlyList<InsetItem> DropSoundAlarms(Func<CameraRef, bool> gone)
    {
        var dropped = new List<InsetItem>();
        if (Swap is { } s && s.Item.Kind.IsCritical && gone(s.Shown))
        {
            dropped.Add(s.Item);
            EndSwap();
        }
        InsetItem[] out1 = [.. _items.Where(i => i.Kind.IsCritical && gone(i.Camera))];
        _items.RemoveAll(i => i.Kind.IsCritical && gone(i.Camera));
        Settle(null);
        dropped.AddRange(out1);
        return dropped;
    }

    /// <summary>Removes what matches (a camera the window no longer may show: its device was removed).</summary>
    public void Remove(Func<InsetItem, bool> condition)
    {
        if (Swap is { } s && condition(s.Item))
        {
            EndSwap();
        }
        _items.RemoveAll(i => condition(i));
        Settle(null);
    }
}
