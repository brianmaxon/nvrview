// Export (ABI 3 minor 10; docs/FFI.md "Export (minor 10)",
// docs/FFI-RECORDINGS.md section 7a): IExport, the calls the export views
// make, and NvCore's implementation over the C functions. The core plans;
// the app decodes, encodes, and writes. Answers stay JSON text in the
// contract's shapes. The trim kind, in_ms and out_ms, dest, and share_safe
// of nv_edit_compute go through IEditSound.EditCompute.

namespace NvrView.Interop;

/// <summary>
/// The export calls (minor 10), as the core and the fake answer them.
/// Errors throw <see cref="NvException"/> with the C function's status. Not
/// from inside a callback.
/// </summary>
public interface IExport
{
    /// <summary>
    /// <c>nv_download_range</c>: queues the download of a time range of
    /// chosen cameras (<c>{"start","end","cameras":[{"camera","tier"}],
    /// "name","dest","share_safe"}</c>) and answers
    /// <c>{"download","folder","existing"}</c>. The same cameras, tiers, and
    /// range return the existing download.
    /// </summary>
    string DownloadRange(string request);

    /// <summary>
    /// <c>nv_still_plan</c>: which picture to save and what to call it, from
    /// a playback's tiles or a position in a finished file.
    /// </summary>
    string StillPlan(string request);
}

public sealed unsafe partial class NvCore : IExport
{
    /// <inheritdoc/>
    public string DownloadRange(string request) =>
        Once((c, b, n, l) => NvNative.nv_download_range(c, request, b, n, l), nameof(NvNative.nv_download_range));

    /// <inheritdoc/>
    public string StillPlan(string request) =>
        Answer((c, b, n, l) => NvNative.nv_still_plan(c, request, b, n, l), nameof(NvNative.nv_still_plan));
}
