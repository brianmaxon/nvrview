// The nvrview C interface, ABI 3, as declared in
// crates/viewer-ffi/include/nvrview.h (contract: docs/FFI.md).
//
// Hand-written to match the generated header. Every struct is blittable with
// the header's exact layout (C `bool` is CBool, one byte; C enums are 32-bit),
// so nothing is converted at the boundary. NvLayoutCheck compares these
// declarations with the sizes, offsets, constants, and function names the C
// compiler and the header report.

using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

// Every type crosses the boundary as it is in memory; the runtime marshaller
// is never involved (LibraryImport generates the little marshalling left).
[assembly: DisableRuntimeMarshalling]

namespace NvrView.Interop;

/// <summary>The C name of an enum member, for NvLayoutCheck (smoke test).</summary>
[AttributeUsage(AttributeTargets.Field)]
public sealed class CNameAttribute(string name) : Attribute
{
    /// <summary>The constant's name in nvrview.h.</summary>
    public string Name { get; } = name;
}

/// <summary>C <c>bool</c>: one byte, 0 or 1.</summary>
[StructLayout(LayoutKind.Sequential)]
public readonly struct CBool
{
    private readonly byte _value;

    private CBool(bool value) => _value = value ? (byte)1 : (byte)0;

    /// <summary>Converts to a managed bool (any nonzero byte is true).</summary>
    public static implicit operator bool(CBool b) => b._value != 0;

    /// <summary>Converts from a managed bool.</summary>
    public static implicit operator CBool(bool b) => new(b);

    /// <inheritdoc/>
    public override string ToString() => ((bool)this).ToString();
}

/// <summary>Return codes. Every function that can fail returns one.</summary>
public enum NvStatus
{
    /// <summary>Success.</summary>
    [CName("NV_OK")] Ok = 0,
    /// <summary>Null pointer, bad UTF-8, out-of-range value.</summary>
    [CName("NV_ERR_INVALID_ARGUMENT")] InvalidArgument = -1,
    /// <summary>Device or tile already removed, or never existed.</summary>
    [CName("NV_ERR_UNKNOWN_ID")] UnknownId = -2,
    /// <summary>The core is shutting down.</summary>
    [CName("NV_ERR_CLOSED")] Closed = -3,
    /// <summary>Too many devices, tiles, layouts, or queued requests.</summary>
    [CName("NV_ERR_LIMIT")] Limit = -4,
    /// <summary>Internal error.</summary>
    [CName("NV_ERR_INTERNAL")] Internal = -5,
    /// <summary>The output buffer is too small; *len holds the size needed.</summary>
    [CName("NV_ERR_BUFFER_TOO_SMALL")] BufferTooSmall = -6,
    /// <summary>Talk-back (camera motion control) is not enabled for that camera.</summary>
    [CName("NV_ERR_NOT_PERMITTED")] NotPermitted = -7,
    /// <summary>The device's backend cannot do this (minor 5).</summary>
    [CName("NV_ERR_UNSUPPORTED")] Unsupported = -8,
    /// <summary>Recordings are not configured, or there is no downloads folder (minor 5).</summary>
    [CName("NV_ERR_NOT_CONFIGURED")] NotConfigured = -9,
    /// <summary>Accepted; the answer arrives as an event (nv_thumbnail_get; minor 5).</summary>
    [CName("NV_PENDING")] Pending = 1,
}

/// <summary>An action's kind. Unknown kinds (a newer core) need no answer.</summary>
public enum NvActionKind
{
    /// <summary>Create the generation's decoder.</summary>
    [CName("NV_ACTION_CONFIGURE")] Configure = 0,
    /// <summary>Decode one access unit; answer exactly once.</summary>
    [CName("NV_ACTION_DECODE")] Decode = 1,
    /// <summary>Discard pending decodes.</summary>
    [CName("NV_ACTION_FLUSH")] Flush = 2,
    /// <summary>Destroy the decoder.</summary>
    [CName("NV_ACTION_RESET")] Reset = 3,
    /// <summary>Clear the tile's picture.</summary>
    [CName("NV_ACTION_CLEAR")] Clear = 4,
    /// <summary>The tile's status changed.</summary>
    [CName("NV_ACTION_STATUS")] Status = 5,
}

