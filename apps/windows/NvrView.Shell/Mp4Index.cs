// What a finished recording's MP4 says about its first video track, read from
// the sample tables alone (no decoding): the picture size, the codec, the
// duration, and the presentation time of every sync sample (keyframe) with the
// edit list applied. The follow cut and grids start decoding at the keyframe
// at or before a position (docs/UX.md "Recordings", "Follow and Grid"), so the
// list is what makes seeks frame accurate and keyframe-only decoding possible.
// Plain .NET, tested on Linux with hand-built files. The same timeline the
// macOS app's RecordedFileSource reads.

using System.Buffers.Binary;

namespace NvrView.Shell;

/// <summary>A video track's description.</summary>
/// <param name="DurationMs">The track's duration.</param>
/// <param name="Width">Picture width.</param>
/// <param name="Height">Picture height.</param>
/// <param name="Codec">"h264", "h265", or the sample entry's four characters.</param>
/// <param name="KeyframesMs">Presentation times of the sync samples, ascending (empty when the file has none listed).</param>
public sealed record Mp4VideoInfo(long DurationMs, int Width, int Height, string Codec, IReadOnlyList<long> KeyframesMs)
{
    /// <summary>The keyframe at or before <paramref name="ms"/> (the first one before the first; 0 when none are listed).</summary>
    public long KeyframeAtOrBefore(long ms)
    {
        if (KeyframesMs.Count == 0)
        {
            return 0;
        }
        int lo = 0, hi = KeyframesMs.Count - 1;
        long found = KeyframesMs[0];
        while (lo <= hi)
        {
            int mid = (lo + hi) / 2;
            if (KeyframesMs[mid] <= ms)
            {
                found = KeyframesMs[mid];
                lo = mid + 1;
            }
            else
            {
                hi = mid - 1;
            }
        }
        return found;
    }
}

/// <summary>Reads <see cref="Mp4VideoInfo"/> from an MP4.</summary>
public static class Mp4Index
{
    /// <summary>Most samples whose sync flags are read.</summary>
    public const int MaxSamples = 500_000;

    /// <summary>Largest moov box read (bytes).</summary>
    public const long MaxMoov = 256L << 20;

