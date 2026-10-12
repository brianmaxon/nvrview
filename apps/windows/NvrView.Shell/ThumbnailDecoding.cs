// The platform-independent parts of turning a recording's cover keyframe into
// a small picture (docs/UX.md "Recordings", Thumbnails): the size it is
// shrunk to, the NV12-to-BGR conversion with the shrinking in one pass, and
// the bounded queue that decodes off the UI thread, newest request first.
// The decoder itself (Media Foundation) and the JPEG encoder (WIC) are in
// NvrView.Video; this file builds and is tested on Linux too.

using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>How a cover picture is sized.</summary>
public static class ThumbnailSizing
{
    /// <summary>The longer side of a stored cover picture, in pixels (the same as the macOS app).</summary>
    public const int MaxSide = 320;

    /// <summary>The size a picture of <paramref name="width"/> x <paramref name="height"/> is shrunk to: its aspect kept, never enlarged.</summary>
    public static (int Width, int Height) Fit(int width, int height, int maxSide = MaxSide)
    {
        if (width <= 0 || height <= 0)
        {
            return (0, 0);
        }
        double scale = Math.Min(1.0, maxSide / (double)Math.Max(width, height));
        return (Math.Max(1, (int)Math.Round(width * scale)), Math.Max(1, (int)Math.Round(height * scale)));
    }
}

/// <summary>The YUV matrix a decoded picture is meant to be shown with.</summary>
public enum YuvMatrix
{
    /// <summary>BT.601 (standard definition).</summary>
    Bt601,

    /// <summary>BT.709 (high definition).</summary>
    Bt709,
}

/// <summary>NV12 pictures to small BGR pictures.</summary>
public static class Nv12Scaler
{
    /// <summary>The matrix to assume when the decoder does not say: BT.709 from 720 lines up, BT.601 below.</summary>
    public static YuvMatrix GuessMatrix(int height) => height >= 720 ? YuvMatrix.Bt709 : YuvMatrix.Bt601;

    /// <summary>
    /// Converts the top-left <paramref name="width"/> x <paramref name="height"/> of an NV12 picture (luma rows of
    /// <paramref name="pitch"/> bytes, then the interleaved chroma plane at <paramref name="chromaOffset"/>) to
    /// <paramref name="destWidth"/> x <paramref name="destHeight"/> 24-bit BGR rows (no padding), averaging the
    /// source pixels each destination pixel covers (a few samples per axis for large reductions).
    /// </summary>
    public static byte[] ToBgr24(ReadOnlySpan<byte> nv12, int pitch, int chromaOffset, int width, int height,
        int destWidth, int destHeight, YuvMatrix matrix, bool fullRange)
    {
        if (width <= 0 || height <= 0 || destWidth <= 0 || destHeight <= 0 || pitch < width ||
            chromaOffset < pitch * height || nv12.Length < chromaOffset + pitch * ((height + 1) / 2))
        {
            throw new ArgumentException("the NV12 picture and its sizes do not fit");
        }
        // Matrix coefficients: R = Y + rv * V, G = Y - gu * U - gv * V, B = Y + bu * U.
        (double rv, double gu, double gv, double bu) = matrix == YuvMatrix.Bt709
            ? (1.5748, 0.1873, 0.4681, 1.8556)
            : (1.402, 0.3441, 0.7141, 1.772);
        double yScale = fullRange ? 1.0 : 255.0 / 219.0;
        double yBias = fullRange ? 0 : 16;
        double cScale = fullRange ? 1.0 : 255.0 / 224.0;
        var output = new byte[destWidth * destHeight * 3];
        for (int dy = 0; dy < destHeight; dy++)
        {
            int y0 = (int)((long)dy * height / destHeight);
            int y1 = Math.Max(y0 + 1, (int)((long)(dy + 1) * height / destHeight));
            int yStep = Math.Max(1, (y1 - y0) / 4);
            for (int dx = 0; dx < destWidth; dx++)
            {
                int x0 = (int)((long)dx * width / destWidth);
                int x1 = Math.Max(x0 + 1, (int)((long)(dx + 1) * width / destWidth));
                int xStep = Math.Max(1, (x1 - x0) / 4);
                double sr = 0, sg = 0, sb = 0;
                int n = 0;
                for (int y = y0; y < y1 && y < height; y += yStep)
                {
                    int chromaRow = chromaOffset + (y >> 1) * pitch;
                    for (int x = x0; x < x1 && x < width; x += xStep)
                    {
                        double luma = (nv12[y * pitch + x] - yBias) * yScale;
                        int c = chromaRow + (x & ~1);
                        double u = (nv12[c] - 128) * cScale;
                        double v = (nv12[c + 1] - 128) * cScale;
                        sr += luma + rv * v;
                        sg += luma - gu * u - gv * v;
                        sb += luma + bu * u;
                        n++;
                    }
                }
                int o = (dy * destWidth + dx) * 3;
                output[o] = Clamp(sb / n);
                output[o + 1] = Clamp(sg / n);
                output[o + 2] = Clamp(sr / n);
            }
        }
        return output;
    }

