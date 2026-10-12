// The offscreen path of Save Picture (docs/CAPTURE.md "Save picture"): the grid renderer's own shaders and color
// matrix draw the tile's shown picture (the one the tile already holds: no second decode, no stream switch) into a
// texture of the saved size, which is read back for the encoder. A renderer of its own per save, on the shared
// device, so nothing of the window's renderer is touched; the device lock covers the whole draw.

using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Video;

/// <summary>What to draw into a saved picture: the part of the picture to copy, or a dewarped view, and the image's size.</summary>
public readonly record struct PicturePlan(int SrcX, int SrcY, int SrcWidth, int SrcHeight, int Width, int Height, bool Dewarp = false, DewarpConstants Rows = default);

/// <summary>Draws a tile's picture offscreen for Save Picture (see the file comment).</summary>
public static class PictureRender
{
    /// <summary>
    /// Renders the picture <paramref name="tile"/> shows now. <paramref name="plan"/> gets that picture's visible size
    /// (read under the device lock, so a stream switch cannot change it between the plan and the draw) and answers
    /// what to draw, or null for nothing. <paramref name="observe"/> (the self-test) sees the picture under the same lock, so
    /// it can read exactly what is drawn. Null when there is no picture or no plan. Waits for the GPU; call off the UI thread.
    /// </summary>
    public static RenderedImage? Render(GpuDevice gpu, VideoTile tile, Func<int, int, PicturePlan?> plan, Action<TilePicture>? observe = null)
    {
        using DeviceLock held = gpu.Lock();
        TilePicture? p = tile.ShownPicture;
        if (p is null || !p.HasPicture)
        {
            return null;
        }
        NvSize size = p.PictureSize;
        if (plan((int)size.Width, (int)size.Height) is not { } job || job.Width < 1 || job.Height < 1)
        {
            return null;
        }
        observe?.Invoke(p);
        using var renderer = new GridRenderer(gpu, (uint)job.Width, (uint)job.Height, gap: 0);
        var draw = new TileDraw
        {
            Tile = tile,
            DestX = 0,
            DestY = 0,
            DestWidth = job.Width,
            DestHeight = job.Height,
            SrcX = job.SrcX,
            SrcY = job.SrcY,
            SrcWidth = job.SrcWidth,
            SrcHeight = job.SrcHeight,
            Dewarp = job.Dewarp,
            DewarpRows = job.Rows,
        };
        return renderer.RenderOne(draw);
    }
}
