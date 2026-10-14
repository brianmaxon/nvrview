// Camera motion as the app sees it (docs/FFI.md "Camera motion (PTZ)", docs/UX.md "Pan, tilt, and
// zoom"): the answer of nv_ptz_info, the ptz events, the Alt keys, the held-move timing, and the
// lens arithmetic. Pure, so the real core and the fake are driven the same way and the rules are
// tested without a window (the Mac app's NVRCore/PTZ.swift does the same).

using System.Text.Json;

namespace NvrView.Shell;

/// <summary>A pan or tilt direction (<c>nv_ptz_move</c>).</summary>
public enum PtzDirection
{
    Left,
    Right,
    Up,
    Down,
}

/// <summary>A lens motor (<c>nv_ptz_lens</c>).</summary>
public enum PtzLensAxis
{
    Zoom,
    Focus,
}

/// <summary>The words the core takes.</summary>
public static class PtzNames
{
    public static string Of(PtzDirection d) => d switch
    {
        PtzDirection.Left => "left",
        PtzDirection.Right => "right",
        PtzDirection.Up => "up",
        _ => "down",
    };

    public static string Of(PtzLensAxis a) => a == PtzLensAxis.Zoom ? "zoom" : "focus";
}

/// <summary>One lens motor's range and position, in the camera's own steps (294).</summary>
public readonly record struct PtzRange(int Min, int Max, int Cur)
{
    /// <summary>The position limited to the range.</summary>
    public int Clamped(int position) => Math.Min(Math.Max(position, Min), Math.Max(Max, Min));

    /// <summary>How far the position is along the range, 0 to 1.</summary>
    public double Fraction(int position) => Max > Min ? (double)(Clamped(position) - Min) / (Max - Min) : 0;

    /// <summary>The position that far along the range.</summary>
    public int PositionAt(double fraction) => Max > Min ? Clamped(Min + (int)Math.Round(fraction * (Max - Min), MidpointRounding.AwayFromZero)) : Min;

    /// <summary>The position one key step from <paramref name="position"/>: a twentieth of the range (at least 1), limited to the range.</summary>
    public int Stepped(int position, int direction)
    {
        int step = Math.Max(1, (Max - Min) / 20);
        return Clamped(position + (direction >= 0 ? step : -step));
    }
}

/// <summary>One preset of the camera's list; recall only.</summary>
public readonly record struct PtzPreset(int Id, string Name);

/// <summary>What <c>nv_ptz_info</c> answers for a tile's camera.</summary>
public sealed record PtzInfo
{
    public bool PanTilt { get; init; }

    public PtzRange? Zoom { get; init; }

    public PtzRange? Focus { get; init; }

    public IReadOnlyList<PtzPreset> Presets { get; init; } = [];

    public int MinSpeed { get; init; } = 1;

    public int MaxSpeed { get; init; } = 64;

    public bool HasLens => Zoom is not null || Focus is not null;

    public PtzRange? Range(PtzLensAxis axis) => axis == PtzLensAxis.Zoom ? Zoom : Focus;

    public bool SameShape(PtzInfo other) =>
        PanTilt == other.PanTilt && Zoom is null == other.Zoom is null && Focus is null == other.Focus is null;

