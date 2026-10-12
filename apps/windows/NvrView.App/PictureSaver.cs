// Encodes a rendered picture with WIC (Windows.Graphics.Imaging) and writes it (docs/CAPTURE.md "Save
// picture"): PNG, or JPEG at quality 0.9, with no metadata, under the next free name in the pictures
// folder (the shared naming rule, NvrView.Tiles PictureNaming) without replacing a file that is already
// there, or to exactly the file the user chose. Errors name the file, never the folder path.

using NvrView.Tiles;
using NvrView.Video;
using Windows.Foundation;
using Windows.Graphics.Imaging;
using Windows.Storage.Streams;

namespace NvrView.App;

/// <summary>A picture written.</summary>
internal sealed record SavedPicture(string Path, int Width, int Height, long Bytes);

/// <summary>Why a save failed.</summary>
internal enum PictureFailure
{
    /// <summary>There was no picture, or drawing or encoding it failed.</summary>
    Encode,

    /// <summary>No free name in the folder.</summary>
    NoName,

    /// <summary>The file could not be written (<see cref="PictureSaveResult.FileName"/> says which).</summary>
    Write,
}

/// <summary>The outcome of one save: the file, or why not.</summary>
internal sealed record PictureSaveResult(SavedPicture? Saved, PictureFailure? Failure = null, string? FileName = null)
{
    public bool Ok => Saved is not null;
}

internal static class PictureSaver
{
    /// <summary>The picture as file bytes: PNG, or JPEG at quality 0.9; null if the encoder failed.</summary>
    public static async Task<byte[]?> EncodeAsync(RenderedImage image, PictureFormat format)
    {
        try
        {
            using var stream = new InMemoryRandomAccessStream();
            Guid id = format == PictureFormat.Png ? BitmapEncoder.PngEncoderId : BitmapEncoder.JpegEncoderId;
            BitmapEncoder encoder;
            if (format == PictureFormat.Jpeg)
            {
                var options = new BitmapPropertySet
                {
                    ["ImageQuality"] = new BitmapTypedValue((float)PictureFormats.JpegQuality, PropertyType.Single),
                };
                encoder = await BitmapEncoder.CreateAsync(id, stream, options);
            }
            else
            {
                encoder = await BitmapEncoder.CreateAsync(id, stream);
            }
            // The picture is opaque; the encoders write no metadata unless given some.
            encoder.SetPixelData(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, (uint)image.Width, (uint)image.Height, 96, 96, image.Bgra);
            await encoder.FlushAsync();
            var bytes = new byte[stream.Size];
            stream.Seek(0);
            using var reader = new DataReader(stream);
            await reader.LoadAsync((uint)bytes.Length);
            reader.ReadBytes(bytes);
            return bytes;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException or ArgumentException)
        {
            AppLog.Warn($"encoding a picture failed: {e.GetType().Name} 0x{e.HResult:X8}");
            return null;
        }
    }

    /// <summary>Writes <paramref name="data"/> under the next free name in <paramref name="folder"/>.</summary>
    public static PictureSaveResult WriteNext(byte[] data, int width, int height, string folder, string camera, DateTimeOffset at, PictureFormat format, bool shareSafe)
    {
        long ms = PictureNaming.LocalMillis(at);
        for (int attempt = 0; attempt < 8; attempt++)
        {
            string? name = PictureNaming.FileName(ms, camera, shareSafe, format, n => File.Exists(Path.Combine(folder, n)));
            if (name is null)
            {
                return new PictureSaveResult(null, PictureFailure.NoName);
            }
            string path = Path.Combine(folder, name);
            try
            {
                Directory.CreateDirectory(folder);
                using (var file = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                {
                    file.Write(data);
                }
                return new PictureSaveResult(new SavedPicture(path, width, height, data.Length));
            }
            catch (IOException) when (File.Exists(path))
            {
                // Taken meanwhile: the next name.
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                AppLog.Warn($"saving a picture failed: {e.GetType().Name}");
                return new PictureSaveResult(null, PictureFailure.Write, name);
            }
        }
        return new PictureSaveResult(null, PictureFailure.NoName);
    }

    /// <summary>Writes <paramref name="data"/> to exactly <paramref name="path"/> (a file the user chose, who has been asked about replacing it).</summary>
    public static PictureSaveResult WriteTo(byte[] data, int width, int height, string path)
    {
        string name = Path.GetFileName(path);
        try
        {
            string temp = path + ".tmp";
            File.WriteAllBytes(temp, data);
            File.Move(temp, path, overwrite: true);
            return new PictureSaveResult(new SavedPicture(path, width, height, data.Length));
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"saving a picture failed: {e.GetType().Name}");
            return new PictureSaveResult(null, PictureFailure.Write, name);
        }
    }

    /// <summary>An image file's pixels as BGRA, top row first (the self-test reads saved pictures back with this).</summary>
    public static async Task<RenderedImage?> DecodeAsync(string path)
    {
        try
        {
            byte[] bytes = await File.ReadAllBytesAsync(path);
            using var stream = new InMemoryRandomAccessStream();
            using (var writer = new DataWriter(stream))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync();
                writer.DetachStream();
            }
            stream.Seek(0);
            BitmapDecoder decoder = await BitmapDecoder.CreateAsync(stream);
            PixelDataProvider pixels = await decoder.GetPixelDataAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Ignore, new BitmapTransform(), ExifOrientationMode.IgnoreExifOrientation, ColorManagementMode.DoNotColorManage);
            return new RenderedImage((int)decoder.PixelWidth, (int)decoder.PixelHeight, pixels.DetachPixelData());
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or IOException or InvalidOperationException)
        {
            return null;
        }
    }
}