/// <summary>Video codec.</summary>
public enum NvCodec
{
    /// <summary>H.264.</summary>
    [CName("NV_CODEC_H264")] H264 = 1,
    /// <summary>H.265.</summary>
    [CName("NV_CODEC_H265")] H265 = 2,
}

/// <summary>Color matrix of a stream.</summary>
public enum NvColorMatrix
{
    /// <summary>BT.601.</summary>
    [CName("NV_MATRIX_BT601")] Bt601 = 0,
    /// <summary>BT.709.</summary>
    [CName("NV_MATRIX_BT709")] Bt709 = 1,
    /// <summary>BT.2020.</summary>
    [CName("NV_MATRIX_BT2020")] Bt2020 = 2,
}

/// <summary>What a tile shows.</summary>
public enum NvTileStatus
{
    /// <summary>Waiting for the first picture.</summary>
    [CName("NV_TILE_WAITING")] Waiting = 0,
    /// <summary>Playing.</summary>
    [CName("NV_TILE_PLAYING")] Playing = 1,
    /// <summary>Recovering after a loss.</summary>
    [CName("NV_TILE_RECOVERING")] Recovering = 2,
    /// <summary>The stream cannot be decoded.</summary>
    [CName("NV_TILE_UNDECODABLE")] Undecodable = 3,
}

/// <summary>A device's stream tier.</summary>
public enum NvStreamTier
{
    /// <summary>Main stream.</summary>
    [CName("NV_TIER_MAIN")] Main = 0,
    /// <summary>Sub stream.</summary>
    [CName("NV_TIER_SUB")] Sub = 1,
    /// <summary>Mobile stream.</summary>
    [CName("NV_TIER_MOBILE")] Mobile = 2,
    /// <summary>Extern stream.</summary>
    [CName("NV_TIER_EXTERN")] Extern = 4,
}

/// <summary>Why a platform decoder could not answer a Configure or a Decode.</summary>
public enum NvDecodeFailure
{
    /// <summary>Answers Configure.</summary>
    [CName("NV_FAIL_INVALID_PARAMETER_SETS")] InvalidParameterSets = 0,
    /// <summary>Answers Configure.</summary>
    [CName("NV_FAIL_SESSION_CREATE_FAILED")] SessionCreateFailed = 1,
    /// <summary>Decode before a successful Configure.</summary>
    [CName("NV_FAIL_NOT_CONFIGURED")] NotConfigured = 2,
    /// <summary>The access unit cannot be submitted.</summary>
    [CName("NV_FAIL_INVALID_ACCESS_UNIT")] InvalidAccessUnit = 3,
    /// <summary>Submitting failed.</summary>
    [CName("NV_FAIL_SUBMIT_FAILED")] SubmitFailed = 4,
    /// <summary>The decoder reported an error.</summary>
    [CName("NV_FAIL_DECODE_ERROR")] DecodeError = 5,
    /// <summary>The decoder dropped the picture; still an answer.</summary>
    [CName("NV_FAIL_NO_PICTURE")] NoPicture = 6,
}

/// <summary>What a cover thumbnail's bytes are (minor 5).</summary>
public enum NvThumbKind
{
    /// <summary>An H.264 keyframe, Annex-B, parameter sets included.</summary>
    [CName("NV_THUMB_H264_KEYFRAME")] H264Keyframe = 1,
    /// <summary>An H.265 keyframe, Annex-B, parameter sets included.</summary>
    [CName("NV_THUMB_H265_KEYFRAME")] H265Keyframe = 2,
    /// <summary>The app's own small image (for example JPEG).</summary>
    [CName("NV_THUMB_IMAGE")] Image = 3,
}

/// <summary>How a seek shows its target (minor 6).</summary>
public enum NvSeekMode
{
    /// <summary>The keyframe at or before the position first (fast; scrubbing).</summary>
    [CName("NV_SEEK_KEYFRAME")] Keyframe = 0,
    /// <summary>Decode from that keyframe and show the picture at the position.</summary>
    [CName("NV_SEEK_EXACT")] Exact = 1,
}

/// <summary>A playback's state (minor 6).</summary>
public enum NvPlaybackState
{
    /// <summary>Playing, but the clock waits for a camera.</summary>
    [CName("NV_PLAYBACK_BUFFERING")] Buffering = 0,
    /// <summary>Playing.</summary>
    [CName("NV_PLAYBACK_PLAYING")] Playing = 1,
    /// <summary>Paused.</summary>
    [CName("NV_PLAYBACK_PAUSED")] Paused = 2,
    /// <summary>At the end of the range or of the recordings.</summary>
    [CName("NV_PLAYBACK_ENDED")] Ended = 3,
    /// <summary>Every camera failed.</summary>
    [CName("NV_PLAYBACK_FAILED")] Failed = 4,
}

