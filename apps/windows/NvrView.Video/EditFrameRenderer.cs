// A follow cut or grid drawn offscreen, frame by frame, from files of its own (so it
// never moves the window's pictures). It has the shape of the export's IEditFrames
// (output size, and Render of a plan time into BGRA pixels, top row first), so the
// app's export can wrap it (docs/UX.md "Follow and Grid").

using NvrView.Shell;

namespace NvrView.Video;

/// <summary>An edit drawn offscreen for export (see the file comment).</summary>
public sealed class EditFrameRenderer : IDisposable
{
    private readonly EditFiles _files;
    private readonly GridRenderer _renderer;

    /// <summary>Opens the plan's files and an offscreen target of <paramref name="width"/> by <paramref name="height"/>.</summary>
    public EditFrameRenderer(EditPlan plan, Func<GpuDevice> gpuOf, int width, int height, bool allowHardware = true)
    {
        _files = EditFiles.Open(plan, gpuOf, allowHardware);
        _renderer = new GridRenderer(gpuOf(), (uint)width, (uint)height);
        Width = width;
        Height = height;
    }

    /// <summary>Output width.</summary>
    public int Width { get; }

    /// <summary>Output height.</summary>
    public int Height { get; }

    /// <summary>Sources whose files could not be opened.</summary>
    public IReadOnlyDictionary<int, string> Failures => _files.Failures;

    /// <summary>Draws the edit at a plan time into <paramref name="bgra"/> (Width x Height x 4 bytes); false when it cannot.</summary>
    public bool Render(long timeMs, byte[] bgra)
    {
        if (bgra.Length < Width * Height * 4 || !_files.AnyOpen)
        {
            return false;
        }
        try
        {
            RenderedImage image = _files.Render(timeMs, _renderer);
            Buffer.BlockCopy(image.Bgra, 0, bgra, 0, Width * Height * 4);
            return true;
        }
        catch (VideoException)
        {
            return false;
        }
    }

    /// <inheritdoc/>
    public void Dispose()
    {
        _renderer.Dispose();
        _files.Dispose();
    }
}
