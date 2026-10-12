// Capture (ABI 3 minor 9; docs/FFI.md "Capture", docs/CAPTURE.md): ICapture,
// the calls the capture UI makes, ICaptureEncoder, the app's AAC encoder
// behind NvAudioEncoder, and NvCore's implementation over the C functions.
// Requests and answers stay JSON text in the contract's shapes; the
// recording event is parsed by NvrView.Shell (RecordingEvent), which also
// has a fake.

using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Interop;

/// <summary>What an opened encoder reports (<c>NvAudioEncoder.open</c>).</summary>
/// <param name="AudioSpecificConfig">The AudioSpecificConfig: two bytes for AAC-LC, at most <see cref="NvNative.AscMax"/>.</param>
/// <param name="PrimingFrames">The encoder's delay in frames (samples), written as an edit list.</param>
public sealed record EncoderOpened(byte[] AudioSpecificConfig, uint PrimingFrames);

/// <summary>
/// The app's AAC encoder. <c>track</c> is the audio track's index in the
/// file (live: 0; call: 0 Conversation, 1 Visitor, 2 You). Called on the
/// core's recorder threads (never at once for one recording); return
/// quickly. Answer each encoded frame with
/// <see cref="ICapture.RecordAudioEncoded"/>, in order, from inside
/// <see cref="Encode"/> or <see cref="Close"/> or from any other thread,
/// until <see cref="Close"/> returns. The core keeps the encoder until it is
/// disposed. Exceptions are caught at the boundary (an <see cref="Open"/>
/// that throws refuses the track).
/// </summary>
public interface ICaptureEncoder
{
    /// <summary>Opens an AAC-LC encoder for one track; null if it cannot.</summary>
    EncoderOpened? Open(ulong recording, uint track, uint sampleRate, uint channels, uint bitRate);

    /// <summary>One frame of 1024 mono samples, borrowed for the call.</summary>
    void Encode(ulong recording, uint track, ReadOnlySpan<float> pcm);

    /// <summary>Ends the track; with <paramref name="flush"/>, the encoder's remaining output first.</summary>
    void Close(ulong recording, uint track, bool flush);
}

