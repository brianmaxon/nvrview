// Export from the Recordings window (docs/UX.md "Recordings", "Export"): in
// and out points, trims, a follow cut or grid as an MP4, range downloads, and
// still pictures. The core plans (FFI-RECORDINGS.md section 7a); the writers
// (IExportWriters, NvrView.Video and the app) only carry the plans out. Every
// export asks where to save (IExportDialogs) and remembers the folder. The
// same behavior as the macOS app's RecordingsExport.swift.

using System.Text.Json.Nodes;
using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>The video codecs a follow cut or grid is written in.</summary>
public enum ExportCodec
{
    H264,
    Hevc,
}

/// <summary>Asks the user where an export goes (a save picker, a folder picker); the self-test answers these itself.</summary>
public interface IExportDialogs
{
    /// <summary>Asks for one file; null when the user cancels.</summary>
    Task<string?> SaveFile(string suggestedName, string? folder, string extension);

    /// <summary>Asks for a folder; null when the user cancels.</summary>
    Task<string?> ChooseFolder(string message, string? folder);
}

/// <summary>
/// What the platform does for exports (NvrView.Video, the app). Called on a worker thread, one export at a
/// time. MP4 writers write to the partial file they are given and never to the destination.
/// </summary>
public interface IExportWriters
{
    /// <summary>Cuts one camera's downloaded file to the trim: a keyframe copy, or (exact) re-encoded to start at the picture asked for.</summary>
    ExportOutcome Trim(TrimOutput output, bool exact, string partialPath, ExportControl control);

    /// <summary>Saves the picture at a position of a downloaded file as PNG or JPEG exactly at <paramref name="path"/>.</summary>
    ExportOutcome StillFromFile(string file, long sourceMs, bool exact, PictureFormat format, string path);

    /// <summary>Saves the picture a playback's tile shows now as PNG or JPEG exactly at <paramref name="path"/>.</summary>
    ExportOutcome StillFromTile(ulong tile, PictureFormat format, string path);

    /// <summary>
    /// Whether follow cuts and grids can be exported: true when a renderer for the edit list is present (the follow
    /// and grid views draw them offscreen).
    /// </summary>
    bool CanExportEdit { get; }

    /// <summary>Renders the edit list <paramref name="planJson"/> frame by frame into the MP4 at <paramref name="partialPath"/>.</summary>
    ExportOutcome ExportEdit(string planJson, ExportCodec codec, string partialPath, ExportControl control, bool withSound = true);
}

/// <summary>One export running (or just ended), for the strip above the transport.</summary>
public sealed class ExportJob
{
    private readonly TaskCompletionSource _done = new(TaskCreationOptions.RunContinuationsAsynchronously);

    public ExportJob(string title) => Title = title;

    public string Title { get; }

    public ExportControl Control { get; } = new();

    /// <summary>0 to 1.</summary>
    public double Fraction => Running ? Control.Fraction : (Files.Count > 0 ? 1 : Control.Fraction);

    public bool Running { get; private set; } = true;

    /// <summary>What happened, once it ended.</summary>
    public string? Result { get; private set; }

    /// <summary>The files written.</summary>
    public IReadOnlyList<string> Files { get; private set; } = [];

    /// <summary>Completes when the export ended (any way).</summary>
    public Task Completion => _done.Task;

    public void Cancel() => Control.Cancel();

    internal void Finish(IReadOnlyList<ExportOutcome> outcomes)
    {
        Running = false;
        Files = [.. outcomes.Where(o => o.Result == ExportResult.Done).Select(o => o.Path)];
        if (outcomes.FirstOrDefault(o => o.Result == ExportResult.Failed) is { } failed)
        {
            Result = failed.Reason ?? "The export failed";
        }
        else if (outcomes.Any(o => o.Result == ExportResult.Canceled))
        {
            Result = "Export canceled";
        }
        else
        {
            Result = Files.Count == 1 ? $"Saved {Path.GetFileName(Files[0])}" : $"Saved {Files.Count} files";
        }
        _done.TrySetResult();
    }
}

/// <summary>A range download being set up: the Export tab's range and checked cameras.</summary>
public sealed class RangeDownloadRequest
{
    public long StartMs { get; set; }

