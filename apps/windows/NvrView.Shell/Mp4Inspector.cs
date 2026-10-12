// Reading a finished recording back (the self-test's real-core steps): the tracks of an MP4 from its
// boxes, with no media framework: what each is (video or sound), whether it is enabled (a call
// recording's alternate group plays one), how many samples it holds and how long it lasts, and
// whether its first sample is a sync sample. Enough to check what the core wrote; it does not decode.

using System.Text;

namespace NvrView.Shell;

/// <summary>One track of an MP4.</summary>
/// <param name="Handler"><c>vide</c> or <c>soun</c>.</param>
/// <param name="Codec">The sample entry's type (<c>avc1</c>, <c>hvc1</c>, <c>mp4a</c>, ...).</param>
/// <param name="Enabled">The track's enabled flag.</param>
/// <param name="AlternateGroup">The track's alternate group (0: none).</param>
/// <param name="Samples">The number of samples.</param>
/// <param name="Seconds">The track's duration.</param>
/// <param name="FirstSampleIsSync">The first sample is a sync sample (no sync table: every sample is).</param>
public sealed record Mp4Track(string Handler, string Codec, bool Enabled, int AlternateGroup, uint Samples, double Seconds, bool FirstSampleIsSync);

/// <summary>What <see cref="Mp4Inspector.Inspect"/> found.</summary>
/// <param name="Brand">The file's major brand.</param>
/// <param name="Fragmented">The file has movie fragments (an unfinished or fragmented file).</param>
/// <param name="Tracks">The tracks, in file order.</param>
public sealed record Mp4Info(string Brand, bool Fragmented, IReadOnlyList<Mp4Track> Tracks)
{
    /// <summary>The video tracks.</summary>
    public IEnumerable<Mp4Track> Video => Tracks.Where(t => t.Handler == "vide");

    /// <summary>The sound tracks.</summary>
    public IEnumerable<Mp4Track> Audio => Tracks.Where(t => t.Handler == "soun");
}

/// <summary>Reads the boxes of an MP4 file.</summary>
public static class Mp4Inspector
{
    private readonly record struct Box(string Type, int Start, int End, int PayloadStart);

    /// <summary>The file's tracks; null when it is not a finished MP4 (no <c>ftyp</c> or no <c>moov</c>).</summary>
    public static Mp4Info? Inspect(byte[] file)
    {
        List<Box> top = Boxes(file, 0, file.Length);
        if (top.Count == 0 || top[0].Type != "ftyp" || !top.Any(b => b.Type == "moov"))
        {
            return null;
        }
        Box ftyp = top[0];
        string brand = ftyp.End - ftyp.PayloadStart >= 4 ? Encoding.ASCII.GetString(file, ftyp.PayloadStart, 4) : "";
        var tracks = new List<Mp4Track>();
        foreach (Box moov in top.Where(b => b.Type == "moov"))
        {
            foreach (Box trak in Boxes(file, moov.PayloadStart, moov.End).Where(b => b.Type == "trak"))
            {
                if (Track(file, trak) is { } t)
                {
                    tracks.Add(t);
                }
            }
        }
        return new Mp4Info(brand, top.Any(b => b.Type == "moof"), tracks);
    }

    private static Mp4Track? Track(byte[] f, Box trak)
    {
        List<Box> parts = Boxes(f, trak.PayloadStart, trak.End);
        Box? tkhd = Find(parts, "tkhd");
        Box? mdia = Find(parts, "mdia");
        if (tkhd is null || mdia is null)
        {
            return null;
        }
        // tkhd: version and 24 bits of flags (bit 0: enabled); alternate_group sits after the ids, duration, reserved and layer.
        int v = f[tkhd.Value.PayloadStart];
        bool enabled = (f[tkhd.Value.PayloadStart + 3] & 1) != 0;
        int groupAt = tkhd.Value.PayloadStart + 4 + (v == 1 ? 8 + 8 + 4 + 4 + 8 : 4 + 4 + 4 + 4 + 4) + 8 + 2;
        int group = groupAt + 2 <= tkhd.Value.End ? U16(f, groupAt) : 0;

        List<Box> media = Boxes(f, mdia.Value.PayloadStart, mdia.Value.End);
        Box? mdhd = Find(media, "mdhd");
        Box? hdlr = Find(media, "hdlr");
        Box? minf = Find(media, "minf");
        if (mdhd is null || hdlr is null || minf is null)
        {
            return null;
        }
        int m = mdhd.Value.PayloadStart;
        int mv = f[m];
        uint timescale = mv == 1 ? U32(f, m + 4 + 16) : U32(f, m + 4 + 8);
        double duration = mv == 1 ? (double)U64(f, m + 4 + 20) : U32(f, m + 4 + 12);
        string handler = Encoding.ASCII.GetString(f, hdlr.Value.PayloadStart + 8, 4);

        Box? stbl = Find(Boxes(f, minf.Value.PayloadStart, minf.Value.End), "stbl");
        if (stbl is null)
        {
            return null;
        }
        List<Box> table = Boxes(f, stbl.Value.PayloadStart, stbl.Value.End);
        string codec = "";
        if (Find(table, "stsd") is { } stsd && stsd.End - stsd.PayloadStart >= 16)
        {
            codec = Encoding.ASCII.GetString(f, stsd.PayloadStart + 12, 4);
        }
        uint samples = 0;
        if (Find(table, "stsz") is { } stsz && stsz.End - stsz.PayloadStart >= 12)
        {
            samples = U32(f, stsz.PayloadStart + 8);
        }
        bool sync = true;
        if (Find(table, "stss") is { } stss)
        {
            sync = stss.End - stss.PayloadStart >= 12 && U32(f, stss.PayloadStart + 4) > 0 && U32(f, stss.PayloadStart + 8) == 1;
        }
        return new Mp4Track(handler, codec, enabled, group, samples, timescale == 0 ? 0 : duration / timescale, sync);
    }

    private static Box? Find(List<Box> boxes, string type)
    {
        foreach (Box b in boxes)
        {
            if (b.Type == type)
            {
                return b;
            }
        }
        return null;
    }

    // The boxes laid end to end in [from, to).
    private static List<Box> Boxes(byte[] f, int from, int to)
    {
        var boxes = new List<Box>();
        long at = from;
        while (at + 8 <= to)
        {
            long size = U32(f, (int)at);
            string type = Encoding.ASCII.GetString(f, (int)at + 4, 4);
            int header = 8;
            if (size == 1)
            {
                if (at + 16 > to)
                {
                    break;
                }
                size = (long)U64(f, (int)at + 8);
                header = 16;
            }
            else if (size == 0)
            {
                size = to - at;
            }
            if (size < header || at + size > to)
            {
                break;
            }
            boxes.Add(new Box(type, (int)at, (int)(at + size), (int)at + header));
            at += size;
        }
        return boxes;
    }

    private static int U16(byte[] f, int at) => f[at] << 8 | f[at + 1];

    private static uint U32(byte[] f, int at) => (uint)(f[at] << 24 | f[at + 1] << 16 | f[at + 2] << 8 | f[at + 3]);

    private static ulong U64(byte[] f, int at) => (ulong)U32(f, at) << 32 | U32(f, at + 4);
}