/// <summary>A cover thumbnail's description (minor 5).</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvThumbnail
{
    /// <summary>What the bytes are.</summary>
    public NvThumbKind Kind;
    /// <summary>Width of the picture the bytes decode to (0 when unknown).</summary>
    public uint Width;
    /// <summary>Height (0 when unknown).</summary>
    public uint Height;
    /// <summary>The keyframe's stream timestamp (0 for an image that did not keep it).</summary>
    public long PtsUs;
}

/// <summary>Core configuration; read only during <c>nv_core_new</c>.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvConfig
{
    /// <summary>Directory for rotated log files (UTF-8, NUL-terminated); null = no file log.</summary>
    public byte* LogDir;
    /// <summary>0 = default.</summary>
    public uint LogMaxFiles;
    /// <summary>0 = default.</summary>
    public ulong LogMaxTotalBytes;
    /// <summary>Data directory for layouts and device keys (UTF-8); null = in memory only.</summary>
    public byte* DataDir;
}

/// <summary>Borrowed bytes: valid only until the callback that received them returns.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvBytes
{
    /// <summary>First byte.</summary>
    public byte* Data;
    /// <summary>Length in bytes.</summary>
    public nuint Len;

    /// <summary>The bytes, valid only during the callback.</summary>
    public readonly ReadOnlySpan<byte> AsSpan() => new(Data, checked((int)Len));
}

/// <summary>A size in pixels.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvSize
{
    /// <summary>Width.</summary>
    public uint Width;
    /// <summary>Height.</summary>
    public uint Height;

    /// <summary>Creates a size.</summary>
    public NvSize(uint width, uint height) => (Width, Height) = (width, height);
}

/// <summary>Pixels, origin at the top left.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvRect
{
    /// <summary>Left.</summary>
    public uint X;
    /// <summary>Top.</summary>
    public uint Y;
    /// <summary>Width.</summary>
    public uint Width;
    /// <summary>Height.</summary>
    public uint Height;
}

/// <summary>Color description of a stream.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvColor
{
    /// <summary>Matrix.</summary>
    public NvColorMatrix Matrix;
    /// <summary>The decoder must output this range.</summary>
    public CBool FullRange;
    /// <summary>Row-major 3x4: (R, G, B) = M * (Y, Cb, Cr, 1), samples as code/255.</summary>
    public fixed float YcbcrToRgb[12];
}

/// <summary>Configure: create a decoder for a generation.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvConfigure
{
    /// <summary>Codec.</summary>
    public NvCodec Codec;
    /// <summary>VPS, SPS, PPS order; NAL units without start codes.</summary>
    public NvBytes* ParameterSets;
    /// <summary>Number of parameter sets.</summary>
    public nuint ParameterSetCount;
    /// <summary>Coded picture size.</summary>
    public NvSize CodedSize;
    /// <summary>Inside the coded picture.</summary>
    public NvRect VisibleRect;
    /// <summary>Color.</summary>
    public NvColor Color;

    /// <summary>The parameter sets, valid only during the callback.</summary>
    public readonly ReadOnlySpan<NvBytes> ParameterSetSpan =>
        ParameterSets == null ? default : new(ParameterSets, checked((int)ParameterSetCount));
}

/// <summary>Decode: one access unit.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvDecode
{
    /// <summary>One picture, Annex-B, parameter sets possibly in band.</summary>
    public NvBytes AccessUnit;
    /// <summary>The access unit is a keyframe.</summary>
    public CBool Keyframe;
    /// <summary>Presentation time, microseconds.</summary>
    public long PtsUs;
}

/// <summary>
/// One instruction for a tile's platform decoder or view. Read only the member
/// that matches <see cref="Kind"/>; Flush, Reset, and Clear carry no data.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvAction
{
    /// <summary>Which member is valid.</summary>
    public NvActionKind Kind;
    /// <summary>For Configure.</summary>
    public NvConfigure Configure;
    /// <summary>For Decode.</summary>
    public NvDecode Decode;
    /// <summary>For Status.</summary>
    public NvTileStatus Status;
    /// <summary>The tile's stream generation; answer with it.</summary>
    public uint Generation;
}

