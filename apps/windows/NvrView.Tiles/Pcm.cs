// Sample formats for the call's audio (docs/UX.md "Doorbell calls"): the system voice renders a
// typed reply as a WAV stream, and the speech recognizer takes 16-bit little-endian PCM while the
// core's taps and talk-back use float samples in -1..1. Pure, so it is tested on any platform.

using System.Buffers.Binary;

namespace NvrView.Tiles;

public static class Pcm
{
    /// <summary>
    /// Reads a RIFF WAVE (16-bit PCM or 32-bit float, any channel count, or the extensible form of
    /// either) into mono float samples, averaging the channels. The data chunk may claim more than
    /// the stream holds (a stream written before its length was known), which reads to the end.
    /// </summary>
    public static bool TryReadWav(ReadOnlySpan<byte> wav, out float[] mono, out uint sampleRate)
    {
        mono = [];
        sampleRate = 0;
        if (wav.Length < 12 || !wav[..4].SequenceEqual("RIFF"u8) || !wav.Slice(8, 4).SequenceEqual("WAVE"u8))
        {
            return false;
        }
        int format = 0, channels = 0, bits = 0;
        int at = 12;
        while (at + 8 <= wav.Length)
        {
            ReadOnlySpan<byte> id = wav.Slice(at, 4);
            uint declared = BinaryPrimitives.ReadUInt32LittleEndian(wav.Slice(at + 4, 4));
            int body = at + 8;
            int size = (int)Math.Min(declared, (uint)(wav.Length - body));
            if (id.SequenceEqual("fmt "u8))
            {
                if (size < 16)
                {
                    return false;
                }
                format = BinaryPrimitives.ReadUInt16LittleEndian(wav.Slice(body, 2));
                channels = BinaryPrimitives.ReadUInt16LittleEndian(wav.Slice(body + 2, 2));
                sampleRate = BinaryPrimitives.ReadUInt32LittleEndian(wav.Slice(body + 4, 4));
                bits = BinaryPrimitives.ReadUInt16LittleEndian(wav.Slice(body + 14, 2));
                if (format == 0xFFFE)
                {
                    // WAVE_FORMAT_EXTENSIBLE: the sample kind follows the channel mask.
                    format = bits == 32 ? 3 : 1;
                }
            }
            else if (id.SequenceEqual("data"u8))
            {
                if (channels < 1 || sampleRate == 0 || !(format == 1 && bits == 16 || format == 3 && bits == 32))
                {
                    return false;
                }
                int bytesPerSample = bits / 8;
                int frames = size / (bytesPerSample * channels);
                mono = new float[frames];
                ReadOnlySpan<byte> data = wav.Slice(body, size);
                for (int i = 0; i < frames; i++)
                {
                    float sum = 0;
                    for (int c = 0; c < channels; c++)
                    {
                        ReadOnlySpan<byte> s = data.Slice((i * channels + c) * bytesPerSample, bytesPerSample);
                        sum += format == 1
                            ? BinaryPrimitives.ReadInt16LittleEndian(s) / 32768f
                            : BitConverter.ToSingle(s);
                    }
                    mono[i] = sum / channels;
                }
                return true;
            }
            // Chunks are padded to an even size.
            at = body + size + (size & 1);
        }
        return false;
    }

    /// <summary>Float samples (-1..1) as 16-bit little-endian PCM into <paramref name="bytes"/> (2 bytes each); clips.</summary>
    public static void ToInt16(ReadOnlySpan<float> samples, Span<byte> bytes)
    {
        for (int i = 0; i < samples.Length; i++)
        {
            float v = Math.Clamp(samples[i], -1f, 1f);
            BinaryPrimitives.WriteInt16LittleEndian(bytes.Slice(i * 2, 2), (short)Math.Round(v * 32767f));
        }
    }
}
