// The app's camera motion (docs/UX.md "Pan, tilt, and zoom", docs/CAPTURE.md): what each camera can do
// (nv_ptz_info, read off the UI thread), the held move (renewed while a control is held, stopped on
// release, window close, tile close and focus loss), preset recall, and lens positions. These are the
// user's own controls and need no enable; the core still rate-limits and logs every command, and stops
// the camera by itself a second after the last renewal. The Mac app's PTZModel.swift is the reference.
//
// One thread (the UI thread in the app); the one blocking call, the capabilities read, runs on
// whatever <c>background</c> runs it and comes back through <c>post</c>.

using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>The core's camera motion calls (<see cref="NvCore"/> in the app, a model of a camera in tests).</summary>
public interface IPtzCore
{
    /// <summary>The JSON of <c>nv_ptz_info</c>; waits for the camera.</summary>
    string Info(ulong tile);

    void Move(ulong tile, PtzDirection direction, int speed);

    void Stop(ulong tile);

    void Lens(ulong tile, PtzLensAxis axis, int position);

    void Preset(ulong tile, int id);
}

/// <summary>What the model's listeners hear.</summary>
public readonly record struct PtzChange(CameraRef? ProblemFor, string? Problem)
{
    public static PtzChange State => new(null, null);
}

public sealed class PtzModel
{
    /// <summary>The camera's positions are read again this long after a zoom or a preset recall (when the camera has settled).</summary>
    public const double RereadDelay = 6;

    private readonly IPtzCore _core;
    private readonly IViewerClock _clock;
    private readonly Func<CameraRef, ulong?> _tileOf;
    private readonly Func<ulong, CameraRef?> _cameraOf;
    private readonly Action<Action> _background;
    private readonly Action<Action> _post;
    private readonly Action<bool> _backupTimer;
    private readonly Action<int> _saveSpeed;

    private readonly PtzHold _hold = new();
    private readonly Dictionary<CameraRef, PtzInfo> _infos = [];
    private readonly HashSet<CameraRef> _loading = [];
    private readonly Dictionary<CameraRef, List<Action<PtzInfo?>>> _waiting = [];
    private readonly Dictionary<CameraRef, Dictionary<PtzLensAxis, int>> _positions = [];
    private readonly Dictionary<CameraRef, Dictionary<PtzLensAxis, PtzLensThrottle>> _throttles = [];
    private readonly Dictionary<CameraRef, double> _rereadAt = [];
    private readonly Dictionary<CameraRef, string> _problems = [];
    private readonly List<(double At, string Text)> _calls = [];
    private readonly List<PtzEvent> _events = [];
    private int _speed;

    /// <param name="core">The core's calls.</param>
    /// <param name="clock">The viewer's clock (a manual one in tests).</param>
    /// <param name="tileOf">The core tile that shows a camera, null while none is open.</param>
    /// <param name="cameraOf">The camera a core tile shows.</param>
    /// <param name="speed">The pad's speed kept from the last run (0: the default, 20).</param>
    /// <param name="saveSpeed">Keeps the pad's speed.</param>
    /// <param name="background">Runs the blocking read off the UI thread (null: right here).</param>
    /// <param name="post">Brings a result back to the UI thread (null: right here).</param>
    /// <param name="backupTimer">Starts (true) or stops (false) a timer that calls <see cref="Tick"/> while a move is held, for when the stage is not refreshing.</param>
    public PtzModel(
        IPtzCore core, IViewerClock clock, Func<CameraRef, ulong?> tileOf, Func<ulong, CameraRef?> cameraOf,
        int speed = 0, Action<int>? saveSpeed = null, Action<Action>? background = null, Action<Action>? post = null,
        Action<bool>? backupTimer = null)
    {
        _core = core;
        _clock = clock;
        _tileOf = tileOf;
        _cameraOf = cameraOf;
        _speed = speed == 0 ? 20 : Math.Clamp(speed, 1, 64);
        _saveSpeed = saveSpeed ?? (_ => { });
        _background = background ?? (a => a());
        _post = post ?? (a => a());
        _backupTimer = backupTimer ?? (_ => { });
    }