    private static byte Clamp(double v) => v <= 0 ? (byte)0 : v >= 255 ? (byte)255 : (byte)(v + 0.5);
}

/// <summary>
/// Decodes cover keyframes off the UI thread: a bounded stack, so the newest
/// request goes first and the oldest is dropped when it is full. Dropped or
/// canceled requests are answered with <see cref="Canceled"/> (the picture
/// is simply asked for again when its row is shown again).
/// </summary>
public sealed class ThumbnailDecodeQueue : IDisposable
{
    /// <summary>The answer for a request that was dropped before it ran (compared by reference).</summary>
    public static readonly RecordedThumbnail Canceled = new(NvThumbKind.Image, 0, 0, 0, []);

    private readonly Func<RecordedThumbnail, RecordedThumbnail?> _decode;
    private readonly Action<string>? _error;
    private readonly int _capacity;
    private readonly List<(RecordedThumbnail Keyframe, Action<RecordedThumbnail?> Done)> _items = [];
    private readonly object _lock = new();
    private readonly Thread? _worker;
    private bool _disposed;

    /// <summary>A queue that decodes with <paramref name="decode"/> (null: could not).</summary>
    /// <param name="decode">The platform decoder; runs on the worker thread, one at a time.</param>
    /// <param name="capacity">Requests held at once.</param>
    /// <param name="background">False: no thread, the caller runs requests with <see cref="RunNext"/> (tests).</param>
    /// <param name="error">Told why a decode threw (the app logs it rate-limited).</param>
    public ThumbnailDecodeQueue(Func<RecordedThumbnail, RecordedThumbnail?> decode, int capacity = 24, bool background = true,
        Action<string>? error = null)
    {
        _decode = decode;
        _capacity = Math.Max(1, capacity);
        _error = error;
        if (background)
        {
            _worker = new Thread(Work) { IsBackground = true, Name = "thumbnail-decode" };
            _worker.Start();
        }
    }

    /// <summary>Requests waiting.</summary>
    public int Count
    {
        get
        {
            lock (_lock)
            {
                return _items.Count;
            }
        }
    }

    /// <summary>Queues a keyframe; <paramref name="done"/> is called once, from any thread.</summary>
    public void Submit(RecordedThumbnail keyframe, Action<RecordedThumbnail?> done)
    {
        Action<RecordedThumbnail?>? dropped = null;
        lock (_lock)
        {
            if (_disposed)
            {
                dropped = done;
            }
            else
            {
                if (_items.Count >= _capacity)
                {
                    dropped = _items[0].Done;
                    _items.RemoveAt(0);
                }
                _items.Add((keyframe, done));
                Monitor.Pulse(_lock);
            }
        }
        dropped?.Invoke(Canceled);
    }

    /// <summary>Drops every waiting request (the one running finishes). Returns how many.</summary>
    public int CancelQueued()
    {
        List<Action<RecordedThumbnail?>> dropped;
        lock (_lock)
        {
            dropped = [.. _items.Select(i => i.Done)];
            _items.Clear();
        }
        foreach (Action<RecordedThumbnail?> d in dropped)
        {
            d(Canceled);
        }
        return dropped.Count;
    }

    /// <summary>Runs the newest waiting request on the calling thread. False when none waits.</summary>
    public bool RunNext()
    {
        (RecordedThumbnail Keyframe, Action<RecordedThumbnail?> Done) item;
        lock (_lock)
        {
            if (_items.Count == 0)
            {
                return false;
            }
            item = _items[^1];
            _items.RemoveAt(_items.Count - 1);
        }
        Run(item.Keyframe, item.Done);
        return true;
    }