    public long EndMs { get; set; }

    public HashSet<RecordingCamera> Cameras { get; set; } = [];

    public string Name { get; set; } = "";
}

/// <summary>What export keeps between launches: the share-safe switch, the trim kind, the codec, and the last folder.</summary>
public sealed class ExportSettings(string? path)
{
    public bool ShareSafe { get; set; }

    /// <summary>Trim by re-encoding to start at the exact picture, instead of copying from the keyframe.</summary>
    public bool TrimExact { get; set; }

    public ExportCodec Codec { get; set; } = ExportCodec.H264;

    /// <summary>The folder the last export went to.</summary>
    public string? Folder { get; set; }

    /// <summary>Exports go straight into <see cref="Folder"/> with no save picker (the Export tab's "Choose…"; "Ask Each Time" clears it).</summary>
    public bool Direct { get; set; }

    /// <summary>Reads the file (defaults when it is missing or unreadable).</summary>
    public void Load()
    {
        if (path is null || JsonFile.Read(path) is not JsonObject o)
        {
            return;
        }
        ShareSafe = o["share_safe"] is JsonValue s && s.TryGetValue(out bool b) && b;
        TrimExact = o["trim_exact"] is JsonValue t && t.TryGetValue(out bool x) && x;
        Codec = o["codec"] is JsonValue c && c.TryGetValue(out string? name) && name == "hevc" ? ExportCodec.Hevc : ExportCodec.H264;
        Folder = o["folder"] is JsonValue f && f.TryGetValue(out string? dir) && !string.IsNullOrEmpty(dir) ? dir : null;
        Direct = o["direct"] is JsonValue d && d.TryGetValue(out bool direct) && direct;
    }

    public void Save()
    {
        if (path is null)
        {
            return;
        }
        var o = new JsonObject { ["share_safe"] = ShareSafe, ["trim_exact"] = TrimExact, ["codec"] = Codec == ExportCodec.Hevc ? "hevc" : "h264" };
        if (Folder is not null)
        {
            o["folder"] = Folder;
        }
        if (Direct)
        {
            o["direct"] = true;
        }
        try
        {
            JsonFile.Write(path, o.ToJsonString(JsonFile.Pretty));
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
    }
}

/// <summary>The window's export: points, switches, the job, and the commands the menu runs.</summary>
public sealed class ExportController
{
    private readonly RecordingsBrowser _browser;
    private readonly ExportSettings _settings;
    private InOutPoints _points;

    public ExportController(RecordingsBrowser browser, ExportSettings? settings = null)
    {
        _browser = browser;
        _settings = settings ?? new ExportSettings(null);
    }

    /// <summary>Raised on the UI thread when the points, the job, the sheet, or a switch changed.</summary>
    public event Action? Changed;

    /// <summary>The platform's writers (null: exporting is not available, which the commands say).</summary>
    public IExportWriters? Writers { get; set; }

    /// <summary>Where the pickers are (the self-test answers these itself).</summary>
    public IExportDialogs? Dialogs { get; set; }

    public ExportJob? Job { get; private set; }

    public bool Busy => Job?.Running == true;

    public PictureFormat StillFormat { get; set; } = PictureFormat.Png;

    private void Notify() => Changed?.Invoke();

    // ---- switches ----

    public bool ShareSafe
    {
        get => _settings.ShareSafe;
        set
        {
            _settings.ShareSafe = value;
            _settings.Save();
            Notify();
        }
    }

    public bool TrimExact
    {
        get => _settings.TrimExact;
        set
        {
            _settings.TrimExact = value;
            _settings.Save();
            Notify();
        }
    }

    public ExportCodec Codec
    {
        get => _settings.Codec;
        set
        {
            _settings.Codec = value;
            _settings.Save();
            Notify();
        }
    }

    /// <summary>Exports go straight into <see cref="Folder"/> with no picker (the tab's "Choose…"); "Ask Each Time" turns it off.</summary>
    public bool Direct => _settings.Direct && Folder is not null;

    /// <summary>The folder exports go straight into, or null when the pickers ask.</summary>
    public string? DirectFolder => Direct ? Folder : null;

