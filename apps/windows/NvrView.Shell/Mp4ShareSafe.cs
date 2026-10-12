// Makes a finished MP4 share-safe in place (FFI-RECORDINGS.md section 7a
// (d)): the creation and modification times of the movie, tracks, and media
// become zero (the 1904 "unknown" epoch the core's own files carry), the
// handler names become the generic VideoHandler and SoundHandler (what a
// platform's writer puts there can name its framework), and user data and
// metadata boxes become free space with their contents cleared. Nothing
// moves: every box keeps its size, so offsets in the file stay right. The
// same rules as the macOS app's MP4ShareSafe; where that one only retypes
// the user data and metadata boxes, this one also clears what they held.

using System.Buffers.Binary;
using System.Text;

namespace NvrView.Shell;

/// <summary>The in-place patch of a finished MP4 (see the file comment).</summary>
public static class Mp4ShareSafe
{
    /// <summary>The file could not be read as an MP4 with a movie box.</summary>
    public sealed class NotMp4Exception(string message) : IOException(message);

    private static readonly HashSet<string> Containers = ["moov", "trak", "mdia"];
    private static readonly HashSet<string> Metadata = ["udta", "meta"];

    /// <summary>Patches the file at <paramref name="path"/>.</summary>
    public static void Apply(string path)
    {
        using var file = new FileStream(path, FileMode.Open, FileAccess.ReadWrite, FileShare.None);
        long size = file.Length, offset = 0;
        var head = new byte[16];
        while (offset + 8 <= size)
        {
            file.Seek(offset, SeekOrigin.Begin);
            int got = file.Read(head, 0, 16);
            if (got < 8)
            {
                break;
            }
            ulong boxSize = BinaryPrimitives.ReadUInt32BigEndian(head);
            string type = Encoding.Latin1.GetString(head, 4, 4);
            int header = 8;
            if (boxSize == 1)
            {
                if (got < 16)
                {
                    break;
                }
                boxSize = BinaryPrimitives.ReadUInt64BigEndian(head.AsSpan(8));
                header = 16;
            }
            else if (boxSize == 0)
            {
                boxSize = (ulong)(size - offset);
            }
            if (boxSize < (ulong)header || (ulong)offset + boxSize > (ulong)size)
            {
                break;
            }
            if (type == "moov")
            {
                var moov = new byte[boxSize];
                file.Seek(offset, SeekOrigin.Begin);
                file.ReadExactly(moov);
                Patch(moov, header, moov.Length);
                file.Seek(offset, SeekOrigin.Begin);
                file.Write(moov);
                return;
            }
            offset += (long)boxSize;
        }
        throw new NotMp4Exception("no movie box");
    }

    /// <summary>Patches the boxes in <c>data[from..to]</c> (offsets within <paramref name="data"/>).</summary>
    public static void Patch(byte[] data, int from, int to)
    {
        int at = from;
        while (at + 8 <= to)
        {
            uint boxSize = BinaryPrimitives.ReadUInt32BigEndian(data.AsSpan(at));
            string type = Encoding.Latin1.GetString(data, at + 4, 4);
            if (boxSize < 8 || (long)at + boxSize > to)
            {
                return;
            }
            int size = (int)boxSize;
            switch (type)
            {
                case "mvhd" or "tkhd" or "mdhd":
                    ZeroTimes(data, at + 8);
                    break;
                case "hdlr":
                    RenameHandler(data, at, size);
                    break;
                default:
                    if (Containers.Contains(type))
                    {
                        Patch(data, at + 8, at + size);
                    }
                    else if (Metadata.Contains(type))
                    {
                        Encoding.ASCII.GetBytes("free").CopyTo(data, at + 4);
                        Array.Clear(data, at + 8, size - 8);
                    }
                    break;
            }
            at += size;
        }
    }

    // The box's payload: version and flags, then the times.
    private static void ZeroTimes(byte[] data, int body)
    {
        if (body + 4 > data.Length)
        {
            return;
        }
        int length = data[body] == 1 ? 16 : 8;
        if (body + 4 + length > data.Length)
        {
            return;
        }
        Array.Clear(data, body + 4, length);
    }

    // A hdlr box: version and flags (4), predefined (4), the handler type (4), reserved (12), then the name as a C string.
    private static void RenameHandler(byte[] data, int box, int size)
    {
        int kind = box + 8 + 8, nameStart = box + 8 + 4 + 4 + 4 + 12;
        if (nameStart > box + size || kind + 4 > data.Length)
        {
            return;
        }
        string generic = Encoding.Latin1.GetString(data, kind, 4) switch
        {
            "vide" => "VideoHandler",
            "soun" => "SoundHandler",
            _ => "",
        };
        if (generic.Length == 0)
        {
            return;
        }
        int room = box + size - nameStart;
        byte[] bytes = Encoding.ASCII.GetBytes(generic);
        if (room < bytes.Length + 1)
        {
            return;
        }
        for (int i = 0; i < room; i++)
        {
            data[nameStart + i] = i < bytes.Length ? bytes[i] : (byte)0;
        }
    }
}