    private void Run(RecordedThumbnail keyframe, Action<RecordedThumbnail?> done)
    {
        RecordedThumbnail? small = null;
        try
        {
            small = _decode(keyframe);
        }
        catch (Exception e) when (e is not OutOfMemoryException)
        {
            _error?.Invoke($"a cover picture failed to decode: {e.Message}");
        }
        done(small);
    }

    private void Work()
    {
        while (true)
        {
            (RecordedThumbnail Keyframe, Action<RecordedThumbnail?> Done) item;
            lock (_lock)
            {
                while (_items.Count == 0)
                {
                    if (_disposed)
                    {
                        return;
                    }
                    Monitor.Wait(_lock);
                }
                item = _items[^1];
                _items.RemoveAt(_items.Count - 1);
            }
            Run(item.Keyframe, item.Done);
        }
    }

    /// <summary>Drops the waiting requests and stops the thread.</summary>
    public void Dispose()
    {
        lock (_lock)
        {
            _disposed = true;
            Monitor.PulseAll(_lock);
        }
        CancelQueued();
    }
}

/// <summary>
/// The colour description in an H.265 stream's sequence parameter set (its VUI), for decoders that do not report
/// it. Reads only what is needed; anything unusual (scaling lists) gives no answer.
/// </summary>
public static class HevcColor
{
    /// <summary>The matrix and range the stream says, each null when it says nothing.</summary>
    public static (YuvMatrix? Matrix, bool? FullRange) Read(byte[] annexB)
    {
        for (int i = 0; i + 4 < annexB.Length; i++)
        {
            if (annexB[i] == 0 && annexB[i + 1] == 0 && annexB[i + 2] == 1 && ((annexB[i + 3] >> 1) & 0x3F) == 33)
            {
                int end = i + 3;
                while (end + 2 < annexB.Length && !(annexB[end] == 0 && annexB[end + 1] == 0 && annexB[end + 2] <= 1))
                {
                    end++;
                }
                try
                {
                    return Parse(annexB.AsSpan(i + 5, Math.Min(end, annexB.Length) - (i + 5)));
                }
                catch (InvalidOperationException)
                {
                    return (null, null);
                }
            }
        }
        return (null, null);
    }

    private sealed class Bits(byte[] data)
    {
        private int _pos;

        public uint U(int n)
        {
            uint v = 0;
            for (int k = 0; k < n; k++, _pos++)
            {
                if (_pos >> 3 >= data.Length)
                {
                    throw new InvalidOperationException("short");
                }
                v = (v << 1) | (uint)((data[_pos >> 3] >> (7 - (_pos & 7))) & 1);
            }
            return v;
        }

        public bool Flag() => U(1) == 1;

        public uint Ue()
        {
            int zeros = 0;
            while (U(1) == 0)
            {
                if (++zeros > 31)
                {
                    throw new InvalidOperationException("bad code");
                }
            }
            return zeros == 0 ? 0 : (1u << zeros) - 1 + U(zeros);
        }

        public void Skip(int n)
        {
            for (; n > 0; n--)
            {
                _ = U(1);
            }
        }
    }

