// Synthetic recordings for the export self-test steps: H.264 MP4 files written with Media Foundation whose every
// picture says which camera it is and which frame it is (the same pictures as the Mac app's SyntheticRecording).
// The top three quarters are one of the palette's colors; the bottom quarter is Bits black or white blocks across
// the width, the frame number in binary (most significant bit on the left). Made up entirely.

using NvrView.Video;

namespace NvrView.App;

internal static class SyntheticRecording
{
    /// <summary>The cameras' colors (RGB), one per source.</summary>
    public static readonly (int R, int G, int B)[] Palette = [(230, 40, 40), (40, 200, 60), (40, 70, 230), (230, 210, 40)];

    /// <summary>Blocks in the frame-number strip.</summary>
    public const int Bits = 10;

    /// <summary>Writes <paramref name="frames"/> pictures at <paramref name="fps"/> with a keyframe every <paramref name="keyframeInterval"/>.</summary>
    public static bool Write(string path, int color, int frames, int width = 640, int height = 360, int fps = 10, int keyframeInterval = 20)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        MfExportResult r = MfExport.RenderEdit(path, width, height, fps, frames, hevc: false, keyframeInterval,
            (i, pixels) =>
            {
                Draw(i, color, width, height, pixels);
                return true;
            },
            () => false, _ => { });
        return r.Status == MfExportStatus.Done;
    }

    private static void Draw(int frame, int color, int width, int height, byte[] bgra)
    {
        (int r, int g, int b) = Palette[color % Palette.Length];
        int strip = height * 3 / 4, block = width / Bits;
        for (int y = 0; y < height; y++)
        {
            for (int x = 0; x < width; x++)
            {
                int i = (y * width + x) * 4;
                if (y < strip)
                {
                    bgra[i] = (byte)b;
                    bgra[i + 1] = (byte)g;
                    bgra[i + 2] = (byte)r;
                }
                else
                {
                    int bit = Math.Min(x / Math.Max(block, 1), Bits - 1);
                    byte v = (frame >> (Bits - 1 - bit) & 1) == 1 ? (byte)255 : (byte)0;
                    bgra[i] = bgra[i + 1] = bgra[i + 2] = v;
                }
                bgra[i + 3] = 255;
            }
        }
    }

    /// <summary>The camera (palette index) and frame number a picture shows; null for a part it cannot read.</summary>
    public static (int? Color, int? Frame) Read(RenderedImage image)
    {
        (int r, int g, int b) = image.Rgb(image.Width / 2, image.Height / 4);
        int? color = null;
        double best = 90;
        for (int c = 0; c < Palette.Length; c++)
        {
            double d = Math.Sqrt(Math.Pow(r - Palette[c].R, 2) + Math.Pow(g - Palette[c].G, 2) + Math.Pow(b - Palette[c].B, 2));
            if (d < best)
            {
                best = d;
                color = c;
            }
        }
        int frame = 0, block = image.Width / Bits;
        for (int k = 0; k < Bits; k++)
        {
            (int pr, int pg, int pb) = image.Rgb(Math.Min(k * block + block / 2, image.Width - 1), image.Height * 7 / 8);
            frame = (frame << 1) | ((pr + pg + pb) / 3 > 128 ? 1 : 0);
        }
        return (color, frame);
    }
}
