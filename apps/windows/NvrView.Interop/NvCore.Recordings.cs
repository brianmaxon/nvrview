// Recordings, part 1 (ABI 3 minor 5; docs/FFI.md "Recordings (minor 5)",
// docs/FFI-RECORDINGS.md sections 1 to 5): IRecordings, the calls the
// recordings views make, and NvCore's implementation over the C functions.
// Queries and answers stay JSON text in the contract's shapes; the events
// are parsed by NvrView.Shell (RecordingsEvent), which also has a fake.

using System.Text;

namespace NvrView.Interop;

/// <summary>A cover thumbnail: what its bytes are, the picture's size, and the bytes.</summary>
/// <param name="Kind">What the bytes are.</param>
/// <param name="Width">Width of the picture the bytes decode to (0 when unknown).</param>
/// <param name="Height">Height (0 when unknown).</param>
/// <param name="PtsUs">The keyframe's stream timestamp (0 for an image that did not keep it).</param>
/// <param name="Data">The bytes.</param>
public sealed record RecordedThumbnail(NvThumbKind Kind, uint Width, uint Height, long PtsUs, byte[] Data);

/// <summary>
/// The recordings calls (minor 5), as the core and the fake answer them. Every
/// call answers from memory and never waits on the network; device work
/// reports by event. Not from inside a callback. Errors throw
/// <see cref="NvException"/> with the C function's status.
/// </summary>
public interface IRecordings
{
    /// <summary><c>nv_recordings_configure</c>: the recordings settings (JSON).</summary>
    void RecordingsConfigure(string json);

    /// <summary><c>nv_recordings_enable</c>: allows (json) or stops (null) indexing a device by its key.</summary>
    void RecordingsEnable(string deviceKey, string? json);

    /// <summary><c>nv_recordings_calendar</c>: days with recordings per camera.</summary>
    string RecordingsCalendar(string query);

    /// <summary><c>nv_recordings_spans</c>: recording spans per camera.</summary>
    string RecordingsSpans(string query);

    /// <summary><c>nv_recordings_events</c>: events matching the query.</summary>
    string RecordingsEvents(string query);

    /// <summary><c>nv_recordings_want</c>: asks for days to be indexed; returns at once.</summary>
    void RecordingsWant(string json);

    /// <summary><c>nv_recordings_hold_today</c>: keeps today fresh for a window's cameras; null releases.</summary>
    void RecordingsHoldToday(ulong hold, string? json);

    /// <summary><c>nv_recordings_status</c>: the index state of every enabled device.</summary>
    string RecordingsStatus();

    /// <summary><c>nv_incidents_get</c>: incidents overlapping the query's range, newest first.</summary>
    string IncidentsGet(string query);

    /// <summary><c>nv_incident_get</c>: one incident by key.</summary>
    string IncidentGet(string key);

    /// <summary><c>nv_incident_edit</c>: applies and stores one edit (made once).</summary>
    string IncidentEdit(string edit);

    /// <summary><c>nv_incident_lock</c>: locks (and downloads) or unlocks an incident.</summary>
    string IncidentLock(string key, bool locked);

    /// <summary><c>nv_camera_groups_get</c>.</summary>
    string CameraGroupsGet();

    /// <summary><c>nv_camera_groups_put</c>: replaces the groups and name overrides.</summary>
    void CameraGroupsPut(string json);

    /// <summary><c>nv_thumbnail_get</c>: the cached thumbnail, or null while it is fetched (a <c>thumbnail</c> event follows).</summary>
    RecordedThumbnail? ThumbnailGet(string key);

    /// <summary><c>nv_thumbnail_put</c>: replaces the cached entry (usually the app's small image).</summary>
    void ThumbnailPut(string key, RecordedThumbnail thumbnail);

    /// <summary><c>nv_thumbnail_cancel</c>: drops queued fetches of a key, or all (null).</summary>
    void ThumbnailCancel(string? key);

    /// <summary><c>nv_download_incident</c>: queues an incident's download.</summary>
    string DownloadIncident(string request);

    /// <summary><c>nv_download_pause</c>: a whole download (camera null) or one camera (JSON camera).</summary>
    void DownloadPause(string download, string? camera, bool paused);

