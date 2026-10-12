// The Recordings window's follow cut and 2-up or 4-up grid (docs/UX.md
// "Recordings", "Follow and Grid"): the core's edit list over an incident's
// downloaded files (nv_edit_compute), the files decoded by Media Foundation
// and drawn by the window's D3D11 renderer (the app's IEditFiles), on the
// edit's own clock driven by the same transport as the device playback (play
// and pause, skip, speeds, scrubbing: keyframes while dragging and at 2x and
// up, the exact picture otherwise). The sound is the shown camera's in the
// follow cut and the chosen cell's (the first) in a grid, at 1x only, with the
// player's mute and volume. The same model as the macOS app's EditPlayer.

using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>What the player shows: the device playback of the incident's cameras, or, from its downloaded files, a follow cut or a 2-up or 4-up grid.</summary>
public enum PlayerView
{
    /// <summary>The device playback.</summary>
    Cameras,

    /// <summary>One picture that cuts to the camera with the action.</summary>
    Follow,

    /// <summary>Two cameras side by side.</summary>
    Grid2,

    /// <summary>Up to four cameras in a grid.</summary>
    Grid4,
}

/// <summary>The view menu's words.</summary>
public static class PlayerViews
{
    /// <summary>Every view, in menu order.</summary>
    public static IReadOnlyList<PlayerView> All { get; } = [PlayerView.Cameras, PlayerView.Follow, PlayerView.Grid2, PlayerView.Grid4];

    /// <summary>"Cameras", "Follow Cut", "2-Up", "4-Up".</summary>
    public static string Title(PlayerView view) => view switch
    {
        PlayerView.Follow => "Follow Cut",
        PlayerView.Grid2 => "2-Up",
        PlayerView.Grid4 => "4-Up",
        _ => "Cameras",
    };

    /// <summary>The words for a menu item (the device playback says so).</summary>
    public static string MenuTitle(PlayerView view) => view == PlayerView.Cameras ? "Cameras (Device Playback)" : Title(view);

    /// <summary>What the menu says while the incident has no finished download.</summary>
    public const string DownloadFirst = "Download the incident for a follow cut or a grid";

    /// <summary>The request body for <c>nv_edit_compute</c>, null for the cameras.</summary>
    public static string? Request(PlayerView view, string download)
    {
        if (view == PlayerView.Cameras)
        {
            return null;
        }
        var request = new JsonObject { ["kind"] = view == PlayerView.Follow ? "follow" : "grid", ["download"] = download };
        if (view != PlayerView.Follow)
        {
            request["layout"] = view == PlayerView.Grid2 ? "2up" : "4up";
            request["canvas"] = new JsonObject { ["width"] = 1920, ["height"] = 1080 };
        }
        return request.ToJsonString();
    }
}

/// <summary>The transport the Recordings window's controls drive: the device playback (<see cref="RecordingsPlayer"/>) or an edit (<see cref="EditPlayer"/>).</summary>
public interface IPlayerTransport
{
    /// <summary>Where it is, in which range, and what the buttons do.</summary>
    TransportState Transport { get; }

    /// <summary>The speed.</summary>
    double Rate { get; }

    /// <summary>Whether the sound is off.</summary>
    bool Muted { get; set; }

    /// <summary>0 to 1.</summary>
    float Volume { get; set; }

    /// <summary>Something is open to play.</summary>
    bool CanPlay { get; }

    /// <summary>Plays or pauses.</summary>
    void TogglePlay();

    /// <summary>Moves the playhead.</summary>
    void Skip(long deltaMs);

    /// <summary>Seeks; <paramref name="exact"/> shows the picture at the time, otherwise the keyframe before it (dragging).</summary>
    void Seek(long ms, bool exact = true);

    /// <summary>Sets the speed.</summary>
    void SetRate(double rate);
}

/// <summary>An edit's files, open for drawing (the app implements it with Media Foundation; tests with a made-up one).</summary>
public interface IEditFiles : IDisposable
{
    /// <summary>The plan the files belong to.</summary>
    EditPlan Plan { get; }

    /// <summary>Sources whose files could not be opened, with why; the cell draws black.</summary>
    IReadOnlyDictionary<int, string> Failures { get; }

    /// <summary>Whether any file is open.</summary>
    bool AnyOpen { get; }

    /// <summary>The path of a source's file, null when it is not open.</summary>
    string? FileOf(int source);

    /// <summary>Asks for the pictures of time <paramref name="t"/> (and the next camera's, when a cut is near); decoding happens off the caller's thread, the newest request winning.</summary>
    void Request(long t, bool keyframesOnly);

    /// <summary>The file position of the picture a source holds, null before its first.</summary>
    long? ShownPositionMs(int source);
}