    private static (YuvMatrix?, bool?) Parse(ReadOnlySpan<byte> payload)
    {
        // Drop the emulation prevention bytes (00 00 03).
        var rbsp = new List<byte>(payload.Length);
        for (int i = 0; i < payload.Length; i++)
        {
            if (i >= 2 && payload[i] == 3 && payload[i - 1] == 0 && payload[i - 2] == 0)
            {
                continue;
            }
            rbsp.Add(payload[i]);
        }
        var b = new Bits([.. rbsp]);
        b.Skip(4);
        int maxSubLayersMinus1 = (int)b.U(3);
        b.Skip(1);
        b.Skip(96);
        bool[] profile = new bool[8], level = new bool[8];
        for (int i = 0; i < maxSubLayersMinus1; i++)
        {
            profile[i] = b.Flag();
            level[i] = b.Flag();
        }
        if (maxSubLayersMinus1 > 0)
        {
            b.Skip(2 * (8 - maxSubLayersMinus1));
        }
        for (int i = 0; i < maxSubLayersMinus1; i++)
        {
            b.Skip((profile[i] ? 88 : 0) + (level[i] ? 8 : 0));
        }
        _ = b.Ue();
        if (b.Ue() == 3)
        {
            b.Skip(1);
        }
        _ = b.Ue();
        _ = b.Ue();
        if (b.Flag())
        {
            for (int k = 0; k < 4; k++)
            {
                _ = b.Ue();
            }
        }
        _ = b.Ue();
        _ = b.Ue();
        uint log2PocLsb = b.Ue() + 4;
        bool orderingInfo = b.Flag();
        for (int i = orderingInfo ? 0 : maxSubLayersMinus1; i <= maxSubLayersMinus1; i++)
        {
            _ = b.Ue();
            _ = b.Ue();
            _ = b.Ue();
        }
        for (int k = 0; k < 6; k++)
        {
            _ = b.Ue();
        }
        if (b.Flag())
        {
            // Scaling lists: not parsed.
            return (null, null);
        }
        b.Skip(2);
        if (b.Flag())
        {
            b.Skip(8);
            _ = b.Ue();
            _ = b.Ue();
            b.Skip(1);
        }
        uint sets = b.Ue();
        var deltaPocs = new int[Math.Max(1, (int)sets)];
        for (int idx = 0; idx < sets; idx++)
        {
            bool inter = idx != 0 && b.Flag();
            if (inter)
            {
                // Only the slice-header form (delta index) is not possible in an SPS: the reference is the previous set.
                b.Skip(1);
                _ = b.Ue();
                int count = deltaPocs[idx - 1];
                int used = 0;
                for (int j = 0; j <= count; j++)
                {
                    if (b.Flag() || b.Flag())
                    {
                        used++;
                    }
                }
                deltaPocs[idx] = used;
            }
            else
            {
                uint negative = b.Ue();
                uint positive = b.Ue();
                for (uint j = 0; j < negative + positive; j++)
                {
                    _ = b.Ue();
                    b.Skip(1);
                }
                deltaPocs[idx] = (int)(negative + positive);
            }
        }
        if (b.Flag())
        {
            uint longTerm = b.Ue();
            for (uint j = 0; j < longTerm; j++)
            {
                b.Skip((int)log2PocLsb + 1);
            }
        }
        b.Skip(2);
        if (!b.Flag())
        {
            return (null, null);
        }
        if (b.Flag() && b.U(8) == 255)
        {
            b.Skip(32);
        }
        if (b.Flag())
        {
            b.Skip(1);
        }
        if (!b.Flag())
        {
            return (null, null);
        }
        b.Skip(3);
        bool? full = b.Flag();
        YuvMatrix? matrix = null;
        if (b.Flag())
        {
            b.Skip(16);
            uint coeffs = b.U(8);
            matrix = coeffs switch { 1 or 9 or 10 => YuvMatrix.Bt709, 5 or 6 => YuvMatrix.Bt601, _ => null };
        }
        return (matrix, full);
    }
}

/// <summary>Finds a keyframe in an Annex-B stream (the self-test cuts cover keyframes out of the synthetic fixtures).</summary>
public static class AnnexBKeyframes
{
    /// <summary>
    /// The last keyframe in <paramref name="stream"/>: the parameter sets in front of its slices, its slices, and
    /// nothing after them. Empty when the stream has none.
    /// </summary>
    public static byte[] Last(byte[] stream, bool h265)
    {
        var nals = new List<(int Start, bool Vcl, bool Key)>();
        for (int i = 0; i + 3 < stream.Length; i++)
        {
            if (stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1)
            {
                int header = i + 3;
                int type = h265 ? (stream[header] >> 1) & 0x3F : stream[header] & 0x1F;
                bool vcl = h265 ? type <= 31 : type is >= 1 and <= 5;
                bool key = h265 ? type is >= 16 and <= 23 : type == 5;
                nals.Add((i > 0 && stream[i - 1] == 0 ? i - 1 : i, vcl, key));
                i += 2;
            }
        }
        int last = nals.FindLastIndex(n => n.Key);
        if (last < 0)
        {
            return [];
        }
        int first = last;
        while (first > 0 && nals[first - 1].Key)
        {
            first--;
        }
        while (first > 0 && !nals[first - 1].Vcl)
        {
            first--;
        }
        int end = last + 1;
        int endOffset = end < nals.Count ? nals[end].Start : stream.Length;
        return stream[nals[first].Start..endOffset];
    }
}