/// <summary><c>nv_record_start</c>'s answer.</summary>
/// <param name="Recording">The recording's ID (in its <c>recording</c> events).</param>
/// <param name="File">The file being written (absolute).</param>
/// <param name="State"><c>waiting_keyframe</c>.</param>
public sealed record RecordingStarted(ulong Recording, string File, string State)
{
    /// <summary>Parses the answer; null when it is not one.</summary>
    public static RecordingStarted? Parse(string json)
    {
        try
        {
            using JsonDocument d = JsonDocument.Parse(json);
            JsonElement e = d.RootElement;
            return e.TryGetProperty("recording", out JsonElement r) && r.TryGetUInt64(out ulong id) &&
                e.TryGetProperty("file", out JsonElement f) && f.GetString() is { } file &&
                e.TryGetProperty("state", out JsonElement s) && s.GetString() is { } state
                ? new RecordingStarted(id, file, state)
                : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

/// <summary>A <c>nv_record_start</c> request.</summary>
public static class RecordRequest
{
    /// <summary>
    /// A live recording of a tile's camera. <paramref name="quality"/>
    /// (<c>main</c> or <c>sub</c>) and <paramref name="shareSafe"/> override
    /// the settings; <paramref name="name"/> is the camera name for the file;
    /// <paramref name="utcOffsetSeconds"/> is the computer's local offset (the
    /// file name uses local time; see <see cref="LocalUtcOffsetSeconds"/>).
    /// </summary>
    public static string Live(ulong tile, int utcOffsetSeconds, string? quality = null, string? name = null, bool? shareSafe = null)
    {
        var o = new JsonObject { ["kind"] = "live", ["tile"] = tile, ["utc_offset_s"] = utcOffsetSeconds };
        if (quality is not null)
        {
            o["quality"] = quality;
        }
        if (name is not null)
        {
            o["name"] = name;
        }
        if (shareSafe is { } s)
        {
            o["share_safe"] = s;
        }
        return o.ToJsonString();
    }

    /// <summary>A call recording, written to the absolute <c>.mp4</c> path the app reserved.</summary>
    public static string Call(ulong tile, string file) =>
        new JsonObject { ["kind"] = "call", ["tile"] = tile, ["file"] = file }.ToJsonString();

    /// <summary>The computer's offset from UTC now, for <see cref="Live"/>.</summary>
    public static int LocalUtcOffsetSeconds => (int)TimeZoneInfo.Local.GetUtcOffset(DateTime.UtcNow).TotalSeconds;
}

/// <summary>
/// The capture calls (minor 9), as the core and the fake answer them. Errors
/// throw <see cref="NvException"/> with the C function's status (for
/// <see cref="NvStatus.Limit"/> from <see cref="RecordStart"/>,
/// <see cref="NvException.Detail"/> names the limit). Not from inside a
/// callback, except <see cref="RecordAudioEncoded"/>.
/// </summary>
public interface ICapture
{
    /// <summary><c>nv_capture_configure</c>: the live recordings' folder and limits. Call it before <see cref="RecordStart"/>; the first call recovers what a crash left.</summary>
    void CaptureConfigure(string json);

    /// <summary><c>nv_capture_set_encoder</c>: installs the app's AAC encoder, or removes it (null). Recordings already running keep theirs.</summary>
    void CaptureSetEncoder(ICaptureEncoder? encoder);

    /// <summary><c>nv_record_audio_encoded</c>: one raw AAC access unit answering <see cref="ICaptureEncoder.Encode"/> (or the flush of <see cref="ICaptureEncoder.Close"/>), in order. Any thread.</summary>
    NvStatus RecordAudioEncoded(ulong recording, uint track, ReadOnlySpan<byte> unit);

    /// <summary><c>nv_record_start</c>: a live or call recording of a tile's camera (<see cref="RecordRequest"/>); <c>recording</c> events follow.</summary>
    RecordingStarted RecordStart(string request);

    /// <summary><c>nv_record_stop</c>: stops and finalizes; returns at once.</summary>
    void RecordStop(ulong recording);

    /// <summary><c>nv_record_status</c>: <c>{"recordings":[...]}</c>, the latest <c>recording</c> event of each running recording.</summary>
    string RecordStatus();
}

/// <summary>An <see cref="ICaptureEncoder"/> behind <c>NvAudioEncoder.context</c>.</summary>
internal sealed unsafe class EncoderBox(ICaptureEncoder encoder, Dispatcher dispatcher)
{
    private readonly ICaptureEncoder _encoder = encoder;
    private readonly Dispatcher _dispatcher = dispatcher;

    private static EncoderBox? From(void* context)
    {
        try
        {
            return GCHandle.FromIntPtr((nint)context).Target as EncoderBox;
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static byte Open(
        void* context, ulong recording, uint track, uint sampleRate, uint channels, uint bitRate,
        byte* asc, nuint ascCap, nuint* ascLen, uint* priming)
    {
        EncoderBox? box = From(context);
        try
        {
            if (box is null || asc is null || ascLen is null || priming is null)
            {
                return 0;
            }
            EncoderOpened? opened = box._encoder.Open(recording, track, sampleRate, channels, bitRate);
            if (opened is null || opened.AudioSpecificConfig.Length == 0 ||
                (nuint)opened.AudioSpecificConfig.Length > ascCap)
            {
                return 0;
            }
            opened.AudioSpecificConfig.CopyTo(new Span<byte>(asc, opened.AudioSpecificConfig.Length));
            *ascLen = (nuint)opened.AudioSpecificConfig.Length;
            *priming = opened.PrimingFrames;
            return 1;
        }
        catch (Exception e)
        {
            box?._dispatcher.Report(e);
            return 0;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static void Encode(void* context, ulong recording, uint track, float* pcm, nuint frames)
    {
        EncoderBox? box = From(context);
        try
        {
            if (box is not null && pcm is not null)
            {
                box._encoder.Encode(recording, track, new ReadOnlySpan<float>(pcm, checked((int)frames)));
            }
        }
        catch (Exception e)
        {
            box?._dispatcher.Report(e);
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static void Close(void* context, ulong recording, uint track, byte flush)
    {
        EncoderBox? box = From(context);
        try
        {
            box?._encoder.Close(recording, track, flush != 0);
        }
        catch (Exception e)
        {
            box?._dispatcher.Report(e);
        }
    }
}

internal sealed partial class Dispatcher
{
    private readonly List<GCHandle> _encoders = [];

    /// <summary>Keeps an encoder for the core until it is freed; its context pointer.</summary>
    public nint KeepEncoder(EncoderBox box)
    {
        GCHandle h = GCHandle.Alloc(box);
        lock (_encoders)
        {
            _encoders.Add(h);
        }
        return GCHandle.ToIntPtr(h);
    }

    /// <summary>After nv_core_free returned: no encoder function runs any more.</summary>
    public void FreeEncoders()
    {
        lock (_encoders)
        {
            foreach (GCHandle h in _encoders)
            {
                h.Free();
            }
            _encoders.Clear();
        }
    }
}

public sealed unsafe partial class NvCore : ICapture
{
    /// <inheritdoc/>
    public void CaptureConfigure(string json) =>
        Call(c => NvNative.nv_capture_configure(c, json), nameof(NvNative.nv_capture_configure));

    /// <inheritdoc/>
    public void CaptureSetEncoder(ICaptureEncoder? encoder)
    {
        NotInCallback(nameof(NvNative.nv_capture_set_encoder));
        using (AcquireLease(out nint core))
        {
            if (encoder is null)
            {
                Check(NvNative.nv_capture_set_encoder(core, null), nameof(NvNative.nv_capture_set_encoder));
                return;
            }
            var e = new NvAudioEncoder
            {
                Context = (void*)_dispatcher.KeepEncoder(new EncoderBox(encoder, _dispatcher)),
                Open = &EncoderBox.Open,
                Encode = &EncoderBox.Encode,
                Close = &EncoderBox.Close,
            };
            Check(NvNative.nv_capture_set_encoder(core, &e), nameof(NvNative.nv_capture_set_encoder));
        }
    }

    /// <inheritdoc/>
    public NvStatus RecordAudioEncoded(ulong recording, uint track, ReadOnlySpan<byte> unit)
    {
        // Like Decoded: the raw pointer stays valid until nv_core_free
        // returns, so an encoder may answer while the core finalizes.
        nint core = Volatile.Read(ref _dispatcher.Raw);
        if (core == 0)
        {
            return NvStatus.Closed;
        }
        fixed (byte* p = unit)
        {
            return NvNative.nv_record_audio_encoded(core, recording, track, p, (nuint)unit.Length);
        }
    }

    /// <inheritdoc/>
    public RecordingStarted RecordStart(string request)
    {
        NotInCallback(nameof(NvNative.nv_record_start));
        // Room for the path; an answer that does not fit stops that
        // recording again, so starting once more is right.
        int cap = 4096;
        using (AcquireLease(out nint core))
        {
            for (int attempt = 0; attempt < 4; attempt++)
            {
                byte[] buffer = new byte[cap];
                nuint len = 0;
                NvStatus s;
                fixed (byte* p = buffer)
                {
                    s = NvNative.nv_record_start(core, request, p, (nuint)cap, &len);
                }
                if (s == NvStatus.BufferTooSmall)
                {
                    cap = checked((int)len) + 1;
                    continue;
                }
                string text = (int)len < cap ? Encoding.UTF8.GetString(buffer, 0, checked((int)len)) : "";
                if (s != NvStatus.Ok)
                {
                    throw new NvException(s, nameof(NvNative.nv_record_start), s == NvStatus.Limit ? LimitOf(text) : null);
                }
                return RecordingStarted.Parse(text) ?? throw new NvException(NvStatus.Internal, nameof(NvNative.nv_record_start));
            }
        }
        throw new NvException(NvStatus.Internal, nameof(NvNative.nv_record_start));
    }

    private static string? LimitOf(string answer)
    {
        try
        {
            using JsonDocument d = JsonDocument.Parse(answer);
            return d.RootElement.TryGetProperty("limit", out JsonElement l) ? l.GetString() : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <inheritdoc/>
    public void RecordStop(ulong recording) =>
        Call(c => NvNative.nv_record_stop(c, recording), nameof(NvNative.nv_record_stop));

    /// <inheritdoc/>
    public string RecordStatus() =>
        Answer((c, b, n, l) => NvNative.nv_record_status(c, b, n, l), nameof(NvNative.nv_record_status));
}