    /// <summary>"Choose…" in the tab: exports go straight into <paramref name="folder"/> from now on.</summary>
    public void ChooseFolder(string folder)
    {
        _settings.Folder = folder;
        _settings.Direct = true;
        _settings.Save();
        Notify();
    }

    /// <summary>"Ask Each Time": the pickers come back.</summary>
    public void AskEachTime()
    {
        _settings.Direct = false;
        _settings.Save();
        Notify();
    }

    /// <summary>The folder the last export went to, if it still exists.</summary>
    public string? Folder => _settings.Folder is { } f && Directory.Exists(f) ? f : null;

    private void Remember(string folder)
    {
        _settings.Folder = folder;
        _settings.Save();
    }

    /// <summary>The options every request carries: the share-safe switch and (when asked) the remembered folder, which the core checks.</summary>
    public ExportOptions Options(bool useFolder = true) => new(useFolder ? Folder : null, ShareSafe);

    // ---- in and out points ----

    /// <summary>The scrubber's range, if it spans anything.</summary>
    public (long Lo, long Hi)? ScrubSpan => _browser.ScrubRange is { } r && r.End > r.Start ? (r.Start, r.End) : null;

    /// <summary>The points as set (the window shows only those inside the scrubber's range, <see cref="Points"/>).</summary>
    public InOutPoints RawPoints
    {
        get => _points;
        set
        {
            _points = value;
            Notify();
        }
    }

    /// <summary>The points inside the scrubber's range.</summary>
    public InOutPoints Points => ScrubSpan is { } s ? _points.Kept(s.Lo, s.Hi) : default;

    /// <summary>The span the points select, null when none is set or it is too short.</summary>
    public (long Start, long End)? PointsWindow => ScrubSpan is { } s && !Points.IsEmpty ? Points.Window(s.Lo, s.Hi) : null;

    public void MarkIn()
    {
        if (ScrubSpan is { } s)
        {
            RawPoints = Points.SetIn(_browser.Driver.Transport.PositionMs, s.Lo, s.Hi);
        }
    }

    public void MarkOut()
    {
        if (ScrubSpan is { } s)
        {
            RawPoints = Points.SetOut(_browser.Driver.Transport.PositionMs, s.Lo, s.Hi);
        }
    }

    public void ClearPoints() => RawPoints = default;

    /// <summary>A press or drag on the scrubber moves a point.</summary>
    public void MovePoint(PointHandle handle, long ms)
    {
        if (ScrubSpan is { } s)
        {
            RawPoints = Points.Move(handle, ms, s.Lo, s.Hi);
        }
    }

    // ---- the Export tab ----

    /// <summary>The kind the user chose in the tab (null: the first that applies, <see cref="Kind"/>).</summary>
    public ExportKind? KindChoice
    {
        get => _kindChoice;
        set
        {
            _kindChoice = value;
            Notify();
        }
    }

    private ExportKind? _kindChoice;

    /// <summary>The cameras checked in the tab (null: all of the source's).</summary>
    public HashSet<RecordingCamera>? CameraChoice
    {
        get => _cameraChoice;
        set
        {
            _cameraChoice = value;
            Notify();
        }
    }

    private HashSet<RecordingCamera>? _cameraChoice;

    /// <summary>The stream the tab chose for this export (null: Settings' choice).</summary>
    public ExportQuality? QualityChoice
    {
        get => _qualityChoice;
        set
        {
            _qualityChoice = value;
            Notify();
        }
    }

    private ExportQuality? _qualityChoice;

    /// <summary>Settings > Capture's "Export from" (the app sets it).</summary>
    public Func<ExportQuality> DefaultQuality { get; set; } = () => ExportQuality.Main;

    /// <summary>The stream a download, and the download an export makes first, reads.</summary>
    public ExportQuality Quality => QualityChoice ?? DefaultQuality();

    /// <summary>Whether a follow cut or grid keeps the sound.</summary>
    public bool Sound
    {
        get => _sound;
        set
        {
            _sound = value;
            Notify();
        }
    }

    private bool _sound = true;