    /// <summary>The first video track of the file at <paramref name="path"/>; null when it has none or is not an MP4.</summary>
    public static Mp4VideoInfo? Read(string path)
    {
        try
        {
            using FileStream stream = File.OpenRead(path);
            return Read(stream);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }

    /// <summary>The first video track of an MP4 stream; null when it has none or is not an MP4.</summary>
    public static Mp4VideoInfo? Read(Stream stream)
    {
        long position = 0, length = stream.Length;
        Span<byte> header = stackalloc byte[16];
        while (position + 8 <= length)
        {
            stream.Seek(position, SeekOrigin.Begin);
            if (stream.Read(header[..8]) != 8)
            {
                return null;
            }
            long size = BinaryPrimitives.ReadUInt32BigEndian(header);
            string type = Type(header[4..8]);
            long headerSize = 8;
            if (size == 1)
            {
                if (stream.Read(header[8..16]) != 8)
                {
                    return null;
                }
                size = (long)BinaryPrimitives.ReadUInt64BigEndian(header[8..16]);
                headerSize = 16;
            }
            else if (size == 0)
            {
                size = length - position;
            }
            if (size < headerSize)
            {
                return null;
            }
            if (type == "moov")
            {
                long body = Math.Min(size, length - position) - headerSize;
                if (body <= 0 || body > MaxMoov)
                {
                    return null;
                }
                byte[] moov = new byte[body];
                stream.Seek(position + headerSize, SeekOrigin.Begin);
                stream.ReadExactly(moov);
                try
                {
                    return Moov(moov);
                }
                catch (Exception e) when (e is ArgumentException or IndexOutOfRangeException)
                {
                    // A damaged sample table: no description.
                    return null;
                }
            }
            position += size;
        }
        return null;
    }

    private static string Type(ReadOnlySpan<byte> four) => new([(char)four[0], (char)four[1], (char)four[2], (char)four[3]]);

    // A box found inside a parent's body: its body and where it ends.
    private readonly record struct Box(string Type, int Start, int End);

    private static IEnumerable<Box> Children(byte[] data, int start, int end)
    {
        int p = start;
        while (p + 8 <= end)
        {
            long size = BinaryPrimitives.ReadUInt32BigEndian(data.AsSpan(p));
            string type = Type(data.AsSpan(p + 4, 4));
            int header = 8;
            if (size == 1 && p + 16 <= end)
            {
                size = (long)BinaryPrimitives.ReadUInt64BigEndian(data.AsSpan(p + 8));
                header = 16;
            }
            else if (size == 0)
            {
                size = end - p;
            }
            if (size < header || p + size > end)
            {
                yield break;
            }
            yield return new Box(type, p + header, (int)(p + size));
            p += (int)size;
        }
    }

    private static Box? Find(byte[] data, Box parent, string type)
    {
        foreach (Box b in Children(data, parent.Start, parent.End))
        {
            if (b.Type == type)
            {
                return b;
            }
        }
        return null;
    }

    private static Mp4VideoInfo? Moov(byte[] data)
    {
        var moov = new Box("moov", 0, data.Length);
        uint movieScale = 1000;
        if (Find(data, moov, "mvhd") is { } mvhd)
        {
            movieScale = data[mvhd.Start] == 1 ? U32(data, mvhd.Start + 20) : U32(data, mvhd.Start + 12);
        }
        foreach (Box trak in Children(data, moov.Start, moov.End).Where(b => b.Type == "trak"))
        {
            if (Track(data, trak, movieScale) is { } info)
            {
                return info;
            }
        }
        return null;
    }

    private static Mp4VideoInfo? Track(byte[] data, Box trak, uint movieScale)
    {
        if (Find(data, trak, "mdia") is not { } mdia || Find(data, mdia, "hdlr") is not { } hdlr ||
            hdlr.End - hdlr.Start < 12 || Type(data.AsSpan(hdlr.Start + 8, 4)) != "vide" ||
            Find(data, mdia, "mdhd") is not { } mdhd || Find(data, mdia, "minf") is not { } minf ||
            Find(data, minf, "stbl") is not { } stbl)
        {
            return null;
        }
        bool v1 = data[mdhd.Start] == 1;
        uint scale = v1 ? U32(data, mdhd.Start + 20) : U32(data, mdhd.Start + 12);
        long duration = v1 ? (long)U64(data, mdhd.Start + 24) : U32(data, mdhd.Start + 16);
        if (scale == 0)
        {
            return null;
        }
        // The sample entry: codec and size.
        string codec = "";
        int width = 0, height = 0;
        if (Find(data, stbl, "stsd") is { } stsd && stsd.End - stsd.Start >= 8 + 8 + 36)
        {
            int entry = stsd.Start + 8;
            string four = Type(data.AsSpan(entry + 4, 4));
            codec = four switch { "avc1" or "avc3" => "h264", "hvc1" or "hev1" => "h265", _ => four };
            width = U16(data, entry + 32);
            height = U16(data, entry + 34);
        }
        // Edit list: leading empty edits delay the track, the first real edit's media time is where it starts.
        long editOffset = 0;
        if (Find(data, trak, "edts") is { } edts && Find(data, edts, "elst") is { } elst)
        {
            bool e1 = data[elst.Start] == 1;
            uint count = U32(data, elst.Start + 4);
            int p = elst.Start + 8;
            long delayTicks = 0;
            for (uint i = 0; i < count && p + (e1 ? 20 : 12) <= elst.End; i++)
            {
                long segment = e1 ? (long)U64(data, p) : U32(data, p);
                long media = e1 ? (long)U64(data, p + 8) : (int)U32(data, p + 4);
                p += e1 ? 20 : 12;
                if (media < 0)
                {
                    delayTicks += segment * scale / Math.Max(movieScale, 1);
                    continue;
                }
                editOffset = delayTicks - media;
                break;
            }
        }
        long Ms(long ticks) => (long)Math.Round((ticks + editOffset) * 1000.0 / scale);

        // Sync sample numbers (1-based); no table means every sample is a sync sample.
        var sync = new List<uint>();
        bool all = true;
        if (Find(data, stbl, "stss") is { } stss)
        {
            all = false;
            uint count = U32(data, stss.Start + 4);
            for (uint i = 0; i < count && stss.Start + 8 + (i + 1) * 4 <= stss.End && sync.Count < MaxSamples; i++)
            {
                sync.Add(U32(data, stss.Start + 8 + (int)i * 4));
            }
        }
        // Decode times by stts, composition offsets by ctts, walked together over the samples.
        var stts = Find(data, stbl, "stts") is { } t ? Entries(data, t, 8) : [];
        var ctts = Find(data, stbl, "ctts") is { } c ? Entries(data, c, 8, signed: true) : [];
        var keyframes = new List<long>();
        long dts = 0;
        uint sample = 1;
        int sttsIndex = 0, cttsIndex = 0;
        uint sttsLeft = stts.Count > 0 ? stts[0].Count : 0, cttsLeft = ctts.Count > 0 ? ctts[0].Count : 0;
        int next = 0;
        while ((all || next < sync.Count) && sttsIndex < stts.Count && sample <= MaxSamples)
        {
            long offset = cttsIndex < ctts.Count ? ctts[cttsIndex].Value : 0;
            if (all || sync[next] == sample)
            {
                keyframes.Add(Ms(dts + offset));
                next++;
            }
            dts += stts[sttsIndex].Value;
            if (--sttsLeft == 0 && ++sttsIndex < stts.Count)
            {
                sttsLeft = stts[sttsIndex].Count;
            }
            if (cttsIndex < ctts.Count && --cttsLeft == 0 && ++cttsIndex < ctts.Count)
            {
                cttsLeft = ctts[cttsIndex].Count;
            }
            sample++;
        }
        keyframes.Sort();
        return new Mp4VideoInfo(Ms(duration) - Ms(0), width, height, codec, keyframes);
    }

    // (count, value) pairs of an stts or ctts box; ctts values may be signed (version 1, and in practice version 0 too).
    private static List<(uint Count, long Value)> Entries(byte[] data, Box box, int tableOffset, bool signed = false)
    {
        var list = new List<(uint, long)>();
        uint count = U32(data, box.Start + 4);
        for (uint i = 0; i < count && box.Start + tableOffset + (i + 1) * 8 <= box.End; i++)
        {
            int p = box.Start + tableOffset + (int)i * 8;
            uint n = U32(data, p);
            uint v = U32(data, p + 4);
            list.Add((n, signed ? (int)v : v));
        }
        return list;
    }

    private static uint U32(byte[] d, int p) => BinaryPrimitives.ReadUInt32BigEndian(d.AsSpan(p));

    private static ulong U64(byte[] d, int p) => BinaryPrimitives.ReadUInt64BigEndian(d.AsSpan(p));

    private static int U16(byte[] d, int p) => BinaryPrimitives.ReadUInt16BigEndian(d.AsSpan(p));
}
