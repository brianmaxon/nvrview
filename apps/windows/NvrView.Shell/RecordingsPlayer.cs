// The Recordings window's player (docs/UX.md "Recordings"): one playback
// (FFI-RECORDINGS.md section 6) of up to four cameras of the shown day, on
// the core's shared clock, drawn by the app's tile pipeline (each camera is a
// core tile whose pictures the window's own renderer draws, as live tiles
// are). Showing an incident opens a playback of its first four cameras over
// its day, positioned at the incident's start less the margin, and plays it;
// another incident of the same cameras and day only seeks. Live view keeps
// running (review mode, PLAYBACK.md).
//
// The playback is closed on every exit path: another day or another set of
// cameras, the window closing (Close), the app quitting
// (RecordingsService.Shutdown), and the core closing it (playback_closed).
// The same model as the macOS app's RecordingsPlayer.

using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>One camera of the playback.</summary>
public sealed class PlayerCell(RecordingCamera camera, ulong tile)
{
    /// <summary>The camera.</summary>
    public RecordingCamera Camera { get; set; } = camera;

    /// <summary>The core tile whose pictures the window draws.</summary>
    public ulong Tile { get; } = tile;

    /// <summary>The latest <c>playback_camera</c> state: buffering, playing, late, no_recording, ended, failed (empty before one).</summary>
    public string State { get; set; } = "";

    /// <summary>Why it failed, if it did.</summary>
    public string? Reason { get; set; }

    /// <summary>With <c>start_rejected</c>: the status the device refused the play with.</summary>
    public int? Status { get; set; }

    /// <summary>
    /// The message across a cell without a picture when the device refused its play (FFI-RECORDINGS.md); null
    /// otherwise.
    /// </summary>
    public string? RefusedText => State == "failed" && Reason == "start_rejected" && Status is { } s
        ? $"The NVR refused this playback (status {s})"
        : null;
}

/// <summary>The player of one Recordings window (see the file comment).</summary>
public sealed class RecordingsPlayer(RecordingsService model) : IPlayerTransport
{
    private PlayerGridMode _mode = PlayerGridMode.Tiled;
    private bool _muted;
    private float _volume = 1;

    /// <summary>Raised on the UI thread when anything the player shows changed (state, cells, position, mode).</summary>
    public event Action? Changed;

    /// <summary>The open playback, if any.</summary>
    public ulong? Playback { get; private set; }

    /// <summary>The cameras of the playback.</summary>
    public List<PlayerCell> Cells { get; private set; } = [];

    /// <summary>The playback's range start, device-local milliseconds.</summary>
    public long StartMs { get; private set; }

    /// <summary>The playback's range end.</summary>
    public long EndMs { get; private set; }

    /// <summary>Where the playhead is.</summary>
    public long PositionMs { get; private set; }

    /// <summary>The playback's state, null before it reports one.</summary>
    public NvPlaybackState? State { get; private set; }

    /// <summary>The speed.</summary>
    public double Rate { get; private set; } = 1;

    /// <summary>The day the playback covers.</summary>
    public long? Day { get; private set; }

    /// <summary>The incident the playback was last positioned for.</summary>
    public string? IncidentKey { get; private set; }

    /// <summary>The incident whose playback could not be opened (not retried by itself; a new selection does).</summary>
    public string? FailedFor { get; set; }

    /// <summary>Why the player shows nothing (a refused open), if it does not.</summary>
    public string? Message { get; private set; }

    /// <summary>Clears the refusal message (the user dismissed it).</summary>
    public void Dismiss() => Message = null;

    /// <summary>
    /// Each camera's digital zoom, Fill choice and fisheye view (docs/UX.md "Recordings"). It belongs to the player's tile:
    /// play, pause, seeks, speed changes, another incident of the same cameras and the grid, large and single views keep it;
    /// a camera that leaves the player forgets it. Nothing downloaded, exported or edited reads it.
    /// </summary>
    public PlayerTileViews Views { get; } = new();

    /// <summary>How many playbacks this player opened (self-test).</summary>
    public int Opens { get; private set; }

    /// <summary>How many seeks it sent (self-test).</summary>
    public int Seeks { get; private set; }