    /// <summary>The cameras the range has: the ones playing, or the follow cut's or grid's.</summary>
    public IReadOnlyList<RecordingCamera> SourceCameras
    {
        get
        {
            List<RecordingCamera> playing = [.. _browser.Player.Cameras];
            if (_browser.PlayerView == PlayerView.Cameras)
            {
                return playing;
            }
            // A follow cut may name fewer cameras than the incident has; the playback still holds all of them.
            List<RecordingCamera> edit = [.. _browser.EditPlayer.Plan?.Sources.Select(x => x.Camera) ?? []];
            List<RecordingCamera> incident = [.. _browser.Editable?.Incident.Cameras.Select(x => x.Camera) ?? []];
            return new[] { edit, playing, incident }.MaxBy(l => l.Count)!;
        }
    }

    /// <summary>The cameras checked: the source's, unless the user unchecked some.</summary>
    public HashSet<RecordingCamera> Checked
    {
        get
        {
            IReadOnlyList<RecordingCamera> source = SourceCameras;
            return CameraChoice is { } chosen ? [.. chosen.Where(source.Contains)] : [.. source];
        }
    }

    /// <summary>The checked cameras in the source's order.</summary>
    public List<RecordingCamera> CheckedList
    {
        get
        {
            HashSet<RecordingCamera> set = Checked;
            return [.. SourceCameras.Where(set.Contains)];
        }
    }

    /// <summary>Checks or unchecks one camera.</summary>
    public void SetCamera(RecordingCamera camera, bool isChecked)
    {
        HashSet<RecordingCamera> set = Checked;
        if (isChecked)
        {
            set.Add(camera);
        }
        else
        {
            set.Remove(camera);
        }
        CameraChoice = set;
    }

    /// <summary>What is open, for the rules (<see cref="ExportPanel"/>).</summary>
    public ExportContext Context => new(
        HasRange: ScrubSpan is not null,
        HasPictures: _browser.Player.Playback is not null && _browser.Player.Cells.Count > 0,
        Editable: _browser.Editable is not null,
        CanWriteEdit: Writers?.CanExportEdit == true,
        SourceCameras: SourceCameras.Count,
        CheckedCameras: CheckedList.Count);

    /// <summary>The kinds that apply now.</summary>
    public IReadOnlyList<ExportKind> Kinds => ExportPanel.Kinds(Context);

    /// <summary>The kind shown: the user's choice while it applies, else the first that does.</summary>
    public ExportKind? Kind => ExportPanel.DefaultKind(Kinds, KindChoice);

    /// <summary>The range to export: the points', else the whole range of the incident (or day) shown.</summary>
    public (long Start, long End)? Window => PointsWindow ?? (ScrubSpan is { } s ? (s.Lo, s.Hi) : null);

    /// <summary>The range's start as the tab shows it.</summary>
    public string StartText => Window is { } w ? ExportPanel.TimeText(w.Start) : "";

    /// <summary>The range's end as the tab shows it.</summary>
    public string EndText => Window is { } w ? ExportPanel.TimeText(w.End) : "";

    private long? TypedTime(string text) =>
        Window is { } w ? ExportPanel.ParseTime(text, DeviceTime.DayStart(DeviceTime.FloorDiv(w.Start, 1000)) * 1000) : null;

    /// <summary>A start time typed in the tab; false when the text is not a time inside the range.</summary>
    public bool SetStart(string text)
    {
        if (ScrubSpan is not { } s || TypedTime(text) is not long ms || ms < s.Lo || ms > s.Hi)
        {
            return false;
        }
        RawPoints = Points.SetIn(ms, s.Lo, s.Hi);
        return Points.InMs == ms;
    }

    /// <summary>An end time typed in the tab; false when the text is not a time inside the range.</summary>
    public bool SetEnd(string text)
    {
        if (ScrubSpan is not { } s || TypedTime(text) is not long ms || ms < s.Lo || ms > s.Hi)
        {
            return false;
        }
        RawPoints = Points.SetOut(ms, s.Lo, s.Hi);
        return Points.OutMs == ms;
    }

    /// <summary>The size the core's spans add up to for the checked cameras and the stream chosen.</summary>
    private long? Estimate((long Start, long End) window)
    {
        string tier = Quality.Tier();
        HashSet<RecordingCamera> cameras = Checked;
        long? total = null;
        foreach (TimelineLane lane in _browser.Lanes.Where(l => cameras.Contains(l.Camera)))
        {
            var spans = lane.Spans.Where(s => s.Tier is null || s.Tier == tier)
                .Select(s => (s.Start, s.End, Bytes: s.Bytes ?? 0)).ToList();
            if (ExportPanel.Estimate(spans, DeviceTime.FloorDiv(window.Start, 1000), DeviceTime.FloorDiv(window.End, 1000)) is long bytes)
            {
                total = (total ?? 0) + bytes;
            }
        }
        return total;
    }