    /// <summary>Raised on a state change and when a problem is announced for a camera.</summary>
    public event Action<PtzChange>? Changed;

    /// <summary>The pad's speed (1 to 64), kept between runs.</summary>
    public int Speed
    {
        get => _speed;
        set
        {
            _speed = Math.Clamp(value, 1, 64);
            _saveSpeed(_speed);
        }
    }

    /// <summary>The latest line to show for a camera (a refusal or a failure).</summary>
    public string? ProblemFor(CameraRef camera) => _problems.GetValueOrDefault(camera);

    public void ClearProblem(CameraRef camera) => _problems.Remove(camera);

    /// <summary>The camera being moved, while a control is held.</summary>
    public CameraRef? Held { get; private set; }

    /// <summary>The arrow key that holds the move, if a key does.</summary>
    public PtzDirection? KeyHeld { get; private set; }

    /// <summary>Reads of the capabilities that finished (self-test).</summary>
    public int InfoReads { get; private set; }

    /// <summary>Every move, renewal and stop the app asked for, with the time (self-test).</summary>
    public IReadOnlyList<(double At, string Text)> Calls => _calls;

    /// <summary>The <c>ptz</c> events heard (self-test).</summary>
    public IReadOnlyList<PtzEvent> Events => _events;

    /// <summary>A frozen app renews nothing (self-test of the dead-man stop).</summary>
    public bool FrozenForTest { get; set; }

    private double Now => _clock.Now;

    private void Notify(PtzChange c) => Changed?.Invoke(c);

    // ---- what a camera can do ----

    /// <summary>The camera's capabilities if they are known.</summary>
    public PtzInfo? InfoFor(CameraRef camera) => _infos.GetValueOrDefault(camera);

    /// <summary>Runs <paramref name="body"/> with the camera's capabilities: at once when known, else after reading them (null when the camera did not answer).</summary>
    public void WithInfo(CameraRef camera, Action<PtzInfo?> body)
    {
        if (_infos.TryGetValue(camera, out PtzInfo? info))
        {
            body(info);
            return;
        }
        if (!_waiting.TryGetValue(camera, out List<Action<PtzInfo?>>? list))
        {
            list = [];
            _waiting[camera] = list;
        }
        list.Add(body);
        Load(camera);
    }

    /// <summary>Reads the camera's capabilities and lens positions (waits for the camera: off the UI thread).</summary>
    public void Load(CameraRef camera, bool force = false)
    {
        if (_tileOf(camera) is not { } tile || _loading.Contains(camera) || (!force && _infos.ContainsKey(camera)))
        {
            return;
        }
        _loading.Add(camera);
        _background(() =>
        {
            PtzInfo? info = null;
            try
            {
                info = PtzInfo.Parse(_core.Info(tile));
            }
            catch (NvException)
            {
            }
            _post(() => FinishLoad(camera, info));
        });
    }

    private void FinishLoad(CameraRef camera, PtzInfo? info)
    {
        _loading.Remove(camera);
        InfoReads++;
        List<Action<PtzInfo?>> callbacks = _waiting.Remove(camera, out List<Action<PtzInfo?>>? list) ? list : [];
        if (info is not null)
        {
            _infos[camera] = info;
            _problems.Remove(camera);
            Dictionary<PtzLensAxis, int> positions = _positions.GetValueOrDefault(camera) ?? [];
            foreach (PtzLensAxis axis in Enum.GetValues<PtzLensAxis>())
            {
                if (info.Range(axis) is { } r)
                {
                    positions[axis] = r.Cur;
                }
            }
            _positions[camera] = positions;
        }
        else
        {
            const string text = "The camera did not answer";
            _problems[camera] = text;
            Notify(new PtzChange(camera, text));
        }
        foreach (Action<PtzInfo?> body in callbacks)
        {
            body(info);
        }
        Notify(PtzChange.State);
    }

    /// <summary>The lens position to show for an axis: the latest sent or read.</summary>
    public int? LensPosition(CameraRef camera, PtzLensAxis axis) =>
        _positions.TryGetValue(camera, out Dictionary<PtzLensAxis, int>? d) && d.TryGetValue(axis, out int p) ? p : InfoFor(camera)?.Range(axis)?.Cur;