    /// <summary><c>nv_download_retry</c>: failed cameras again (camera null: all).</summary>
    void DownloadRetry(string download, string? camera);

    /// <summary><c>nv_download_forget</c>: a download with no job running.</summary>
    void DownloadForget(string download, bool deletePartial);

    /// <summary><c>nv_download_trash</c>: <c>{"folder":...}</c> to move to the Recycle Bin after its <c>download_removed</c> event.</summary>
    string DownloadTrash(string download);

    /// <summary><c>nv_downloads_get</c>: every known download.</summary>
    string DownloadsGet();

    /// <summary><c>nv_sound_configure</c>: live sound settings (JSON).</summary>
    void SoundConfigure(string json);
}

public sealed unsafe partial class NvCore : IRecordings
{
    private delegate NvStatus AnswerCall(nint core, byte* output, nuint cap, nuint* len);

    // For calls that change state: one call with a buffer large enough for
    // the answer, because calling again would apply the change again
    // (docs/FFI.md, "Recordings (minor 5)").
    private string Once(AnswerCall call, string function)
    {
        NotInCallback(function);
        byte[] buffer = new byte[64 * 1024];
        nuint len = 0;
        NvStatus s;
        using (AcquireLease(out nint core))
        {
            fixed (byte* p = buffer)
            {
                s = call(core, p, (nuint)buffer.Length, &len);
            }
        }
        Check(s, function);
        return Encoding.UTF8.GetString(buffer, 0, checked((int)len));
    }

    private void Call(Func<nint, NvStatus> call, string function)
    {
        NotInCallback(function);
        using (AcquireLease(out nint core))
        {
            Check(call(core), function);
        }
    }

    private string Answer(AnswerCall call, string function) =>
        ReadString((core, buf, cap, len) => call(core, buf, cap, len), function, 4096);

    /// <inheritdoc/>
    public void RecordingsConfigure(string json) =>
        Call(c => NvNative.nv_recordings_configure(c, json), nameof(NvNative.nv_recordings_configure));

    /// <inheritdoc/>
    public void RecordingsEnable(string deviceKey, string? json) =>
        Call(c => NvNative.nv_recordings_enable(c, deviceKey, json), nameof(NvNative.nv_recordings_enable));

    /// <inheritdoc/>
    public string RecordingsCalendar(string query) =>
        Answer((c, b, n, l) => NvNative.nv_recordings_calendar(c, query, b, n, l), nameof(NvNative.nv_recordings_calendar));

    /// <inheritdoc/>
    public string RecordingsSpans(string query) =>
        Answer((c, b, n, l) => NvNative.nv_recordings_spans(c, query, b, n, l), nameof(NvNative.nv_recordings_spans));

    /// <inheritdoc/>
    public string RecordingsEvents(string query) =>
        Answer((c, b, n, l) => NvNative.nv_recordings_events(c, query, b, n, l), nameof(NvNative.nv_recordings_events));

    /// <inheritdoc/>
    public void RecordingsWant(string json) =>
        Call(c => NvNative.nv_recordings_want(c, json), nameof(NvNative.nv_recordings_want));

    /// <inheritdoc/>
    public void RecordingsHoldToday(ulong hold, string? json) =>
        Call(c => NvNative.nv_recordings_hold_today(c, hold, json), nameof(NvNative.nv_recordings_hold_today));

    /// <inheritdoc/>
    public string RecordingsStatus() =>
        Answer((c, b, n, l) => NvNative.nv_recordings_status(c, b, n, l), nameof(NvNative.nv_recordings_status));

    /// <inheritdoc/>
    public string IncidentsGet(string query) =>
        Answer((c, b, n, l) => NvNative.nv_incidents_get(c, query, b, n, l), nameof(NvNative.nv_incidents_get));

    /// <inheritdoc/>
    public string IncidentGet(string key) =>
        Answer((c, b, n, l) => NvNative.nv_incident_get(c, key, b, n, l), nameof(NvNative.nv_incident_get));

    /// <inheritdoc/>
    public string IncidentEdit(string edit) =>
        Once((c, b, n, l) => NvNative.nv_incident_edit(c, edit, b, n, l), nameof(NvNative.nv_incident_edit));