    /// <summary>What the tab says is about to be written.</summary>
    public string Summary =>
        Kind is { } kind ? ExportPanel.Summary(kind, (Window?.End ?? 0) - (Window?.Start ?? 0), CheckedList.Count, Window is { } w ? Estimate(w) : null) : "";

    /// <summary>Why the Export button would not start anything, or null.</summary>
    public string? Problem => Kind is { } kind
        ? ExportPanel.Problem(kind, Window?.Start ?? 0, Window?.End ?? 0, CheckedList.Count, Busy)
        : "Play an incident or a camera to export from it";

    /// <summary>The Export button: checks the choice, then runs the kind's command.</summary>
    public async Task StartAsync()
    {
        if (Kind is not { } kind || Problem is { } problem)
        {
            Say(Problem ?? "Play an incident or a camera to export from it");
            return;
        }
        (long Start, long End) window = Window ?? (0, 0);
        bool all = CheckedList.Count == SourceCameras.Count;
        switch (kind)
        {
            case ExportKind.TrimCopy or ExportKind.TrimExact:
                TrimExact = kind == ExportKind.TrimExact;
                await TrimAsync(all ? null : CheckedList);
                break;
            case ExportKind.FollowCut:
                await ExportEditAsync(null);
                break;
            case ExportKind.Grid2:
                await ExportEditAsync("2up");
                break;
            case ExportKind.Grid4:
                await ExportEditAsync("4up");
                break;
            case ExportKind.Download:
                DownloadRange(new RangeDownloadRequest { StartMs = window.Start, EndMs = window.End, Cameras = Checked });
                break;
            case ExportKind.StillCurrent:
                await SaveStillsAsync(false);
                break;
            default:
                await SaveStillsAsync(true);
                break;
        }
    }

    // ---- destinations and running ----

    private void Say(string text) => _browser.Message = text;

    private bool Ready(out IExportWriters writers, out IExportDialogs dialogs)
    {
        writers = Writers!;
        dialogs = Dialogs!;
        if (Writers is null || Dialogs is null)
        {
            Say("Exporting is not available here");
            return false;
        }
        if (Busy)
        {
            Say("Wait for the export in progress, or cancel it");
            return false;
        }
        return true;
    }

    /// <summary>
    /// Where an export goes: one file by a save picker, several by a folder picker (names from
    /// <paramref name="suggested"/>); the folder is remembered. Names already taken in a chosen folder get " (2)".
    /// Null when the user cancels.
    /// </summary>
    public async Task<List<string>?> Destinations(IReadOnlyList<string> suggested, string extension, string message)
    {
        if (suggested.Count == 0)
        {
            return null;
        }
        if (DirectFolder is { } direct)
        {
            var used = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var chosen = new List<string>();
            foreach (string s in suggested)
            {
                string free = ExportFile.Free(Path.Combine(direct, Path.GetFileName(s)), taken: used);
                used.Add(Path.GetFileName(free));
                chosen.Add(free);
            }
            return chosen;
        }
        if (Dialogs is null)
        {
            return null;
        }
        string? start = Folder ?? Path.GetDirectoryName(suggested[0]);
        if (start is not null && !Directory.Exists(start))
        {
            start = null;
        }
        if (suggested.Count == 1)
        {
            string? chosen = await Dialogs.SaveFile(Path.GetFileName(suggested[0]), start, extension);
            if (chosen is null)
            {
                return null;
            }
            if (Path.GetDirectoryName(chosen) is { Length: > 0 } dir)
            {
                Remember(dir);
            }
            return [chosen];
        }
        string? folder = await Dialogs.ChooseFolder(message, start);
        if (folder is null)
        {
            return null;
        }
        Remember(folder);
        var taken = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var paths = new List<string>();
        foreach (string s in suggested)
        {
            string free = ExportFile.Free(Path.Combine(folder, Path.GetFileName(s)), taken: taken);
            taken.Add(Path.GetFileName(free));
            paths.Add(free);
        }
        return paths;
    }