    // ---- moving (pad buttons and keys) ----

    /// <summary>A direction control goes down: the move starts, and is renewed until <see cref="Release"/>.</summary>
    public void Press(PtzDirection direction, CameraRef camera)
    {
        if (_tileOf(camera) is null)
        {
            return;
        }
        if (Held is { } h && h != camera)
        {
            Release();
        }
        PtzCall call = _hold.Press(direction, Speed, Now);
        Held = camera;
        _backupTimer(true);
        Send(call, camera);
    }

    /// <summary>The control comes up (or the window closes, the tile closes, the focus is lost): the camera is stopped.</summary>
    public void Release()
    {
        KeyHeld = null;
        _backupTimer(false);
        if (Held is not { } camera)
        {
            _ = _hold.Release();
            return;
        }
        Held = null;
        if (_hold.Release() is { } call)
        {
            Send(call, camera);
        }
        Notify(PtzChange.State);
    }

    /// <summary>Stops whatever is held, for any reason that is not a release.</summary>
    public void StopAll() => Release();

    /// <summary>A key holds the move (Alt with an arrow). Key repeats change nothing.</summary>
    public void KeyDown(PtzDirection direction, CameraRef camera)
    {
        if (KeyHeld == direction && Held == camera)
        {
            return;
        }
        KeyHeld = direction;
        Press(direction, camera);
        KeyHeld = direction;
    }

    /// <summary>An arrow key came up: the move it held ends.</summary>
    public void KeyUp(PtzDirection direction)
    {
        if (KeyHeld == direction)
        {
            Release();
        }
    }

    /// <summary>Alt came up while an arrow key still holds a move.</summary>
    public void ModifiersChanged(bool altDown)
    {
        if (!altDown && KeyHeld is not null)
        {
            Release();
        }
    }

    /// <summary>The tile of a camera closed: its move, readings and positions go.</summary>
    public void CameraClosed(CameraRef camera)
    {
        if (Held == camera)
        {
            Release();
        }
        _infos.Remove(camera);
        _loading.Remove(camera);
        _positions.Remove(camera);
        _throttles.Remove(camera);
        _rereadAt.Remove(camera);
    }

    /// <summary>
    /// Time passes (the stage's tick and the backup timer): renews a held move, sends the lens positions a
    /// drag is waiting to send, and reads the lens again when the camera has settled.
    /// </summary>
    public void Tick() => Tick(Now);

    public void Tick(double now)
    {
        if (FrozenForTest)
        {
            return;
        }
        if (_hold.Tick(now) is { } call && Held is { } camera)
        {
            Send(call, camera);
        }
        foreach ((CameraRef cam, Dictionary<PtzLensAxis, PtzLensThrottle> axes) in _throttles.ToArray())
        {
            foreach ((PtzLensAxis axis, PtzLensThrottle t) in axes.ToArray())
            {
                if (t.Tick(now) is { } p)
                {
                    SendLens(cam, axis, p);
                }
            }
        }
        foreach ((CameraRef cam, double at) in _rereadAt.ToArray())
        {
            if (now >= at)
            {
                _rereadAt.Remove(cam);
                Load(cam, force: true);
            }
        }
    }

    private void Send(PtzCall call, CameraRef camera)
    {
        if (_tileOf(camera) is not { } tile)
        {
            return;
        }
        try
        {
            _calls.Add((Now, call.ToString()));
            if (call.IsStop)
            {
                _core.Stop(tile);
            }
            else
            {
                _core.Move(tile, call.Direction, call.Speed);
            }
        }
        catch (NvException e)
        {
            Fail(camera, e.Status);
        }
    }

    private void Fail(CameraRef camera, NvStatus status) => Announce(camera, $"The camera was not moved (status {(int)status})");

    // ---- presets and lens ----

    /// <summary>Recalls a preset of the camera's list.</summary>
    public void Recall(int id, CameraRef camera)
    {
        if (_tileOf(camera) is not { } tile)
        {
            return;
        }
        try
        {
            _core.Preset(tile, id);
            _rereadAt[camera] = Now + RereadDelay;
        }
        catch (NvException e)
        {
            Fail(camera, e.Status);
        }
    }