    /// <inheritdoc/>
    public string IncidentLock(string key, bool locked) =>
        Once((c, b, n, l) => NvNative.nv_incident_lock(c, key, locked, b, n, l), nameof(NvNative.nv_incident_lock));

    /// <inheritdoc/>
    public string CameraGroupsGet() =>
        Answer((c, b, n, l) => NvNative.nv_camera_groups_get(c, b, n, l), nameof(NvNative.nv_camera_groups_get));

    /// <inheritdoc/>
    public void CameraGroupsPut(string json) =>
        Call(c => NvNative.nv_camera_groups_put(c, json), nameof(NvNative.nv_camera_groups_put));

    /// <inheritdoc/>
    public RecordedThumbnail? ThumbnailGet(string key)
    {
        NotInCallback();
        int cap = 64 * 1024;
        using (AcquireLease(out nint core))
        {
            for (int attempt = 0; attempt < 4; attempt++)
            {
                byte[] buffer = new byte[cap];
                NvThumbnail info = default;
                nuint len = 0;
                NvStatus s;
                fixed (byte* p = buffer)
                {
                    s = NvNative.nv_thumbnail_get(core, key, &info, p, (nuint)cap, &len);
                }
                switch (s)
                {
                    case NvStatus.Pending:
                        return null;
                    case NvStatus.BufferTooSmall:
                        cap = Math.Max(checked((int)len), 1);
                        continue;
                    case NvStatus.Ok:
                        return new RecordedThumbnail(info.Kind, info.Width, info.Height, info.PtsUs, buffer[..checked((int)len)]);
                    default:
                        throw new NvException(s, nameof(NvNative.nv_thumbnail_get));
                }
            }
        }
        throw new NvException(NvStatus.BufferTooSmall, nameof(NvNative.nv_thumbnail_get));
    }

    /// <inheritdoc/>
    public void ThumbnailPut(string key, RecordedThumbnail thumbnail)
    {
        ArgumentNullException.ThrowIfNull(thumbnail);
        var info = new NvThumbnail
        {
            Kind = thumbnail.Kind,
            Width = thumbnail.Width,
            Height = thumbnail.Height,
            PtsUs = thumbnail.PtsUs,
        };
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            fixed (byte* p = thumbnail.Data)
            {
                Check(
                    NvNative.nv_thumbnail_put(core, key, &info, p, (nuint)thumbnail.Data.Length),
                    nameof(NvNative.nv_thumbnail_put));
            }
        }
    }

    /// <inheritdoc/>
    public void ThumbnailCancel(string? key) =>
        Call(c => NvNative.nv_thumbnail_cancel(c, key), nameof(NvNative.nv_thumbnail_cancel));

    /// <inheritdoc/>
    public string DownloadIncident(string request) =>
        Once((c, b, n, l) => NvNative.nv_download_incident(c, request, b, n, l), nameof(NvNative.nv_download_incident));

    /// <inheritdoc/>
    public void DownloadPause(string download, string? camera, bool paused) =>
        Call(c => NvNative.nv_download_pause(c, download, camera, paused), nameof(NvNative.nv_download_pause));

    /// <inheritdoc/>
    public void DownloadRetry(string download, string? camera) =>
        Call(c => NvNative.nv_download_retry(c, download, camera), nameof(NvNative.nv_download_retry));

    /// <inheritdoc/>
    public void DownloadForget(string download, bool deletePartial) =>
        Call(c => NvNative.nv_download_forget(c, download, deletePartial), nameof(NvNative.nv_download_forget));

    /// <inheritdoc/>
    public string DownloadTrash(string download) =>
        Once((c, b, n, l) => NvNative.nv_download_trash(c, download, b, n, l), nameof(NvNative.nv_download_trash));

    /// <inheritdoc/>
    public string DownloadsGet() =>
        Answer((c, b, n, l) => NvNative.nv_downloads_get(c, b, n, l), nameof(NvNative.nv_downloads_get));

    /// <inheritdoc/>
    public void SoundConfigure(string json) =>
        Call(c => NvNative.nv_sound_configure(c, json), nameof(NvNative.nv_sound_configure));
}