    /// <summary>Runs <paramref name="work"/> as the window's export, off the calling thread, with a progress strip and a cancel.</summary>
    private void Run(string title, Func<ExportControl, List<ExportOutcome>> work)
    {
        var job = new ExportJob(title);
        Job = job;
        Notify();
        _ = Task.Run(() =>
        {
            List<ExportOutcome> outcomes;
            try
            {
                outcomes = work(job.Control);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException or InvalidOperationException or NvException)
            {
                outcomes = [ExportOutcome.Failed("", $"The export failed ({e.GetType().Name})")];
            }
            _browser.Model.Post(() =>
            {
                job.Finish(outcomes);
                if (job.Result is { } result)
                {
                    _browser.Model.Notice = result;
                }
                Notify();
            });
        });
    }

    /// <summary>Takes the finished job's strip away.</summary>
    public void DismissJob()
    {
        if (Job?.Running == false)
        {
            Job = null;
            Notify();
        }
    }

    // ---- trim ----

    /// <summary>
    /// Trims the cameras (all of the incident's when null) to the in and out points: from the incident's download when
    /// it has one, otherwise from a range download of just that span, made first.
    /// </summary>
    public async Task TrimAsync(IReadOnlyList<RecordingCamera>? chosen)
    {
        if ((PointsWindow ?? (ScrubSpan is { } span ? (span.Lo, span.Hi) : null)) is not { } window)
        {
            Say("Set an in point and an out point first (I and O)");
            return;
        }
        if (!Ready(out _, out _))
        {
            return;
        }
        Incident? playing = _browser.PlayingIncident ?? _browser.Selected;
        if (playing is not null && Model.FinishedDownload(playing.Key) is { } finished)
        {
            await StartTrim(finished.Download, chosen, window);
            return;
        }
        List<RecordingCamera> cameras = chosen is null ? _browser.Player.Cameras : [.. chosen];
        if (cameras.Count == 0)
        {
            Say("Play an incident or a camera to trim it");
            return;
        }
        if (Model.DownloadRange(cameras, window.Start, window.End, null, new ExportOptions(null, ShareSafe), Quality) is not { } queued)
        {
            return;
        }
        Say("Downloading that range first; the trim follows when it is done");
        Model.WhenFinished(queued.Download, done =>
        {
            if (done.State == "done")
            {
                _browser.Message = null;
                _ = StartTrim(done.Download, chosen, window);
            }
            else
            {
                Say("The range download failed, so there is nothing to trim");
            }
        });
    }

    private RecordingsService Model => _browser.Model;

    /// <summary>Plans the trim with the core and writes it where the user says.</summary>
    public async Task StartTrim(string download, IReadOnlyList<RecordingCamera>? cameras, (long Start, long End) window)
    {
        if (!Ready(out IExportWriters writers, out _))
        {
            return;
        }
        Planned<TrimPlan> Plan(ExportOptions o) => Model.PlanTrim(ExportRequest.Trim(download, cameras, window.Start, window.End, TrimExact, o));
        Planned<TrimPlan> planned = Plan(Options());
        if (planned.Plan is null && Options().Dest is not null)
        {
            planned = Plan(Options(useFolder: false));
        }
        if (planned.Plan is not { } plan)
        {
            Say(planned.Refusal!.Text);
            return;
        }
        List<string>? paths = await Destinations([.. plan.Outputs.Select(o => o.SuggestedFile)], "mp4", "Choose where to save the trimmed files");
        if (paths is null || paths.Count != plan.Outputs.Count)
        {
            return;
        }
        bool exact = TrimExact, shareSafe = ShareSafe;
        var jobs = plan.Outputs.Zip(paths).ToList();
        Run(jobs.Count == 1 ? "Trimming" : $"Trimming {jobs.Count} cameras", control =>
        {
            var outcomes = new List<ExportOutcome>();
            for (int i = 0; i < jobs.Count; i++)
            {
                (TrimOutput output, string path) = jobs[i];
                var one = new ExportControl();
                string partial = ExportFile.Partial(path);
                ExportOutcome outcome;
                using (new Watch(control, one, i, jobs.Count))
                {
                    outcome = writers.Trim(output, exact, partial, one);
                }
                outcomes.Add(Commit(outcome, partial, path, shareSafe));
                if (outcomes[^1].Result != ExportResult.Done)
                {
                    break;
                }
            }
            return outcomes;
        });
    }