    /// <summary>How the cameras share the player.</summary>
    public PlayerGridMode Mode
    {
        get => _mode;
        set
        {
            if (!Equals(_mode, value))
            {
                _mode = value;
                UpdateAudio();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>Whether the sound is off.</summary>
    public bool Muted
    {
        get => _muted;
        set
        {
            if (_muted != value)
            {
                _muted = value;
                UpdateAudio();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>0 to 1.</summary>
    public float Volume
    {
        get => _volume;
        set
        {
            if (_volume != value)
            {
                _volume = value;
                UpdateAudio();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>The transport's view.</summary>
    public TransportState Transport => new(StartMs, EndMs, PositionMs, State, Rate);

    /// <inheritdoc/>
    public bool CanPlay => Playback is not null;

    /// <summary>The cameras in the player, in order.</summary>
    public List<RecordingCamera> Cameras => [.. Cells.Select(c => c.Camera)];

    // ---- showing ----

    /// <summary>Plays an incident: its first four cameras over <paramref name="day"/>, from its start less its margin. With the same cameras and day already open, only seeks.</summary>
    public void Show(Incident incident, long day)
    {
        List<RecordingCamera> cams = PlayerGrid.Cameras(incident.Cameras.Select(c => c.Camera));
        long at = (incident.Start - incident.Margin) * 1000;
        Show(cams, day, at);
        IncidentKey = incident.Key;
        // Not retried by itself (every index event would ask again); a new selection does.
        FailedFor = Playback is null ? incident.Key : null;
    }

    /// <summary>The selected incident changed shape (a camera added or removed by an edit): the player follows its cameras without moving the playhead.</summary>
    public void Follow(Incident incident, long day)
    {
        List<RecordingCamera> want = PlayerGrid.Cameras(incident.Cameras.Select(c => c.Camera));
        if (Playback is null || Day != day)
        {
            Show(incident, day);
            return;
        }
        foreach (RecordingCamera c in Cameras.Where(c => !want.Contains(c)))
        {
            Remove(c);
        }
        foreach (RecordingCamera c in want.Where(c => !Cameras.Contains(c)))
        {
            Add(c);
        }
        IncidentKey = incident.Key;
    }

    /// <summary>The incident the player plays got a new key (a split kept the part at the playhead).</summary>
    public void Renamed(string key) => IncidentKey = key;

    /// <summary>Plays these cameras (at most four) over <paramref name="day"/> from <paramref name="atMs"/>.</summary>
    public void Show(IReadOnlyList<RecordingCamera> cameras, long day, long atMs)
    {
        IncidentKey = null;
        List<RecordingCamera> cams = PlayerGrid.Cameras(cameras);
        if (cams.Count == 0)
        {
            Close();
            return;
        }
        if (Playback is not null && Day == day && Cameras.SequenceEqual(cams))
        {
            Seek(atMs, exact: true);
            Play();
            return;
        }
        Close();
        Open(cams, day, atMs);
    }

    private void Open(List<RecordingCamera> cameras, long day, long atMs)
    {
        long start = day * 1000, end = (day + DeviceTime.Day) * 1000;
        long position = Math.Min(Math.Max(atMs, start), end);
        string request = PlaybackRequest.Range(start, end, cameras.Select(c => new PlaybackCamera(c)), position, Rate);
        try
        {
            OpenedPlayback opened = model.OpenPlayback(request, this);
            Opens++;
            Playback = opened.Playback;
            Cells = [.. cameras.Zip(opened.Tiles, (c, t) => new PlayerCell(c, t))];
            Views.Retain(cameras);
            Day = day;
            StartMs = start;
            EndMs = end;
            PositionMs = position;
            State = NvPlaybackState.Paused;
            Message = null;
            _mode = PlayerGrid.Normalized(_mode, Cells.Count);
            UpdateAudio();
            Play();
        }
        catch (NvException e)
        {
            Message = PlaybackOpenMessage.Text(e.Status);
        }
        Changed?.Invoke();
    }

    /// <summary>Closes the playback (every tile closes with it).</summary>
    public void Close()
    {
        if (Playback is { } p)
        {
            model.ClosePlayback(p, [.. Cells.Select(c => c.Tile)]);
        }
        Playback = null;
        Cells = [];
        State = null;
        Day = null;
        Changed?.Invoke();
    }

    private void ClosedByCore()
    {
        model.ForgetPlaybackTiles([.. Cells.Select(c => c.Tile)]);
        Playback = null;
        Cells = [];
        State = null;
        Day = null;
        Message = "The playback was closed";
        Changed?.Invoke();
    }

    // ---- transport ----

    /// <summary>Plays.</summary>
    public void Play()
    {
        if (Playback is { } p)
        {
            model.PlaybackCall(c => c.PlaybackPlay(p));
        }
    }

    /// <summary>Pauses.</summary>
    public void Pause()
    {
        if (Playback is { } p)
        {
            model.PlaybackCall(c => c.PlaybackPause(p));
        }
    }

    /// <summary>Plays or pauses.</summary>
    public void TogglePlay()
    {
        if (Transport.IsPlaying)
        {
            Pause();
        }
        else
        {
            Play();
        }
    }

    /// <summary>Seeks: <paramref name="exact"/> shows the picture at the time (a click or a release); otherwise the keyframe before it (dragging).</summary>
    public void Seek(long ms, bool exact = true)
    {
        if (Playback is not { } p)
        {
            return;
        }
        long target = Transport.Clamp(ms);
        PositionMs = target;
        Seeks++;
        model.PlaybackCall(c => c.PlaybackSeek(p, target, exact ? NvSeekMode.Exact : NvSeekMode.Keyframe));
        Changed?.Invoke();
    }

    /// <summary>Moves the playhead by <paramref name="deltaMs"/>.</summary>
    public void Skip(long deltaMs) => Seek(Transport.Skipped(deltaMs));

    /// <summary>Sets the speed.</summary>
    public void SetRate(double rate)
    {
        if (!PlaybackSpeed.All.Contains(rate) || rate == Rate)
        {
            return;
        }
        Rate = rate;
        UpdateAudio();
        if (Playback is { } p)
        {
            model.PlaybackCall(c => c.PlaybackSetRate(p, rate));
        }
        Changed?.Invoke();
    }

    /// <summary>Takes a camera out of the player (Remove from Incident does it too).</summary>
    public void Remove(RecordingCamera camera)
    {
        int i = Cells.FindIndex(c => c.Camera == camera);
        if (Playback is not { } p || i < 0)
        {
            return;
        }
        PlayerCell cell = Cells[i];
        Cells.RemoveAt(i);
        Views.Retain(Cameras);
        _mode = PlayerGrid.Normalized(_mode, Cells.Count);
        model.RemovePlaybackCamera(p, cell.Tile);
        if (Cells.Count == 0)
        {
            Close();
        }
        else
        {
            UpdateAudio();
            Changed?.Invoke();
        }
    }

    /// <summary>Adds a camera to the player (at most four), joining at the position.</summary>
    public void Add(RecordingCamera camera)
    {
        if (Playback is not { } p || Cells.Count >= PlayerGrid.MaxCameras || Cameras.Contains(camera))
        {
            return;
        }
        if (model.AddPlaybackCamera(p, camera) is { } tile)
        {
            Cells.Add(new PlayerCell(camera, tile));
            UpdateAudio();
            Changed?.Invoke();
        }
    }

    /// <summary>A click on camera <paramref name="i"/>.</summary>
    public void Click(int i) => Mode = PlayerGrid.Click(Mode, i, Cells.Count);

    /// <summary>A double click on camera <paramref name="i"/>.</summary>
    public void DoubleClick(int i) => Mode = PlayerGrid.DoubleClick(Mode, i, Cells.Count);

    // ---- sound ----

    // The playback's sound goes through the core's tile audio path, like a
    // live tile's: the large (or first) camera is audible unless muted; the
    // core plays recorded sound at 1x only.
    private void UpdateAudio()
    {
        int? audible = _muted ? null : PlayerGrid.AudibleIndex(_mode, Cells.Count);
        for (int i = 0; i < Cells.Count; i++)
        {
            model.SetPlaybackAudio(Cells[i].Tile, i == audible && PlaybackSpeed.HasSound(Rate), _volume);
        }
    }

    /// <summary>The camera whose sound plays, if any.</summary>
    public RecordingCamera? AudibleCamera =>
        !_muted && PlaybackSpeed.HasSound(Rate) && PlayerGrid.AudibleIndex(_mode, Cells.Count) is { } i ? Cells[i].Camera : null;

    // ---- the clock ----

    /// <summary>Reads the position and state now (the app calls it ten times a second; the read is lock-free and from memory).</summary>
    public void Tick()
    {
        if (Playback is not { } p || model.PlaybackPosition(p) is not { } now)
        {
            return;
        }
        if (now.PositionMs != PositionMs || now.State != State)
        {
            PositionMs = now.PositionMs;
            State = now.State;
            Changed?.Invoke();
        }
    }

    // ---- events ----

    /// <summary>Applies a playback event.</summary>
    public void Apply(PlaybackEvent e)
    {
        switch (e)
        {
            case PlaybackEvent.State s:
                State = s.Value;
                PositionMs = s.PositionMs;
                if (s.Rate != Rate && PlaybackSpeed.All.Contains(s.Rate))
                {
                    Rate = s.Rate;
                }
                Changed?.Invoke();
                break;
            case PlaybackEvent.Camera c:
                if (Cells.FirstOrDefault(x => x.Tile == c.Tile) is { } cell)
                {
                    cell.State = c.Value;
                    cell.Reason = c.Reason;
                    cell.Status = c.Status;
                    Changed?.Invoke();
                }
                break;
            case PlaybackEvent.Closed:
                ClosedByCore();
                break;
        }
    }
}