/// <summary>One block of a tile's tapped audio: 16 kHz mono, borrowed for the callback.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvAudioBlock
{
    /// <summary>Samples in -1..1.</summary>
    public float* Samples;
    /// <summary>Number of samples.</summary>
    public nuint SampleCount;
    /// <summary>Always 16000.</summary>
    public uint SampleRate;
    /// <summary>Numbering and camera times restart with each epoch.</summary>
    public uint Epoch;
    /// <summary>Number of the first sample in this epoch, at 16 kHz.</summary>
    public ulong FirstSample;
    /// <summary>Camera time of the first sample, if <see cref="HasCameraTime"/>.</summary>
    public long CameraTimeUs;
    /// <summary>Arrival (microseconds since the Unix epoch), if <see cref="HasWallClock"/>.</summary>
    public long WallClockUs;
    /// <summary><see cref="CameraTimeUs"/> is valid.</summary>
    public CBool HasCameraTime;
    /// <summary><see cref="WallClockUs"/> is valid.</summary>
    public CBool HasWallClock;
    /// <summary>Audio is missing or restarted before this block.</summary>
    public CBool Discontinuity;

    /// <summary>The samples, valid only during the callback.</summary>
    public readonly ReadOnlySpan<float> SampleSpan =>
        Samples == null ? default : new(Samples, checked((int)SampleCount));
}

/// <summary>The callbacks passed by value to <c>nv_core_new</c>.</summary>
[StructLayout(LayoutKind.Sequential)]
public unsafe struct NvCallbacks
{
    /// <summary>Passed back to every callback.</summary>
    public void* Context;
    /// <summary>NvActionCallback.</summary>
    public delegate* unmanaged[Cdecl]<void*, ulong, NvAction*, void> OnAction;
    /// <summary>NvEventCallback: UTF-8 JSON, not NUL-terminated.</summary>
    public delegate* unmanaged[Cdecl]<void*, byte*, nuint, void> OnEvent;
    /// <summary>NvAudioTapCallback; may be null.</summary>
    public delegate* unmanaged[Cdecl]<void*, ulong, NvAudioBlock*, void> OnAudioTap;
}

/// <summary>Grid layout of one tile: its cell, where to draw, and what to sample.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NvTileLayout
{
    /// <summary>The tile's cell in the viewport.</summary>
    public NvRect Cell;
    /// <summary>Where to draw.</summary>
    public NvRect Dest;
    /// <summary>What to sample, in visible-picture pixels.</summary>
    public NvRect Source;
}

/// <summary>
/// The raw functions of nvrview.h. Prefer <see cref="NvCore"/>, which adds
/// handle lifetime, callback dispatch, and error checking. The library is
/// <c>nvrview.dll</c> on Windows and <c>libnvrview.so</c> on Linux.
/// </summary>
public static unsafe partial class NvNative
{
    /// <summary>The library name the runtime resolves.</summary>
    public const string Library = "nvrview";

    /// <summary>NV_ABI_VERSION of the header these bindings were written for.</summary>
    public const uint AbiVersion = 3;

    /// <summary>NV_ABI_MINOR of the header these bindings were written for.</summary>
    public const uint AbiMinor = 7;

    /// <summary>The library's ABI version.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial uint nv_abi_version();

    /// <summary>The library's minor version: additions to its ABI version.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial uint nv_abi_minor();

    /// <summary>Starts the core.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_core_new(NvConfig* config, NvCallbacks callbacks, nint* core);

    /// <summary>Stops and frees the core; never from a callback.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial void nv_core_free(nint core);