    // Moves a finished partial file into place (share-safe if asked); discards it otherwise.
    private static ExportOutcome Commit(ExportOutcome outcome, string partial, string path, bool shareSafe)
    {
        if (outcome.Result != ExportResult.Done)
        {
            ExportFile.Discard(partial);
            return outcome with { Path = path };
        }
        try
        {
            long bytes = ExportFile.Commit(partial, path, shareSafe);
            return outcome with { Path = path, Bytes = bytes };
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            ExportFile.Discard(partial);
            return ExportOutcome.Failed(path, "The file could not be written");
        }
    }

    // Forwards the job's cancel to one writer and the writer's progress to the job (a share of n).
    private sealed class Watch : IDisposable
    {
        private readonly Timer _timer;

        public Watch(ExportControl job, ExportControl one, int index, int count)
        {
            _timer = new Timer(_ =>
            {
                if (job.IsCanceled)
                {
                    one.Cancel();
                }
                job.Fraction = (index + one.Fraction) / count;
            }, null, 0, 50);
        }

        public void Dispose() => _timer.Dispose();
    }

    // ---- follow cut and grid ----

    /// <summary>
    /// Writes the follow cut or a grid ("2up", "4up") of the incident's downloaded files, cut to the points when
    /// they are set, as an MP4.
    /// </summary>
    public async Task ExportEditAsync(string? gridLayout)
    {
        if (!Ready(out IExportWriters writers, out _))
        {
            return;
        }
        if (!writers.CanExportEdit)
        {
            Say("Follow cut and grid export need the Follow and Grid views");
            return;
        }
        Incident? incident = _browser.PlayingIncident ?? _browser.Selected;
        if (incident is null || Model.FinishedDownload(incident.Key) is not { } download)
        {
            Say("Download the incident to export a follow cut or a grid");
            return;
        }
        (long Start, long End)? window = PointsWindow;
        Planned<EditExportPlanJson> Plan(ExportOptions o) => Model.PlanEdit(ExportRequest.Edit(gridLayout, download.Download, window, o));
        Planned<EditExportPlanJson> planned = Plan(Options());
        if (planned.Plan is null && Options().Dest is not null)
        {
            planned = Plan(Options(useFolder: false));
        }
        if (planned.Plan is not { } edit)
        {
            Say(planned.Refusal!.Text);
            return;
        }
        List<string>? paths = await Destinations([edit.Plan.SuggestedFile], "mp4", "Choose where to save the video");
        if (paths is not { Count: 1 })
        {
            return;
        }
        string path = paths[0], partial = ExportFile.Partial(path);
        ExportCodec codec = Codec;
        bool shareSafe = ShareSafe, sound = Sound;
        Run(gridLayout is null ? "Exporting the follow cut" : $"Exporting the {gridLayout} grid", control =>
            [Commit(writers.ExportEdit(edit.Json, codec, partial, control, sound), partial, path, shareSafe)]);
    }

    // ---- range download ----

    /// <summary>Queues the Export tab's download; the Downloads tab lists it.</summary>
    public void DownloadRange(RangeDownloadRequest request)
    {
        List<RecordingCamera> cameras = [.. _browser.Cameras.Where(request.Cameras.Contains)];
        if (cameras.Count == 0)
        {
            Say("Choose at least one camera");
            return;
        }
        if (Model.DownloadRange(cameras, request.StartMs, request.EndMs, request.Name, new ExportOptions(DirectFolder, ShareSafe), Quality) is not null)
        {
            Say("Download queued; its progress is in Downloads");
            Notify();
        }
    }

    // ---- still pictures ----

    private static string StillExtension(string suggested) => Path.GetExtension(suggested).TrimStart('.').ToLowerInvariant() is "jpg" or "jpeg" ? "jpg" : "png";