    /// <summary>Parses the JSON of <c>nv_ptz_info</c>; null when it is not one.</summary>
    public static PtzInfo? Parse(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            JsonElement r = doc.RootElement;
            if (r.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            var presets = new List<PtzPreset>();
            if (r.TryGetProperty("presets", out JsonElement list) && list.ValueKind == JsonValueKind.Array)
            {
                foreach (JsonElement p in list.EnumerateArray())
                {
                    if (p.ValueKind == JsonValueKind.Object && p.TryGetProperty("id", out JsonElement id) && id.TryGetInt32(out int n))
                    {
                        presets.Add(new PtzPreset(n, p.TryGetProperty("name", out JsonElement name) && name.ValueKind == JsonValueKind.String ? name.GetString() ?? "" : ""));
                    }
                }
            }
            int min = 1;
            int max = 64;
            if (r.TryGetProperty("speeds", out JsonElement speeds) && speeds.ValueKind == JsonValueKind.Array)
            {
                int[] s = [.. speeds.EnumerateArray().Where(x => x.ValueKind == JsonValueKind.Number && x.TryGetInt32(out _)).Select(x => x.GetInt32())];
                min = s.Length > 0 ? s[0] : 1;
                max = s.Length > 1 ? s[1] : 64;
            }
            return new PtzInfo
            {
                PanTilt = r.TryGetProperty("pan_tilt", out JsonElement pt) && pt.ValueKind == JsonValueKind.True,
                Zoom = Range(r, "zoom"),
                Focus = Range(r, "focus"),
                Presets = presets,
                MinSpeed = min,
                MaxSpeed = max,
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static PtzRange? Range(JsonElement r, string name)
    {
        if (!r.TryGetProperty(name, out JsonElement e) || e.ValueKind != JsonValueKind.Object
            || !e.TryGetProperty("min", out JsonElement min) || !min.TryGetInt32(out int lo)
            || !e.TryGetProperty("max", out JsonElement max) || !max.TryGetInt32(out int hi))
        {
            return null;
        }
        return new PtzRange(lo, hi, e.TryGetProperty("cur", out JsonElement cur) && cur.TryGetInt32(out int c) ? c : lo);
    }
}

/// <summary>How a <c>ptz</c> event ended.</summary>
public enum PtzEventState
{
    Sent,
    Refused,
    Failed,
    Other,
}

/// <summary>A <c>ptz</c> event: what became of one command (docs/FFI.md).</summary>
public sealed record PtzEvent(ulong Tile, string Action, PtzEventState State, string? Reason = null, int? Status = null, int? Position = null)
{
    public static PtzEventState StateOf(string? s) => s switch
    {
        "sent" => PtzEventState.Sent,
        "refused" => PtzEventState.Refused,
        "failed" => PtzEventState.Failed,
        _ => PtzEventState.Other,
    };

    /// <summary>A short line for the popover and the message over the tile, null for an event that needs none.</summary>
    public string? Problem => State switch
    {
        PtzEventState.Refused => Reason switch
        {
            "rate_limited" => "Moving too fast; try again in a moment",
            "not_supported" => "This camera cannot do that",
            "unknown_preset" => "That preset is no longer on the camera",
            "max_hold" => "Stopped after 30 seconds; release and press again",
            _ => $"The camera was not moved ({Reason ?? "refused"})",
        },
        PtzEventState.Failed => Reason switch
        {
            "device_refused" => "The camera refused the command" + (Status is { } s ? $" (status {s})" : ""),
            "connection_lost" or "session_closed" => "The connection to the camera was lost",
            "timed_out" => "The camera did not answer",
            _ => $"The command failed ({Reason ?? "failed"})",
        },
        _ => null,
    };
}

/// <summary>What to tell the core about a held move.</summary>
public readonly record struct PtzCall(bool IsStop, PtzDirection Direction = PtzDirection.Left, int Speed = 0)
{
    public static PtzCall Stop => new(true);

    public static PtzCall Move(PtzDirection d, int speed) => new(false, d, speed);

    public override string ToString() => IsStop ? "stop" : $"move {PtzNames.Of(Direction)} {Speed}";
}

/// <summary>
/// The timing of a held pan or tilt: the app calls <c>nv_ptz_move</c> again at least every 500 ms while
/// the control is held (the core stops the camera 1 s after the last call); release calls
/// <c>nv_ptz_stop</c>. Pure, on any monotonic clock.
/// </summary>
public sealed class PtzHold
{
    /// <summary>How often a held move is renewed (well inside the core's 500 ms).</summary>
    public const double RenewInterval = 0.25;

    private double _lastCall;

    public PtzDirection? Direction { get; private set; }

    public int Speed { get; private set; } = 20;

    public bool IsHeld => Direction is not null;

    /// <summary>The control goes down (or its direction or speed changes while held).</summary>
    public PtzCall Press(PtzDirection direction, int speed, double now)
    {
        Direction = direction;
        Speed = speed;
        _lastCall = now;
        return PtzCall.Move(direction, speed);
    }

    /// <summary>The control comes up: a stop when something was held.</summary>
    public PtzCall? Release()
    {
        if (Direction is null)
        {
            return null;
        }
        Direction = null;
        return PtzCall.Stop;
    }

    /// <summary>Time passes: the renewal when it is due.</summary>
    public PtzCall? Tick(double now)
    {
        if (Direction is not { } d || now - _lastCall < RenewInterval)
        {
            return null;
        }
        _lastCall = now;
        return PtzCall.Move(d, Speed);
    }
}

/// <summary>A drag on a lens slider sends a position at most five times a second and once when it ends.</summary>
public sealed class PtzLensThrottle
{
    public const double Interval = 0.2;

    private double? _lastSent;
    private int? _pending;

    /// <summary>The slider is at <paramref name="position"/>: the position to send now, if the interval has passed (otherwise it waits for <see cref="Tick"/> or <see cref="End"/>).</summary>
    public int? Moved(int position, double now)
    {
        if (_lastSent is { } last && now - last < Interval)
        {
            _pending = position;
            return null;
        }
        _lastSent = now;
        _pending = null;
        return position;
    }

    /// <summary>Time passes during the drag: a waiting position once the interval has passed.</summary>
    public int? Tick(double now)
    {
        if (_pending is not { } p || _lastSent is not { } last || now - last < Interval)
        {
            return null;
        }
        _lastSent = now;
        _pending = null;
        return p;
    }

    /// <summary>The drag ends: the last position, if it was not sent.</summary>
    public int? End()
    {
        int? p = _pending;
        _lastSent = null;
        _pending = null;
        return p;
    }
}

/// <summary>The names automation uses for <c>nv_motion_grant</c>.</summary>
public static class MotionClient
{
    /// <summary>Automatic correction (docs/ARCHITECTURE.md "Device commands").</summary>
    public const string AutoCorrection = "auto_correction";
}

/// <summary>What a camera key does to the selected tile's camera (docs/UX.md "Capture and camera keys").</summary>
public readonly record struct PtzKey(PtzKeyKind Kind, PtzDirection Direction = PtzDirection.Left, int Value = 0)
{
    public static PtzKey Move(PtzDirection d) => new(PtzKeyKind.Move, d);

    public static PtzKey Zoom(int direction) => new(PtzKeyKind.Zoom, Value: direction);

    public static PtzKey Preset(int n) => new(PtzKeyKind.Preset, Value: n);
}

public enum PtzKeyKind
{
    /// <summary>A plain arrow on a tile that pans and tilts: moves while held.</summary>
    Move,
    /// <summary>= or - (alone or with Alt): lens zoom in (+1) or out (-1).</summary>
    Zoom,
    /// <summary>Alt with 1 to 9: recalls preset 1 to 9 (the order of the camera's list).</summary>
    Preset,
}

/// <summary>Reads the camera keys by key position, so they work on every layout.</summary>
public static class PtzKeys
{
    // Windows virtual-key codes of the arrows and the digit row.
    public const int VkLeft = 0x25;
    public const int VkUp = 0x26;
    public const int VkRight = 0x27;
    public const int VkDown = 0x28;

    // Scan codes (set 1) of the keys by position: the digits 1 to 9, "-" and "=".
    public const int ScanMinus = 0x0C;
    public const int ScanEquals = 0x0D;
    private const int ScanDigit1 = 0x02;

    /// <summary>
    /// The camera key for a key press, never while text is edited. A plain arrow (no modifier) is a move when the tile
    /// pans and tilts (<paramref name="canMove"/>); otherwise, and with Alt, it is the selection's. Arrows repeat (the
    /// model's timer renews the hold, so a repeat changes nothing); the others ignore repeats. = and - zoom alone when
    /// the tile has a zoom lens (<paramref name="canZoom"/>) and with Alt always; digits recall presets with Alt only.
    /// <paramref name="scanCode"/> 0 (unknown) falls back to the virtual key for the digits.
    /// </summary>
    public static PtzKey? Map(int virtualKey, int scanCode, bool alt, bool ctrl, bool shift, bool isRepeat, bool editingText = false,
        bool canMove = false, bool canZoom = false)
    {
        if (editingText || ctrl || shift)
        {
            return null;
        }
        if (!alt && ArrowOf(virtualKey) is { } d)
        {
            return canMove ? PtzKey.Move(d) : null;
        }
        if (isRepeat)
        {
            return null;
        }
        if (!alt)
        {
            // Plain = and - zoom a lens tile.
            if (!canZoom)
            {
                return null;
            }
            if (scanCode == ScanEquals || (scanCode == 0 && virtualKey is 0xBB or 0x6B))
            {
                return PtzKey.Zoom(1);
            }
            if (scanCode == ScanMinus || (scanCode == 0 && virtualKey is 0xBD or 0x6D))
            {
                return PtzKey.Zoom(-1);
            }
            return null;
        }
        if (ArrowOf(virtualKey) is not null)
        {
            return null;
        }
        if (scanCode == ScanEquals)
        {
            return PtzKey.Zoom(1);
        }
        if (scanCode == ScanMinus)
        {
            return PtzKey.Zoom(-1);
        }
        if (scanCode is >= ScanDigit1 and <= ScanDigit1 + 8)
        {
            return PtzKey.Preset(scanCode - ScanDigit1 + 1);
        }
        if (scanCode == 0)
        {
            // No scan code (a synthesized key): the unshifted US-style codes.
            return virtualKey switch
            {
                0xBB or 0x6B => PtzKey.Zoom(1),
                0xBD or 0x6D => PtzKey.Zoom(-1),
                >= 0x31 and <= 0x39 => PtzKey.Preset(virtualKey - 0x30),
                _ => null,
            };
        }
        return null;
    }

    /// <summary>The direction an arrow key's release ends, with or without Alt still down.</summary>
    public static PtzDirection? ArrowOf(int virtualKey) => virtualKey switch
    {
        VkLeft => PtzDirection.Left,
        VkRight => PtzDirection.Right,
        VkUp => PtzDirection.Up,
        VkDown => PtzDirection.Down,
        _ => null,
    };
}