    /// <summary>Adds a device. Strings are UTF-8, NUL-terminated.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_device_add(
        nint core, byte* host, ushort port, byte* user, byte* password, ulong* device);

    /// <summary>Closes a device's session.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_device_remove(nint core, ulong device);

    /// <summary>Opens a tile.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_open(
        nint core, ulong device, byte channel, NvStreamTier tier, ulong* tile);

    /// <summary>Closes a tile; never from a callback.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_close(nint core, ulong tile);

    /// <summary>Switches a tile's stream tier.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_set_tier(nint core, ulong tile, NvStreamTier tier);

    /// <summary>Retries a tile without a picture (minor 3): restarts its stream now.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_retry(nint core, ulong tile);

    /// <summary>Answers a Decode; any thread, including callbacks.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_decoded(nint core, ulong tile, uint generation, long ptsUs);

    /// <summary>Answers a Decode or Configure with a failure; any thread.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_decode_failed(
        nint core, ulong tile, uint generation, NvDecodeFailure kind);

    /// <summary>Now on the core's clock, nanoseconds.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial ulong nv_clock_now_ns();

    /// <summary>Sets the audio output format.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_audio_set_output(nint core, uint sampleRate, uint channels);

    /// <summary>Renders the mix of audible tiles; real-time safe.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_audio_render(
        nint core, float* output, uint frames, uint channels, ulong hostTimeNs);

    /// <summary>Makes a tile audible or silent.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_audio_set(
        nint core, ulong tile, [MarshalAs(UnmanagedType.U1)] bool audible, float volume);

    /// <summary>Starts or stops a tile's audio tap.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_audio_tap(
        nint core, ulong tile, [MarshalAs(UnmanagedType.U1)] bool enabled);

    /// <summary>Peak and RMS level of a tile's audio; any thread, lock-free.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_tile_audio_level(nint core, ulong tile, float* peak, float* rms);

    /// <summary>Allows or forbids talk-back to a channel (camera motion control; off by default).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_set_permission(
        nint core, ulong device, byte channel, [MarshalAs(UnmanagedType.U1)] bool allowed);

    /// <summary>Starts talk-back to a tile's camera; progress arrives as talk events.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_start(nint core, ulong tile);

    /// <summary>Queues mono samples (any rate the core accepts) for the tile's talk-back.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_push(nint core, ulong tile, float* samples, nuint count, uint sampleRate);

    /// <summary>Stops talk-back, after sending what is queued and a short silent tail when drain is true.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_stop(nint core, ulong tile, [MarshalAs(UnmanagedType.U1)] bool drain);

    /// <summary>Marks the tile's camera as in an open call, or no longer (minor 4): no per-minute talk-back limit while marked.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_set_conversation(nint core, ulong tile, [MarshalAs(UnmanagedType.U1)] bool open);

    /// <summary>The device's stable key into a buffer.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_device_key(nint core, ulong device, byte* output, nuint cap, nuint* len);

    /// <summary>Every stored layout as JSON into a buffer.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_layouts_get(nint core, byte* output, nuint cap, nuint* len);

    /// <summary>Inserts or replaces one layout.</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_layout_put(nint core, string json);

    /// <summary>Deletes a layout by id.</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_layout_delete(nint core, string id);

    /// <summary>
    /// Starts a search for devices on the local network (minor 1). The JSON
    /// may be null for the defaults.
    /// </summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_discover_start(nint core, string? json);

    /// <summary>Stops the running search, if any (minor 1).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_discover_stop(nint core);

    /// <summary>Starts an update check (minor 2; docs/FFI.md, "Updates").</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_update_check(nint core, string json);

    /// <summary>Downloads, verifies, and unpacks the accepted update (minor 2).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_update_download(nint core, string json);

    /// <summary>Cancels the running update operation, if any (minor 2).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_update_cancel(nint core);

    /// <summary>Parses an update public key (pure; minor 2).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_update_parse_key(byte* text, byte* output, nuint cap, nuint* len);

    /// <summary>Checks an update source as typed (pure; minor 2).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_update_parse_source(byte* text, byte* output, nuint cap, nuint* len);

    /// <summary>Grid layout (pure; no core needed).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_grid_layout(
        uint tileCount, NvSize viewport, uint gap, NvSize* pictureSizes,
        [MarshalAs(UnmanagedType.U1)] bool fill, NvTileLayout* output);

    // Recordings, part 1 (minor 5; docs/FFI.md, "Recordings (minor 5)").
    // JSON goes in as UTF-8 strings; answers come back through the
    // out-buffer convention (output, cap, len).

    /// <summary>Sets the recordings configuration (JSON; minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_configure(nint core, string json);

    /// <summary>Allows (json non-null) or stops (null) indexing a device by its key (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_enable(nint core, string deviceKey, string? json);

    /// <summary>Days with recordings per camera (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_calendar(nint core, string query, byte* output, nuint cap, nuint* len);

    /// <summary>Recording spans per camera (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_spans(nint core, string query, byte* output, nuint cap, nuint* len);

    /// <summary>Events matching a query (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_events(nint core, string query, byte* output, nuint cap, nuint* len);

    /// <summary>Asks for cameras' days to be indexed; returns at once (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_want(nint core, string json);

    /// <summary>Keeps today fresh for a hold's cameras; null json releases the hold (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_hold_today(nint core, ulong hold, string? json);

    /// <summary>Index state of every enabled device (minor 5).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_recordings_status(nint core, byte* output, nuint cap, nuint* len);

    /// <summary>Incidents overlapping a query's range, newest first (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_incidents_get(nint core, string query, byte* output, nuint cap, nuint* len);

    /// <summary>One incident by key (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_incident_get(nint core, string key, byte* output, nuint cap, nuint* len);

    /// <summary>Applies and stores one incident edit; changes state even when the answer does not fit (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_incident_edit(nint core, string edit, byte* output, nuint cap, nuint* len);

    /// <summary>Locks or unlocks an incident; changes state even when the answer does not fit (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_incident_lock(
        nint core, string key, [MarshalAs(UnmanagedType.U1)] bool locked, byte* output, nuint cap, nuint* len);

    /// <summary>Camera groups and name overrides (minor 5).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_camera_groups_get(nint core, byte* output, nuint cap, nuint* len);

    /// <summary>Replaces the camera groups and name overrides (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_camera_groups_put(nint core, string json);

    /// <summary>A cached cover thumbnail; NV_PENDING while it is fetched (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_thumbnail_get(
        nint core, string key, NvThumbnail* info, byte* output, nuint cap, nuint* len);

    /// <summary>Replaces a cached thumbnail (usually with the app's small image; minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_thumbnail_put(nint core, string key, NvThumbnail* info, byte* data, nuint len);

    /// <summary>Drops queued thumbnail fetches of one key, or all for null (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_thumbnail_cancel(nint core, string? key);

    /// <summary>Queues an incident's download; changes state even when the answer does not fit (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_download_incident(nint core, string request, byte* output, nuint cap, nuint* len);

    /// <summary>Pauses or resumes a download, or one camera of it (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_download_pause(
        nint core, string download, string? camera, [MarshalAs(UnmanagedType.U1)] bool paused);

    /// <summary>Retries a download's failed cameras, or one (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_download_retry(nint core, string download, string? camera);

    /// <summary>Forgets a download with no job running (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_download_forget(
        nint core, string download, [MarshalAs(UnmanagedType.U1)] bool deletePartial);

    /// <summary>Hands a download's folder to the app for the Recycle Bin (minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_download_trash(nint core, string download, byte* output, nuint cap, nuint* len);

    /// <summary>Every known download (minor 5).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_downloads_get(nint core, byte* output, nuint cap, nuint* len);

    /// <summary>Live sound settings (JSON; minor 5).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_sound_configure(nint core, string json);

    // Recorded playback (minor 6; docs/FFI.md, "Recorded playback (minor 6)").

    /// <summary>Opens a playback, paused; one tile ID per camera into tiles (minor 6).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_open(
        nint core, string json, ulong* playback, ulong* tiles, nuint tileCap, nuint* tileCount);

    /// <summary>Adds a camera at the current position (minor 6).</summary>
    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_add_camera(nint core, ulong playback, string json, ulong* tile);

    /// <summary>Removes one camera; its tile closes (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_remove_camera(nint core, ulong playback, ulong tile);

    /// <summary>Starts the shared clock (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_play(nint core, ulong playback);

    /// <summary>Holds the shared clock (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_pause(nint core, ulong playback);

    /// <summary>Moves the shared clock; returns at once (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_seek(nint core, ulong playback, long positionMs, NvSeekMode mode);

    /// <summary>0.25, 0.5, 1, 2, 4, 8, or 16 (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_set_rate(nint core, ulong playback, double rate);

    /// <summary>This camera shows position + offsetMs (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_set_offset(nint core, ulong playback, ulong tile, long offsetMs);

    /// <summary>Position and state for the scrubber; also inside callbacks (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_position(
        nint core, ulong playback, long* positionMs, NvPlaybackState* state);

    /// <summary>The playback's details (JSON; minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_status(nint core, ulong playback, byte* output, nuint cap, nuint* len);

    /// <summary>Closes every tile, then playback_closed (minor 6).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_playback_close(nint core, ulong playback);
}