    /// <summary>Saves the picture the device playback's shown tile shows, or one per camera, as PNG or JPEG.</summary>
    public async Task SaveStillsAsync(bool eachCamera)
    {
        if (!Ready(out IExportWriters writers, out _))
        {
            return;
        }
        RecordingsPlayer player = _browser.Player;
        if (player.Playback is not { } playback || player.Cells.Count == 0)
        {
            Say("Play an incident or a camera to save its picture");
            return;
        }
        int shown = PlayerGrid.AudibleIndex(player.Mode, player.Cells.Count) ?? 0;
        HashSet<RecordingCamera> checkedCameras = Checked;
        List<PlayerCell> cells = eachCamera ? [.. player.Cells.Where(c => checkedCameras.Contains(c.Camera))] : [player.Cells[Math.Min(shown, player.Cells.Count - 1)]];
        Planned<StillPlan> planned = Model.PlanStills(ExportRequest.Stills(playback, cells.Select(c => c.Tile), StillFormat, Options(useFolder: false)));
        if (planned.Plan is not { } plan)
        {
            Say(planned.Refusal!.Text);
            return;
        }
        List<string>? paths = await Destinations([.. plan.Stills.Select(s => s.SuggestedFile)], StillExtension(plan.Stills[0].SuggestedFile), "Choose where to save the pictures");
        if (paths is null)
        {
            return;
        }
        var items = plan.Stills.Zip(paths).Where(p => p.First.Tile is not null).ToList();
        PictureFormat format = StillFormat;
        Run(items.Count == 1 ? "Saving the picture" : $"Saving {items.Count} pictures", control =>
            WriteStills(items.Select<(StillItem Still, string Path), Func<ExportOutcome>>(
                p => () => writers.StillFromTile(p.Still.Tile!.Value, FormatOf(p.Path, format), p.Path)).ToList(), control));
    }

    /// <summary>
    /// Saves one camera's picture at <paramref name="atMs"/> from the downloaded files of <paramref name="download"/>
    /// (decoded from the keyframe at or before, on to the picture asked for), one file per camera.
    /// </summary>
    public async Task SaveStillsFromFilesAsync(string download, IEnumerable<RecordingCamera> cameras, long atMs)
    {
        if (!Ready(out IExportWriters writers, out _))
        {
            return;
        }
        var stills = new List<StillItem>();
        foreach (RecordingCamera camera in cameras)
        {
            Planned<StillPlan> planned = Model.PlanStills(ExportRequest.Still(download, camera, atMs, true, StillFormat, Options(useFolder: false)));
            if (planned.Plan is null)
            {
                Say(planned.Refusal!.Text);
                return;
            }
            stills.AddRange(planned.Plan.Stills);
        }
        if (stills.Count == 0)
        {
            return;
        }
        List<string>? paths = await Destinations([.. stills.Select(s => s.SuggestedFile)], StillExtension(stills[0].SuggestedFile), "Choose where to save the pictures");
        if (paths is null)
        {
            return;
        }
        var items = stills.Zip(paths).ToList();
        PictureFormat format = StillFormat;
        Run($"Saving {items.Count} picture{(items.Count == 1 ? "" : "s")}", control =>
            WriteStills(items.Select<(StillItem Still, string Path), Func<ExportOutcome>>(p => () =>
                p.Still.File is { } file && p.Still.SourceInMs is long ms
                    ? writers.StillFromFile(file, ms, true, FormatOf(p.Path, format), p.Path)
                    : ExportOutcome.Failed(p.Path, "A picture could not be read from the file")).ToList(), control));
    }

    private static PictureFormat FormatOf(string path, PictureFormat fallback) => PictureFormats.FromExtension(Path.GetExtension(path)) ?? fallback;

    private static List<ExportOutcome> WriteStills(List<Func<ExportOutcome>> saves, ExportControl control)
    {
        var outcomes = new List<ExportOutcome>();
        for (int i = 0; i < saves.Count; i++)
        {
            if (control.IsCanceled)
            {
                outcomes.Add(ExportOutcome.Canceled(""));
                break;
            }
            outcomes.Add(saves[i]());
            control.Fraction = (i + 1.0) / saves.Count;
            if (outcomes[^1].Result != ExportResult.Done)
            {
                break;
            }
        }
        return outcomes;
    }
}
