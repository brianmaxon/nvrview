// A model of cameras that move, answering PtzModel the way the core does (docs/FFI.md "Camera motion
// (PTZ)"): the capabilities as nv_ptz_info's JSON, a move that goes on until stopped or until the app has
// been quiet for a second (the core's dead-man stop), presets, and lens positions clamped to their ranges.
// For the shell tests; the app's self-test runs the real core against tools/mock-nvr.

using System.Globalization;
using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>A camera that moves, by core tile.</summary>
public sealed class FakePtzCamera
{
    public bool PanTilt { get; init; } = true;

    public PtzRange? Zoom { get; set; } = new(0, 28, 3);

    public PtzRange? Focus { get; set; } = new(0, 28, 10);

    public List<PtzPreset> Presets { get; init; } = [new(0, "Door"), new(1, "Gate"), new(2, "Yard")];

    /// <summary>The lens (0 or 1) the tile addresses.</summary>
    public int Lens { get; init; }

    public PtzDirection? Moving { get; internal set; }

    internal double LastMove;
}

public sealed class FakePtzCore(IViewerClock clock) : IPtzCore
{
    /// <summary>The core's dead-man limit: a move not renewed for this long stops.</summary>
    public const double DeadMan = 1.0;

    private readonly Dictionary<ulong, FakePtzCamera> _cameras = [];

    /// <summary>Every call the core received: the tile, the text ("move left 20", "stop", "preset 1", "lens zoom 28") and the lens addressed.</summary>
    public List<(ulong Tile, string Text, int Lens)> Calls { get; } = [];

    /// <summary>The next call fails with this status, once.</summary>
    public NvStatus? FailNext { get; set; }

    public FakePtzCamera Add(ulong tile, FakePtzCamera camera)
    {
        _cameras[tile] = camera;
        return camera;
    }

    public FakePtzCamera Camera(ulong tile) => _cameras[tile];

    /// <summary>Whether the camera is moving now: a move not stopped and renewed within the dead-man limit.</summary>
    public bool IsMoving(ulong tile) =>
        _cameras[tile] is { Moving: not null } c && clock.Now - c.LastMove < DeadMan;

    public string Info(ulong tile)
    {
        FakePtzCamera c = Known(tile);
        static string R(PtzRange? r) => r is { } x ? $"{{\"min\":{x.Min},\"max\":{x.Max},\"cur\":{x.Cur}}}" : "null";
        string presets = string.Join(",", c.Presets.Select(p => $"{{\"id\":{p.Id},\"name\":\"{p.Name}\"}}"));
        return string.Create(CultureInfo.InvariantCulture,
            $"{{\"pan_tilt\":{(c.PanTilt ? "true" : "false")},\"zoom\":{R(c.Zoom)},\"focus\":{R(c.Focus)},\"presets\":[{presets}],\"speeds\":[1,64]}}");
    }

    public void Move(ulong tile, PtzDirection direction, int speed)
    {
        FakePtzCamera c = Begin(tile, $"move {PtzNames.Of(direction)} {speed}");
        c.Moving = direction;
        c.LastMove = clock.Now;
    }

    public void Stop(ulong tile)
    {
        FakePtzCamera c = Begin(tile, "stop");
        c.Moving = null;
    }

    public void Lens(ulong tile, PtzLensAxis axis, int position)
    {
        FakePtzCamera c = Begin(tile, $"lens {PtzNames.Of(axis)} {position}");
        PtzRange? r = axis == PtzLensAxis.Zoom ? c.Zoom : c.Focus;
        if (r is { } range)
        {
            PtzRange moved = range with { Cur = range.Clamped(position) };
            if (axis == PtzLensAxis.Zoom)
            {
                c.Zoom = moved;
            }
            else
            {
                c.Focus = moved;
            }
        }
    }

    public void Preset(ulong tile, int id)
    {
        FakePtzCamera c = Begin(tile, $"preset {id}");
        if (c.Presets.All(p => p.Id != id))
        {
            throw new NvException(NvStatus.InvalidArgument, nameof(Preset));
        }
    }

    private FakePtzCamera Known(ulong tile) =>
        _cameras.TryGetValue(tile, out FakePtzCamera? c) ? c : throw new NvException(NvStatus.UnknownId, "ptz");

    private FakePtzCamera Begin(ulong tile, string text)
    {
        FakePtzCamera c = Known(tile);
        Calls.Add((tile, text, c.Lens));
        if (FailNext is { } status)
        {
            FailNext = null;
            throw new NvException(status, "ptz");
        }
        return c;
    }
}
