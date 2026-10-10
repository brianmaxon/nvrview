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

    /// <summary>The library's ABI version.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial uint nv_abi_version();

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

    /// <summary>Stops talk-back, after sending what is queued when drain is true.</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_talk_stop(nint core, ulong tile, [MarshalAs(UnmanagedType.U1)] bool drain);

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

    /// <summary>Grid layout (pure; no core needed).</summary>
    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NvStatus nv_grid_layout(
        uint tileCount, NvSize viewport, uint gap, NvSize* pictureSizes,
        [MarshalAs(UnmanagedType.U1)] bool fill, NvTileLayout* output);
}
