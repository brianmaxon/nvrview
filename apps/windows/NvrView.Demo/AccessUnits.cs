// Splits an Annex-B file into access units for --bench (the core does this
// for live streams). An access unit starts at an access unit delimiter, at
// a parameter set or SEI after the previous picture's slices, or at a slice
// that begins a new picture.

using NvrView.Interop;

namespace NvrView.Demo;

internal readonly record struct AccessUnit(int Offset, int Length, bool Keyframe);

internal static class AccessUnits
{
    public static List<AccessUnit> Split(byte[] data, NvCodec codec)
    {
        var units = new List<AccessUnit>();
        int start = -1;
        bool hasSlice = false, keyframe = false;
        foreach ((int nalStart, int header) in Nals(data))
        {
            int type = codec == NvCodec.H264 ? data[header] & 0x1F : (data[header] >> 1) & 0x3F;
            bool vcl = codec == NvCodec.H264 ? type is >= 1 and <= 5 : type <= 31;
            bool beginsPicture = vcl && header + (codec == NvCodec.H264 ? 1 : 2) < data.Length &&
                (data[header + (codec == NvCodec.H264 ? 1 : 2)] & 0x80) != 0;
            bool prefix = codec == NvCodec.H264 ? type is 6 or 7 or 8 or 9 : type is 32 or 33 or 34 or 35 or 39;
            if (start >= 0 && hasSlice && (prefix || beginsPicture))
            {
                units.Add(new AccessUnit(start, nalStart - start, keyframe));
                start = -1;
                hasSlice = false;
                keyframe = false;
            }
            if (start < 0)
            {
                start = nalStart;
            }
            if (vcl)
            {
                hasSlice = true;
                keyframe |= codec == NvCodec.H264 ? type == 5 : type is >= 16 and <= 23;
            }
        }
        if (start >= 0 && hasSlice)
        {
            units.Add(new AccessUnit(start, data.Length - start, keyframe));
        }
        return units;
    }

    // (start of the start code, first byte of the NAL header) for every NAL unit.
    private static IEnumerable<(int Start, int Header)> Nals(byte[] d)
    {
        for (int i = 0; i + 3 < d.Length; i++)
        {
            if (d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1)
            {
                int s = i > 0 && d[i - 1] == 0 ? i - 1 : i;
                yield return (s, i + 3);
                i += 2;
            }
        }
    }
}