/// <summary>Opens an edit's files.</summary>
public interface IEditFilesFactory
{
    /// <summary>Opens every file the plan shows; never throws for a file that cannot be read (see <see cref="IEditFiles.Failures"/>).</summary>
    Task<IEditFiles> OpenAsync(EditPlan plan, CancellationToken cancel);
}

/// <summary>The edit's sound: one player per source file, the audible one playing at the shown position, the others paused.</summary>
public interface IEditAudio : IDisposable
{
    /// <summary>The source that plays, if any.</summary>
    int? Active { get; }

    /// <summary>Plays <paramref name="source"/>'s file at <paramref name="filePositionMs"/> (kept near it), or nothing; a paused edit pauses the sound.</summary>
    void Update(int? source, string? file, long? filePositionMs, bool playing, float volume);

    /// <summary>Stops everything and lets the players go.</summary>
    void Close();
}

/// <summary>The edit player (see the file comment).</summary>
public sealed class EditPlayer : IPlayerTransport
{
    private readonly RecordingsService _model;
    private readonly IPlayerTransport _cameras;
    private readonly IEditFilesFactory _factory;
    private readonly IEditAudio _audio;
    private CancellationTokenSource? _opening;
    private volatile EditClock _clock = new(0, 0);
    private int _gridSound;

    /// <param name="model">The recordings model (it makes the plans).</param>
    /// <param name="cameras">The device playback's player, whose mute and volume this shares.</param>
    /// <param name="factory">Opens the files.</param>
    /// <param name="audio">The edit's sound.</param>
    public EditPlayer(RecordingsService model, IPlayerTransport cameras, IEditFilesFactory factory, IEditAudio audio)
    {
        _model = model;
        _cameras = cameras;
        _factory = factory;
        _audio = audio;
    }

    /// <summary>Raised on the UI thread when anything the edit shows changed.</summary>
    public event Action? Changed;

    /// <summary>The host clock in seconds; the self-test sets a simulated one.</summary>
    public Func<double> Now { get; set; } = () => System.Diagnostics.Stopwatch.GetTimestamp() / (double)System.Diagnostics.Stopwatch.Frequency;

    /// <summary>The view shown, null while the cameras show.</summary>
    public PlayerView? View { get; private set; }

    /// <summary>The download the edit is made of.</summary>
    public string? Download { get; private set; }

    /// <summary>The plan.</summary>
    public EditPlan? Plan { get; private set; }

    /// <summary>The open files.</summary>
    public IEditFiles? Files { get; private set; }

    /// <summary>Opening (computing the plan, opening the files).</summary>
    public bool Loading { get; private set; }

    /// <summary>Why nothing is shown, if so.</summary>
    public string? Message { get; private set; }

    /// <summary>Where the playhead is.</summary>
    public long PositionMs { get; private set; }

    /// <summary>The transport's state, null while nothing is open.</summary>
    public NvPlaybackState? State { get; private set; }

    /// <inheritdoc/>
    public double Rate { get; private set; } = 1;

    /// <summary>Plans opened (the self-test).</summary>
    public int Opens { get; private set; }

    /// <summary>Dragging the scrubber: keyframes only until the release.</summary>
    public bool Scrubbing { get; set; }

    /// <summary>The grid cell whose sound plays.</summary>
    public int GridSound
    {
        get => _gridSound;
        set
        {
            _gridSound = value;
            UpdateAudio();
            Changed?.Invoke();
        }
    }

    /// <summary>The edit is open (or opening).</summary>
    public bool IsActive => View is not null;

    /// <inheritdoc/>
    public bool CanPlay => Files is not null;

    /// <inheritdoc/>
    public bool Muted
    {
        get => _cameras.Muted;
        set
        {
            _cameras.Muted = value;
            UpdateAudio();
        }
    }

    /// <inheritdoc/>
    public float Volume
    {
        get => _cameras.Volume;
        set
        {
            _cameras.Volume = value;
            UpdateAudio();
        }
    }

    /// <inheritdoc/>
    public TransportState Transport => new(_clock.StartMs, _clock.EndMs, PositionMs, State, Rate);

    /// <summary>Where the follow cut changes camera (scrubber marks).</summary>
    public List<long> Cuts => Plan is { } p ? EditTimeline.Cuts(p) : [];

    /// <summary>The source whose sound plays now.</summary>
    public int? AudibleSource => Plan is { } p ? EditTimeline.AudibleSource(p, PositionMs, Rate, Muted, _gridSound) : null;

    /// <summary>The source whose sound the audio plays (the self-test).</summary>
    public int? PlayingSoundSource => _audio.Active;

    /// <summary>Pictures decode as keyframes only (dragging, 2x and up).</summary>
    public bool KeyframesOnly => Scrubbing || PlaybackSpeed.KeyframesOnly(Rate);

    /// <summary>The position to draw now (the renderer asks once per frame).</summary>
    public long FramePosition() => _clock.Position(Now());

    // ---- opening ----

