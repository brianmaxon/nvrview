// What the Recordings window needs from Windows (docs/FFI-RECORDINGS.md
// section 12): the playback host that makes a playback's tiles' decoders
// before any action reaches them, the Recycle Bin through IFileOperation
// (the core stops writing into a download's folder first; nothing is deleted
// permanently), and Show in Explorer.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Shell;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.CLSCTX;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.App;

/// <summary>The playback host over the real core: decoders for a playback's tiles are made with the tiles.</summary>
internal sealed class CorePlaybackHost(CoreHost host) : IPlaybackHost
{
    /// <inheritdoc/>
    public IPlayback Playback => host.Core;

    /// <inheritdoc/>
    public OpenedPlayback Open(string json) => host.OpenPlayback(json);

    /// <inheritdoc/>
    public ulong AddCamera(ulong playback, string json) => host.AddPlaybackCamera(playback, json);

    /// <inheritdoc/>
    public void Released(IReadOnlyList<ulong> tiles) => host.ReleaseTiles(tiles);

    /// <inheritdoc/>
    public void SetAudio(ulong tile, bool audible, float volume)
    {
        try
        {
            host.Core.SetTileAudio(tile, audible, volume);
        }
        catch (NvException)
        {
        }
    }
}

/// <summary>The system calls the Recordings window makes, in one place so the self-test can record them instead.</summary>
internal static unsafe class SystemActions
{
    private const uint FofSilent = 0x4, FofNoConfirmation = 0x10, FofAllowUndo = 0x40, FofNoErrorUi = 0x400, FofxRecycleOnDelete = 0x00080000;

    /// <summary>Moves a file or folder to the Recycle Bin with IFileOperation; false when it could not.</summary>
    public static bool MoveToRecycleBin(string path)
    {
        if (!Directory.Exists(path) && !File.Exists(path))
        {
            return true;
        }
        _ = CoInitializeEx(null, (uint)COINIT.COINIT_APARTMENTTHREADED);
        IFileOperation* operation = null;
        IShellItem* item = null;
        try
        {
            HRESULT hr = CoCreateInstance(Ref(in CLSID.CLSID_FileOperation), null, (uint)CLSCTX_ALL,
                __uuidof<IFileOperation>(), (void**)&operation);
            if (hr.FAILED)
            {
                return false;
            }
            hr = operation->SetOperationFlags(FofSilent | FofNoConfirmation | FofAllowUndo | FofNoErrorUi | FofxRecycleOnDelete);
            if (hr.FAILED)
            {
                return false;
            }
            fixed (char* p = path)
            {
                hr = SHCreateItemFromParsingName(p, null, __uuidof<IShellItem>(), (void**)&item);
            }
            if (hr.FAILED)
            {
                return false;
            }
            hr = operation->DeleteItem(item, null);
            if (hr.FAILED)
            {
                return false;
            }
            hr = operation->PerformOperations();
            return hr.SUCCEEDED;
        }
        finally
        {
            if (item is not null)
            {
                _ = item->Release();
            }
            if (operation is not null)
            {
                _ = operation->Release();
            }
        }
    }

    /// <summary>Shows files (or a folder) in Explorer, the first file selected.</summary>
    public static void ShowInExplorer(IReadOnlyList<string> paths)
    {
        if (paths.Count == 0)
        {
            return;
        }
        string first = paths[0];
        try
        {
            var info = new ProcessStartInfo("explorer.exe") { UseShellExecute = false };
            if (File.Exists(first))
            {
                info.ArgumentList.Add("/select,");
                info.ArgumentList.Add(first);
            }
            else
            {
                info.ArgumentList.Add(first);
            }
            using Process? p = Process.Start(info);
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AppLog.Warn($"Show in Explorer failed: {e.Message}");
        }
    }

    private static Guid* Ref(in Guid g) =>
        (Guid*)System.Runtime.CompilerServices.Unsafe.AsPointer(ref System.Runtime.CompilerServices.Unsafe.AsRef(in g));
}
