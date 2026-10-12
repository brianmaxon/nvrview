// Export in the recordings model (docs/FFI-RECORDINGS.md section 7a): range
// downloads, the plans for trims, stills, and follow cuts or grids, and
// waiting for a download to finish. The refusal texts are the macOS app's.

using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>Why a plan could not be made, in words for the user.</summary>
public sealed record ExportRefusal(string Text);

/// <summary>A plan, or why there is none.</summary>
public readonly record struct Planned<T>(T? Plan, ExportRefusal? Refusal) where T : class
{
    public static Planned<T> Of(T plan) => new(plan, null);

    public static Planned<T> Refused(string text) => new(null, new ExportRefusal(text));
}

public sealed partial class RecordingsService
{
    private readonly Dictionary<string, List<Action<DownloadProgress>>> _finishWaiters = [];

    /// <summary>An incident's finished download (the newest), whose files an export is made from.</summary>
    public DownloadProgress? FinishedDownload(string incidentKey) =>
        Downloads.LastOrDefault(d => d.Incident == incidentKey && d.State == "done");

    /// <summary>The newest finished range download that holds <paramref name="camera"/> for the whole of the span.</summary>
    public DownloadProgress? FinishedRange(RecordingCamera camera, long startMs, long endMs) =>
        Downloads.LastOrDefault(d => d.State == "done" && d.Range is { } r && r.Start * 1000 <= startMs && endMs <= r.End * 1000
            && d.Cameras.Any(c => c.Camera == camera && c.Output is not null));

    /// <summary>
    /// Queues a range download of <paramref name="cameras"/> (<c>nv_download_range</c>); null with
    /// <see cref="Notice"/> set when it was refused.
    /// </summary>
    public QueuedDownload? DownloadRange(IEnumerable<RecordingCamera> cameras, long startMs, long endMs, string? name, ExportOptions options)
    {
        if (_closed || _core is not IExport export)
        {
            return null;
        }
        try
        {
            QueuedDownload? answer = QueuedDownload.Parse(export.DownloadRange(ExportRequest.Range(startMs, endMs, cameras, name, options)));
            Notice = answer?.Existing == true ? "Already downloaded or downloading" : null;
            return answer;
        }
        catch (NvException e)
        {
            Notice = e.Status switch
            {
                NvStatus.NotConfigured => options.Dest is null ? "There is no downloads folder" : "That folder cannot be used for downloads",
                NvStatus.Limit => "At most 16 cameras, 24 hours, and 1000 downloads",
                NvStatus.InvalidArgument => "That range or those cameras cannot be downloaded",
                NvStatus.Unsupported => "This version of the core cannot download a range",
                _ => $"Could not download {StatusText(e)}",
            };
            return null;
        }
    }

    /// <summary>Runs <paramref name="body"/> once when download <paramref name="id"/> is done or failed (at once if it already is).</summary>
    public void WhenFinished(string id, Action<DownloadProgress> body)
    {
        if (Downloads.FirstOrDefault(d => d.Download == id) is { State: "done" or "failed" } d)
        {
            body(d);
            return;
        }
        if (!_finishWaiters.TryGetValue(id, out List<Action<DownloadProgress>>? list))
        {
            _finishWaiters[id] = list = [];
        }
        list.Add(body);
    }

    private void RunFinishWaiters()
    {
        foreach (string id in _finishWaiters.Keys.ToList())
        {
            if (Downloads.FirstOrDefault(d => d.Download == id) is { State: "done" or "failed" } d)
            {
                List<Action<DownloadProgress>> bodies = _finishWaiters[id];
                _finishWaiters.Remove(id);
                foreach (Action<DownloadProgress> body in bodies)
                {
                    body(d);
                }
            }
        }
    }

    private static string ExportRefusalText(NvException e, string none) => e.Status switch
    {
        NvStatus.UnknownId => "The download or playback is gone",
        NvStatus.NotConfigured => "There is no downloads folder",
        NvStatus.InvalidArgument => none,
        NvStatus.Unsupported => "This version of the core cannot export",
        _ => $"Could not plan the export {StatusText(e)}",
    };

    /// <summary><c>nv_edit_compute</c> of kind <c>trim</c>.</summary>
    public Planned<TrimPlan> PlanTrim(string requestJson)
    {
        if (!Usable || _core is not IEditSound sound)
        {
            return Planned<TrimPlan>.Refused("Recordings are not available");
        }
        try
        {
            return TrimPlan.Parse(sound.EditCompute(requestJson)) is { } plan ? Planned<TrimPlan>.Of(plan)
                : Planned<TrimPlan>.Refused("The core's trim could not be read");
        }
        catch (NvException e)
        {
            return Planned<TrimPlan>.Refused(ExportRefusalText(e, "No camera has video between the in and out points"));
        }
    }

    /// <summary><c>nv_edit_compute</c> of kind <c>follow</c> or <c>grid</c>: the core's answer as JSON (the follow and grid views parse it fully).</summary>
    public Planned<EditExportPlanJson> PlanEdit(string requestJson)
    {
        if (!Usable || _core is not IEditSound sound)
        {
            return Planned<EditExportPlanJson>.Refused("Recordings are not available");
        }
        try
        {
            string json = sound.EditCompute(requestJson);
            return EditExportPlan.Parse(json) is { } plan ? Planned<EditExportPlanJson>.Of(new EditExportPlanJson(plan, json))
                : Planned<EditExportPlanJson>.Refused("The core's edit list could not be read");
        }
        catch (NvException e)
        {
            return Planned<EditExportPlanJson>.Refused(ExportRefusalText(e, "No camera of this download has video where the incident happened"));
        }
    }

    /// <summary><c>nv_still_plan</c>.</summary>
    public Planned<StillPlan> PlanStills(string requestJson)
    {
        if (!Usable || _core is not IExport export)
        {
            return Planned<StillPlan>.Refused("Recordings are not available");
        }
        try
        {
            return StillPlan.Parse(export.StillPlan(requestJson)) is { } plan ? Planned<StillPlan>.Of(plan)
                : Planned<StillPlan>.Refused("The core's picture plan could not be read");
        }
        catch (NvException e)
        {
            return Planned<StillPlan>.Refused(ExportRefusalText(e, "There is no picture to save yet"));
        }
    }
}

/// <summary>A follow cut or grid plan: what the export needs of it, and the core's whole answer for the view that renders it.</summary>
public sealed record EditExportPlanJson(EditExportPlan Plan, string Json);