    /// <summary>Shows <paramref name="view"/> over a finished download, from <paramref name="at"/> (kept within the plan's range; its start when null), and plays.</summary>
    public void Open(PlayerView view, string download, long? at)
    {
        Close();
        string request = PlayerViews.Request(view, download) ?? throw new ArgumentException("not an edit", nameof(view));
        View = view;
        Download = download;
        Loading = true;
        Message = null;
        (EditPlan? plan, string? refusal) = _model.ComputeEdit(request);
        if (plan is null)
        {
            Loading = false;
            Message = refusal;
            Changed?.Invoke();
            return;
        }
        Plan = plan;
        Opens++;
        _opening = new CancellationTokenSource();
        _ = OpenFiles(plan, view, at, _opening.Token);
        Changed?.Invoke();
    }

    private async Task OpenFiles(EditPlan plan, PlayerView view, long? at, CancellationToken cancel)
    {
        IEditFiles files;
        try
        {
            files = await _factory.OpenAsync(plan, cancel);
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (cancel.IsCancellationRequested || !ReferenceEquals(Plan, plan) || View != view)
        {
            files.Dispose();
            return;
        }
        Files = files;
        Loading = false;
        if (!files.AnyOpen)
        {
            Message = "The downloaded files could not be opened";
        }
        _clock = new EditClock(plan.StartMs, plan.EndMs, at);
        _clock.SetRate(Rate, Now());
        Play();
    }

    /// <summary>Closes the edit (its files and sound).</summary>
    public void Close()
    {
        _opening?.Cancel();
        _opening?.Dispose();
        _opening = null;
        Files?.Dispose();
        Files = null;
        _audio.Close();
        Plan = null;
        View = null;
        Download = null;
        Loading = false;
        State = null;
        Message = null;
        Scrubbing = false;
        Changed?.Invoke();
    }

    // ---- transport ----

    /// <summary>Plays.</summary>
    public void Play()
    {
        if (Files is null)
        {
            return;
        }
        _clock.Play(Now());
        Tick();
    }

    /// <summary>Pauses.</summary>
    public void Pause()
    {
        _clock.Pause(Now());
        Tick();
    }

    /// <inheritdoc/>
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

    /// <inheritdoc/>
    public void Seek(long ms, bool exact = true)
    {
        Scrubbing = !exact;
        _clock.Seek(ms, Now());
        Tick();
    }

    /// <inheritdoc/>
    public void Skip(long deltaMs) => Seek(Transport.Skipped(deltaMs));

    /// <inheritdoc/>
    public void SetRate(double rate)
    {
        if (!PlaybackSpeed.All.Contains(rate) || rate == Rate)
        {
            return;
        }
        Rate = rate;
        _clock.SetRate(rate, Now());
        Tick();
    }

    // ---- the clock ----

    /// <summary>Reads the clock (ten times a second, and after every transport call).</summary>
    public void Tick()
    {
        double now = Now();
        long p = _clock.Position(now);
        NvPlaybackState? s = Files is null ? null : _clock.State(now);
        bool changed = p != PositionMs || s != State;
        PositionMs = p;
        State = s;
        UpdateAudio();
        if (changed)
        {
            Changed?.Invoke();
        }
    }

    private void UpdateAudio()
    {
        if (Plan is not { } plan || Files is not { } files)
        {
            _audio.Close();
            return;
        }
        int? source = AudibleSource;
        long? position = source is { } s
            ? plan.Kind == EditKind.Follow
                ? EditTimeline.Shot(plan, PositionMs)?.FilePositionMs
                : plan.SourceOf(s) is { } src ? EditTimeline.FilePosition(src, PositionMs) : null
            : null;
        _audio.Update(source, source is { } k ? files.FileOf(k) : null, position, State == NvPlaybackState.Playing, Volume);
    }
}

/// <summary>No files can be opened (a window made without a renderer).</summary>
internal sealed class NoEditFiles : IEditFilesFactory
{
    public Task<IEditFiles> OpenAsync(EditPlan plan, CancellationToken cancel) => Task.FromResult<IEditFiles>(new Closed(plan));

    private sealed class Closed(EditPlan plan) : IEditFiles
    {
        public EditPlan Plan { get; } = plan;

        public IReadOnlyDictionary<int, string> Failures { get; } = new Dictionary<int, string>();

        public bool AnyOpen => false;

        public string? FileOf(int source) => null;

        public void Request(long t, bool keyframesOnly)
        {
        }

        public long? ShownPositionMs(int source) => null;

        public void Dispose()
        {
        }
    }
}

/// <summary>No sound.</summary>
internal sealed class NoEditAudio : IEditAudio
{
    public int? Active { get; private set; }

    public void Update(int? source, string? file, long? filePositionMs, bool playing, float volume) => Active = playing ? source : null;

    public void Close() => Active = null;

    public void Dispose() => Active = null;
}