    /// <summary>Alt with 1 to 9: the nth preset of the list (the order the camera gives). False when the camera has no such preset.</summary>
    public bool RecallNth(int n, CameraRef camera)
    {
        if (InfoFor(camera) is not { } info || n < 1 || n > info.Presets.Count)
        {
            return false;
        }
        Recall(info.Presets[n - 1].Id, camera);
        return true;
    }

    /// <summary>Alt with = or -: one step of zoom within the camera's range.</summary>
    public void ZoomStep(int direction, CameraRef camera) => StepLens(PtzLensAxis.Zoom, direction, camera);

    public void StepLens(PtzLensAxis axis, int direction, CameraRef camera)
    {
        if (InfoFor(camera)?.Range(axis) is not { } range)
        {
            return;
        }
        int current = LensPosition(camera, axis) ?? range.Cur;
        SendLens(camera, axis, range.Stepped(current, direction));
    }

    /// <summary>A slider drag reached <paramref name="position"/>: sent at most five times a second.</summary>
    public void DragLens(PtzLensAxis axis, int position, CameraRef camera)
    {
        if (InfoFor(camera)?.Range(axis) is not { } range)
        {
            return;
        }
        int p = range.Clamped(position);
        PositionsOf(camera)[axis] = p;
        if (ThrottleOf(camera, axis).Moved(p, Now) is { } send)
        {
            SendLens(camera, axis, send);
        }
    }

    /// <summary>The drag ended: the last position goes out.</summary>
    public void EndLensDrag(PtzLensAxis axis, CameraRef camera)
    {
        if (_throttles.GetValueOrDefault(camera)?.GetValueOrDefault(axis)?.End() is { } p)
        {
            SendLens(camera, axis, p);
        }
    }

    private Dictionary<PtzLensAxis, int> PositionsOf(CameraRef camera)
    {
        if (!_positions.TryGetValue(camera, out Dictionary<PtzLensAxis, int>? d))
        {
            d = [];
            _positions[camera] = d;
        }
        return d;
    }

    private PtzLensThrottle ThrottleOf(CameraRef camera, PtzLensAxis axis)
    {
        if (!_throttles.TryGetValue(camera, out Dictionary<PtzLensAxis, PtzLensThrottle>? d))
        {
            d = [];
            _throttles[camera] = d;
        }
        if (!d.TryGetValue(axis, out PtzLensThrottle? t))
        {
            t = new PtzLensThrottle();
            d[axis] = t;
        }
        return t;
    }

    private void SendLens(CameraRef camera, PtzLensAxis axis, int position)
    {
        if (_tileOf(camera) is not { } tile)
        {
            return;
        }
        int p = InfoFor(camera)?.Range(axis) is { } r ? r.Clamped(position) : position;
        PositionsOf(camera)[axis] = p;
        try
        {
            _core.Lens(tile, axis, p);
            if (axis == PtzLensAxis.Zoom)
            {
                _rereadAt[camera] = Now + RereadDelay;
            }
        }
        catch (NvException e)
        {
            Fail(camera, e.Status);
        }
        Notify(PtzChange.State);
    }

    // ---- events ----

    /// <summary>
    /// A <c>ptz</c> event: refusals and failures become a line over the tile and in the flyout; the core's
    /// own stops end the app's hold.
    /// </summary>
    public void Handle(PtzEvent e)
    {
        _events.Add(e);
        if (_cameraOf(e.Tile) is not { } camera)
        {
            return;
        }
        if (e.Action == "stop" && e.State == PtzEventState.Sent
            && (e.Reason is "max_hold" or "device_refused" or "session_closed" or "reconnected" or "deadman")
            && Held == camera)
        {
            // The core stopped the camera by itself (the 30 s limit, a refusal): the app's hold is over too.
            Release();
            if (e.Reason == "max_hold")
            {
                Announce(camera, "Stopped after 30 seconds; release and press again");
            }
        }
        if (e.Problem is { } text)
        {
            Announce(camera, text);
        }
    }

    private void Announce(CameraRef camera, string text)
    {
        _problems[camera] = text;
        Notify(new PtzChange(camera, text));
        Notify(PtzChange.State);
    }
}
