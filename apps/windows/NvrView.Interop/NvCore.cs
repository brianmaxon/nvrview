// Safe wrappers over NvNative: the core's lifetime as a SafeHandle, callbacks
// dispatched to managed code without ever letting an exception reach native
// code, UTF-8 strings, and the out/cap/len buffer loop (docs/FFI.md).

using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;

namespace NvrView.Interop;

/// <summary>A call into the core returned an error status.</summary>
public sealed class NvException(NvStatus status, string function, string? detail = null)
    : Exception($"{function} returned {status}" + (detail is null ? "" : $" ({detail})"))
{
    /// <summary>The status the core returned.</summary>
    public NvStatus Status { get; } = status;

    /// <summary>
    /// What the answer said about the error, when it says anything: for
    /// <see cref="NvStatus.Limit"/> from <c>nv_record_start</c>, which limit
    /// (<c>live</c>, <c>calls</c>, <c>main_streams</c>, <c>disk_low</c>).
    /// </summary>
    public string? Detail { get; } = detail;

    /// <summary>The C function that returned it.</summary>
    public string Function { get; } = function;
}

/// <summary>
/// The library's ABI version is not the one these bindings were written for,
/// or its minor version is older than they need.
/// </summary>
public sealed class NvAbiMismatchException(uint found, uint foundMinor = uint.MaxValue)
    : Exception(
        foundMinor == uint.MaxValue
            ? $"nvrview ABI {found}, bindings expect {NvNative.AbiVersion}"
            : $"nvrview ABI {found}.{foundMinor}, bindings expect {NvNative.AbiVersion}.{NvNative.AbiMinor} or newer")
{
    /// <summary>What <c>nv_abi_version()</c> returned.</summary>
    public uint Found { get; } = found;
}

/// <summary>Options read once when the core starts.</summary>
public sealed class NvCoreOptions
{
    /// <summary>Directory for rotated log files; null = no file log.</summary>
    public string? LogDir { get; init; }

    /// <summary>0 = the core's default.</summary>
    public uint LogMaxFiles { get; init; }

    /// <summary>0 = the core's default.</summary>
    public ulong LogMaxTotalBytes { get; init; }

    /// <summary>Directory for layouts and device keys; null = in memory only.</summary>
    public string? DataDir { get; init; }

    /// <summary>Register on_audio_tap, so taps can be enabled per tile.</summary>
    public bool AudioTap { get; init; }
}

/// <summary>
/// Receives the core's callbacks on threads the core owns (docs/FFI.md,
/// "Threads"). Return quickly. From inside a callback call only
/// <see cref="NvCore.Decoded"/>, <see cref="NvCore.DecodeFailed"/>, and
/// <see cref="NvCore.TileAudioLevel"/>. Exceptions thrown here are caught at
/// the boundary and reported through <see cref="OnCallbackException"/>.
/// </summary>
public abstract class NvCoreCallbacks
{
    /// <summary>One action for a tile; serialized per tile. Pointers inside are borrowed.</summary>
    public abstract void OnAction(NvCore core, ulong tile, in NvAction action);

    /// <summary>One event as UTF-8 JSON, borrowed; serialized across the core.</summary>
    public abstract void OnEvent(NvCore core, ReadOnlySpan<byte> json);

    /// <summary>One tapped audio block (only with <see cref="NvCoreOptions.AudioTap"/>).</summary>
    public virtual void OnAudioTap(NvCore core, ulong tile, in NvAudioBlock block)
    {
    }

    /// <summary>
    /// A callback threw. Called on the callback's thread; must not throw
    /// (anything it throws is dropped).
    /// </summary>
    public virtual void OnCallbackException(Exception exception)
    {
    }
}

/// <summary>
/// Owns an <c>NvCore*</c>: <c>nv_core_free</c> runs once, from Dispose or
/// the finalizer, and the callback context outlives it.
/// </summary>
internal sealed class NvCoreHandle : SafeHandle
{
    private readonly Dispatcher _dispatcher;
    private GCHandle _context;

    public NvCoreHandle(nint core, GCHandle context, Dispatcher dispatcher)
        : base(0, ownsHandle: true)
    {
        _dispatcher = dispatcher;
        _context = context;
        SetHandle(core);
    }

    public override bool IsInvalid => handle == 0;

    protected override bool ReleaseHandle()
    {
        // Callbacks may still run (and answer decodes) until nv_core_free
        // returns; they reach the core through the dispatcher's raw pointer.
        nint core = handle;
        NvNative.nv_core_free(core);
        Volatile.Write(ref _dispatcher.Raw, 0);
        // No callback runs after nv_core_free returns, so the context may go,
        // and so may the encoders the core was given.
        _context.Free();
        _dispatcher.FreeEncoders();
        return true;
    }
}

/// <summary>The object behind every callback's context pointer.</summary>
internal sealed unsafe partial class Dispatcher(NvCoreCallbacks callbacks)
{
    [ThreadStatic] private static int t_depth;

    public readonly NvCoreCallbacks Callbacks = callbacks;
    public NvCore? Owner;
    public nint Raw;
    public int ExceptionCount;
    public Exception? FirstException;

    public static bool InCallback => t_depth > 0;

    // Exceptions from a context that cannot be resolved (should never happen).
    private static int s_orphanExceptions;

    public static int OrphanExceptions => Volatile.Read(ref s_orphanExceptions);

    private static Dispatcher? From(void* context)
    {
        try
        {
            return GCHandle.FromIntPtr((nint)context).Target as Dispatcher;
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    internal void Report(Exception e)
    {
        Interlocked.CompareExchange(ref FirstException, e, null);
        Interlocked.Increment(ref ExceptionCount);
        try
        {
            Callbacks.OnCallbackException(e);
        }
        catch
        {
            // The handler must not throw; nothing may cross into native code.
        }
    }

    private static void Fail(void* context, Exception e)
    {
        if (From(context) is { } d)
        {
            d.Report(e);
        }
        else
        {
            Interlocked.Increment(ref s_orphanExceptions);
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static void OnAction(void* context, ulong tile, NvAction* action)
    {
        t_depth++;
        try
        {
            Dispatcher d = From(context) ?? throw new InvalidOperationException("no dispatcher");
            d.Callbacks.OnAction(d.Owner!, tile, in *action);
        }
        catch (Exception e)
        {
            Fail(context, e);
        }
        finally
        {
            t_depth--;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static void OnEvent(void* context, byte* json, nuint len)
    {
        t_depth++;
        try
        {
            Dispatcher d = From(context) ?? throw new InvalidOperationException("no dispatcher");
            d.Callbacks.OnEvent(d.Owner!, new ReadOnlySpan<byte>(json, checked((int)len)));
        }
        catch (Exception e)
        {
            Fail(context, e);
        }
        finally
        {
            t_depth--;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    public static void OnAudioTap(void* context, ulong tile, NvAudioBlock* block)
    {
        t_depth++;
        try
        {
            Dispatcher d = From(context) ?? throw new InvalidOperationException("no dispatcher");
            d.Callbacks.OnAudioTap(d.Owner!, tile, in *block);
        }
        catch (Exception e)
        {
            Fail(context, e);
        }
        finally
        {
            t_depth--;
        }
    }
}

/// <summary>
/// A running nvrview core (docs/FFI.md). Dispose it (never from a callback)
/// to stop every session; no callback runs after Dispose returns.
/// </summary>
public sealed unsafe partial class NvCore : IDisposable
{
    private readonly Dispatcher _dispatcher;
    private NvCoreHandle? _handle;

    private NvCore(NvCoreCallbacks callbacks)
    {
        _dispatcher = new Dispatcher(callbacks) { Owner = this };
    }

    /// <summary>Callbacks that threw (caught at the boundary).</summary>
    public int CallbackExceptionCount => Volatile.Read(ref _dispatcher.ExceptionCount);

    /// <summary>The first exception a callback threw, if any.</summary>
    public Exception? FirstCallbackException => Volatile.Read(ref _dispatcher.FirstException);

    /// <summary>True on a thread that is inside one of this library's callbacks.</summary>
    public static bool InCallback => Dispatcher.InCallback;

    /// <summary>
    /// Checks the ABI version and starts a core. Throws
    /// <see cref="NvAbiMismatchException"/> on a mismatch.
    /// </summary>
    public static NvCore Create(NvCoreOptions options, NvCoreCallbacks callbacks)
    {
        ArgumentNullException.ThrowIfNull(options);
        ArgumentNullException.ThrowIfNull(callbacks);
        uint abi = NvNative.nv_abi_version();
        if (abi != NvNative.AbiVersion)
        {
            throw new NvAbiMismatchException(abi);
        }

        // Additions keep the version and raise the minor: the bindings call
        // functions of NvNative.AbiMinor, which an older core does not export.
        uint minor = NvNative.nv_abi_minor();
        if (minor < NvNative.AbiMinor)
        {
            throw new NvAbiMismatchException(abi, minor);
        }

        var core = new NvCore(callbacks);
        GCHandle context = GCHandle.Alloc(core._dispatcher);
        try
        {
            byte[]? logDir = Utf8.NulTerminated(options.LogDir);
            byte[]? dataDir = Utf8.NulTerminated(options.DataDir);
            nint raw = 0;
            NvStatus status;
            fixed (byte* log = logDir)
            fixed (byte* data = dataDir)
            {
                var config = new NvConfig
                {
                    LogDir = log,
                    LogMaxFiles = options.LogMaxFiles,
                    LogMaxTotalBytes = options.LogMaxTotalBytes,
                    DataDir = data,
                };
                var cb = new NvCallbacks
                {
                    Context = (void*)GCHandle.ToIntPtr(context),
                    OnAction = &Dispatcher.OnAction,
                    OnEvent = &Dispatcher.OnEvent,
                    OnAudioTap = options.AudioTap ? &Dispatcher.OnAudioTap : null,
                };
                // No action can arrive before a tile exists, so Raw is set
                // before any callback could need it.
                status = NvNative.nv_core_new(&config, cb, &raw);
            }
            Check(status, nameof(NvNative.nv_core_new));
            if (raw == 0)
            {
                throw new NvException(NvStatus.Internal, nameof(NvNative.nv_core_new));
            }
            Volatile.Write(ref core._dispatcher.Raw, raw);
            core._handle = new NvCoreHandle(raw, context, core._dispatcher);
            return core;
        }
        catch
        {
            if (core._handle is null)
            {
                context.Free();
            }
            throw;
        }
    }

    /// <summary>
    /// Frees the core (<c>nv_core_free</c>): stops every preview, closes every
    /// session, joins every core thread. First stop audio rendering and any
    /// decoder thread that answers outside a callback: no other call may
    /// start once nv_core_free has begun (callbacks still running may answer).
    /// </summary>
    public void Dispose()
    {
        if (Dispatcher.InCallback)
        {
            // nv_core_free would wait on the thread it runs on.
            throw new InvalidOperationException("NvCore.Dispose called from a callback");
        }
        _handle?.Dispose();
    }

    /// <summary>The library's ABI version (no core needed).</summary>
    public static uint AbiVersion() => NvNative.nv_abi_version();

    /// <summary>The library's minor version (no core needed).</summary>
    public static uint AbiMinor() => NvNative.nv_abi_minor();

    /// <summary>Now on the core's clock, nanoseconds.</summary>
    public static ulong ClockNowNs() => NvNative.nv_clock_now_ns();

    /// <summary>Adds a device; connecting starts in the background. Returns its ID.</summary>
    public ulong AddDevice(string host, ushort port, string user, ReadOnlySpan<char> password)
    {
        NotInCallback();
        byte[] h = Utf8.NulTerminated(host)!;
        byte[] u = Utf8.NulTerminated(user)!;
        // The password's UTF-8 copy is cleared as soon as the core has copied it.
        byte[] p = new byte[Encoding.UTF8.GetByteCount(password) + 1];
        try
        {
            Encoding.UTF8.GetBytes(password, p);
            ulong device = 0;
            NvStatus s;
            using (AcquireLease(out nint core))
            {
                fixed (byte* hp = h)
                fixed (byte* up = u)
                fixed (byte* pp = p)
                {
                    s = NvNative.nv_device_add(core, hp, port, up, pp, &device);
                }
            }
            Check(s, nameof(NvNative.nv_device_add));
            return device;
        }
        finally
        {
            CryptographicClear(p);
        }
    }

    /// <summary>Closes the device's session (its tiles end first).</summary>
    public void RemoveDevice(ulong device)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_device_remove(core, device), nameof(NvNative.nv_device_remove));
        }
    }

    /// <summary>Opens a tile; actions for it start on <see cref="NvCoreCallbacks.OnAction"/>.</summary>
    public ulong OpenTile(ulong device, byte channel, NvStreamTier tier)
    {
        NotInCallback();
        ulong tile = 0;
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_open(core, device, channel, tier, &tile), nameof(NvNative.nv_tile_open));
        }
        return tile;
    }

    /// <summary>Closes a tile; no action for it runs after this returns.</summary>
    public void CloseTile(ulong tile)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_close(core, tile), nameof(NvNative.nv_tile_close));
        }
    }

    /// <summary>Asks a tile to show another tier; progress arrives as tile_tier events.</summary>
    public void SetTier(ulong tile, NvStreamTier tier)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_set_tier(core, tile, tier), nameof(NvNative.nv_tile_set_tier));
        }
    }

    /// <summary>
    /// The Retry button of a tile without a picture (minor 3): the core restarts
    /// the tile's stream now, or connects now when the device is waiting to. It
    /// ignores a second ask within 2 s and does nothing for a refused login.
    /// </summary>
    public void RetryTile(ulong tile)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_retry(core, tile), nameof(NvNative.nv_tile_retry));
        }
    }

    /// <summary>
    /// Answers a Decode of <paramref name="generation"/>. Any thread, including
    /// callbacks; never blocks or throws. Returns the core's status
    /// (<see cref="NvStatus.Closed"/> once the core is freed).
    /// </summary>
    public NvStatus Decoded(ulong tile, uint generation, long ptsUs)
    {
        nint core = Volatile.Read(ref _dispatcher.Raw);
        return core == 0 ? NvStatus.Closed : NvNative.nv_tile_decoded(core, tile, generation, ptsUs);
    }

    /// <summary>Answers a Decode or Configure with a failure. Same rules as <see cref="Decoded"/>.</summary>
    public NvStatus DecodeFailed(ulong tile, uint generation, NvDecodeFailure kind)
    {
        nint core = Volatile.Read(ref _dispatcher.Raw);
        return core == 0 ? NvStatus.Closed : NvNative.nv_tile_decode_failed(core, tile, generation, kind);
    }

    /// <summary>
    /// Peak and RMS level (0..1) of a tile's decoded audio. Any thread,
    /// including callbacks; lock-free. Both 0 for an unknown tile or once freed.
    /// </summary>
    public (float Peak, float Rms) TileAudioLevel(ulong tile)
    {
        nint core = Volatile.Read(ref _dispatcher.Raw);
        float peak = 0, rms = 0;
        if (core != 0)
        {
            Check(NvNative.nv_tile_audio_level(core, tile, &peak, &rms), nameof(NvNative.nv_tile_audio_level));
        }
        return (peak, rms);
    }

    /// <summary>Sets the format the app renders audio in.</summary>
    public void SetAudioOutput(uint sampleRate, uint channels)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_audio_set_output(core, sampleRate, channels), nameof(NvNative.nv_audio_set_output));
        }
    }

    /// <summary>
    /// Fills <paramref name="output"/> (interleaved, a whole number of frames)
    /// with the mix of audible tiles. For the real-time audio thread: no
    /// allocation, no lock, no throw. Stop rendering before Dispose.
    /// </summary>
    public NvStatus RenderAudio(Span<float> output, uint channels, ulong hostTimeNs)
    {
        nint core = Volatile.Read(ref _dispatcher.Raw);
        if (core == 0 || channels == 0)
        {
            output.Clear();
            return core == 0 ? NvStatus.Closed : NvStatus.InvalidArgument;
        }
        uint frames = (uint)output.Length / channels;
        fixed (float* p = output)
        {
            return NvNative.nv_audio_render(core, p, frames, channels, hostTimeNs);
        }
    }

    /// <summary>Makes a tile audible (claims a playout slot) or silent.</summary>
    public void SetTileAudio(ulong tile, bool audible, float volume)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_audio_set(core, tile, audible, volume), nameof(NvNative.nv_tile_audio_set));
        }
    }

    /// <summary>Starts or stops a tile's audio tap (needs <see cref="NvCoreOptions.AudioTap"/>).</summary>
    public void SetTileAudioTap(ulong tile, bool enabled)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_tile_audio_tap(core, tile, enabled), nameof(NvNative.nv_tile_audio_tap));
        }
    }

    /// <summary>
    /// Allows or forbids talk-back to one channel of a device for this run of the core (camera
    /// motion control: off until the user allows it); withdrawing stops a running talk.
    /// </summary>
    public void TalkSetPermission(ulong device, byte channel, bool allowed)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_talk_set_permission(core, device, channel, allowed), nameof(NvNative.nv_talk_set_permission));
        }
    }

    /// <summary>
    /// Asks to talk to the tile's camera; progress arrives as <c>talk</c> events. Without the
    /// permission this throws <see cref="NvStatus.NotPermitted"/> and a <c>refused</c> event follows.
    /// </summary>
    public void TalkStart(ulong tile)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_talk_start(core, tile), nameof(NvNative.nv_talk_start));
        }
    }

    /// <summary>Queues mono samples (-1..1) at <paramref name="sampleRate"/> for the tile's talk. Not from a real-time audio thread.</summary>
    public void TalkPush(ulong tile, ReadOnlySpan<float> samples, uint sampleRate)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            fixed (float* p = samples)
            {
                Check(NvNative.nv_talk_push(core, tile, p, (nuint)samples.Length, sampleRate), nameof(NvNative.nv_talk_push));
            }
        }
    }

    /// <summary>
    /// Stops the tile's talk at once, or, when <paramref name="drain"/> is true, after the audio
    /// already pushed and the core's 1.5 s silent tail (audio pushed before the close continues it).
    /// </summary>
    public void TalkStop(ulong tile, bool drain)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_talk_stop(core, tile, drain), nameof(NvNative.nv_talk_stop));
        }
    }

    /// <summary>
    /// Tells the core a call is open on the tile's camera (<paramref name="open"/>), or no longer
    /// (minor 4). While it is, talk starts on that camera are not limited per minute; they still need
    /// the permission and are logged. The core also ends the mark when the tile closes or the
    /// permission is withdrawn.
    /// </summary>
    public void TalkSetConversation(ulong tile, bool open)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_talk_set_conversation(core, tile, open), nameof(NvNative.nv_talk_set_conversation));
        }
    }

    /// <summary>The device's stable key, for layouts.</summary>
    /// <param name="device">The device ID.</param>
    /// <param name="initialCapacity">First buffer size; grown on NV_ERR_BUFFER_TOO_SMALL.</param>
    public string DeviceKey(ulong device, int initialCapacity = 128) =>
        ReadString(
            (core, buf, cap, len) => NvNative.nv_device_key(core, device, buf, cap, len),
            nameof(NvNative.nv_device_key),
            initialCapacity);

    /// <summary>Every stored layout: <c>{"revision":N,"layouts":[...]}</c>.</summary>
    /// <param name="initialCapacity">First buffer size; grown on NV_ERR_BUFFER_TOO_SMALL.</param>
    public string LayoutsJson(int initialCapacity = 4096) =>
        ReadString(
            (core, buf, cap, len) => NvNative.nv_layouts_get(core, buf, cap, len),
            nameof(NvNative.nv_layouts_get),
            initialCapacity);

    /// <summary>Inserts or replaces (by "id") one layout.</summary>
    public void PutLayout(string json)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_layout_put(core, json), nameof(NvNative.nv_layout_put));
        }
    }

    /// <summary>Deletes a layout by id.</summary>
    public void DeleteLayout(string id)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_layout_delete(core, id), nameof(NvNative.nv_layout_delete));
        }
    }

    /// <summary>
    /// Starts a search for devices on the local network. Answers arrive as
    /// <c>discovered</c> events and the end as one <c>discovery_done</c>
    /// (docs/FFI.md, "Device discovery"). A search already running is stopped
    /// first. <paramref name="json"/> is null for the defaults, or
    /// <c>{"seconds":3,"interfaces":["192.0.2.5"],"onvif":true}</c>.
    /// </summary>
    public void DiscoverStart(string? json = null)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_discover_start(core, json), nameof(NvNative.nv_discover_start));
        }
    }

    /// <summary>Stops the running search, if any; its <c>discovery_done</c> (stopped) follows.</summary>
    public void DiscoverStop()
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_discover_stop(core), nameof(NvNative.nv_discover_stop));
        }
    }

    /// <summary>
    /// Starts an update check (docs/FFI.md, "Updates"); the outcome arrives as
    /// <c>update</c> events. Throws <see cref="NvStatus.Limit"/> while another
    /// update operation runs.
    /// </summary>
    public void UpdateCheck(string json)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_update_check(core, json), nameof(NvNative.nv_update_check));
        }
    }

    /// <summary>Downloads, verifies, and unpacks the accepted version into the request's <c>staging_dir</c>.</summary>
    public void UpdateDownload(string json)
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_update_download(core, json), nameof(NvNative.nv_update_download));
        }
    }

    /// <summary>Cancels the running update operation; its final event is queued before this returns.</summary>
    public void UpdateCancel()
    {
        NotInCallback();
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_update_cancel(core), nameof(NvNative.nv_update_cancel));
        }
    }

    /// <summary>
    /// Parses an update public key (pure): <c>{"key":...,"id":...}</c>, or null
    /// when the text is not one key.
    /// </summary>
    public static string? UpdateParseKey(string text)
    {
        try
        {
            return PureString(text, (t, b, c, l) => NvNative.nv_update_parse_key(t, b, c, l), nameof(NvNative.nv_update_parse_key));
        }
        catch (NvException e) when (e.Status == NvStatus.InvalidArgument)
        {
            return null;
        }
    }

    /// <summary>
    /// Checks an update source as typed (pure): <c>{"ok":true,"kind":...,"display":...}</c>
    /// or <c>{"ok":false,"reason":...,"message":...}</c>.
    /// </summary>
    public static string UpdateParseSource(string text) =>
        PureString(text, (t, b, c, l) => NvNative.nv_update_parse_source(t, b, c, l), nameof(NvNative.nv_update_parse_source));

    private delegate NvStatus PureStringCall(byte* text, byte* output, nuint cap, nuint* len);

    // The out/cap/len convention for functions that need no core.
    private static string PureString(string text, PureStringCall call, string function)
    {
        byte[] input = Encoding.UTF8.GetBytes(text + "\0");
        int cap = 512;
        for (int attempt = 0; attempt < 4; attempt++)
        {
            byte[] buffer = new byte[cap];
            nuint len = 0;
            NvStatus s;
            fixed (byte* t = input)
            fixed (byte* p = buffer)
            {
                s = call(t, p, (nuint)cap, &len);
            }
            if (s == NvStatus.Ok)
            {
                return Encoding.UTF8.GetString(buffer, 0, checked((int)len));
            }
            if (s != NvStatus.BufferTooSmall)
            {
                throw new NvException(s, function);
            }
            cap = checked((int)len + 1);
        }
        throw new NvException(NvStatus.BufferTooSmall, function);
    }

    /// <summary>
    /// Grid layout (pure). <paramref name="pictureSizes"/> may hold zero sizes
    /// for tiles with no picture yet; <paramref name="output"/> receives one
    /// entry per picture size.
    /// </summary>
    public static void GridLayout(
        NvSize viewport, uint gap, ReadOnlySpan<NvSize> pictureSizes, bool fill, Span<NvTileLayout> output)
    {
        if (output.Length < pictureSizes.Length)
        {
            throw new ArgumentException("output is shorter than pictureSizes", nameof(output));
        }
        fixed (NvSize* sizes = pictureSizes)
        fixed (NvTileLayout* outp = output)
        {
            Check(
                NvNative.nv_grid_layout((uint)pictureSizes.Length, viewport, gap, sizes, fill, outp),
                nameof(NvNative.nv_grid_layout));
        }
    }

    private delegate NvStatus StringCall(nint core, byte* output, nuint cap, nuint* len);

    // The out/cap/len convention: *len receives the length without the NUL;
    // NV_ERR_BUFFER_TOO_SMALL means call again with *len + 1 bytes. The value
    // can grow between calls (another window stores a layout), so loop.
    private string ReadString(StringCall call, string function, int initialCapacity)
    {
        NotInCallback();
        int cap = Math.Max(initialCapacity, 1);
        using (AcquireLease(out nint core))
        {
            for (int attempt = 0; attempt < 8; attempt++)
            {
                byte[] buffer = new byte[cap];
                nuint len = 0;
                NvStatus s;
                fixed (byte* p = buffer)
                {
                    s = call(core, p, (nuint)cap, &len);
                }
                if (s == NvStatus.Ok)
                {
                    return Encoding.UTF8.GetString(buffer, 0, checked((int)len));
                }
                if (s != NvStatus.BufferTooSmall)
                {
                    throw new NvException(s, function);
                }
                cap = checked((int)len + 1);
            }
        }
        throw new NvException(NvStatus.BufferTooSmall, function);
    }

    private static void Check(NvStatus status, string function)
    {
        if (status != NvStatus.Ok)
        {
            throw new NvException(status, function);
        }
    }

    private static void NotInCallback([CallerMemberName] string? caller = null)
    {
        if (Dispatcher.InCallback)
        {
            throw new InvalidOperationException($"NvCore.{caller} may not be called from a callback");
        }
    }

    private static void CryptographicClear(byte[] bytes) =>
        System.Security.Cryptography.CryptographicOperations.ZeroMemory(bytes);

    // Keeps the handle alive (and nv_core_free from starting) during a call.
    private Lease AcquireLease(out nint core)
    {
        NvCoreHandle handle = _handle ?? throw new ObjectDisposedException(nameof(NvCore));
        bool added = false;
        try
        {
            handle.DangerousAddRef(ref added);
        }
        catch (ObjectDisposedException)
        {
            throw new NvException(NvStatus.Closed, "NvCore");
        }
        core = handle.DangerousGetHandle();
        return new Lease(handle);
    }
}

internal readonly struct Lease(SafeHandle handle) : IDisposable
{
    public void Dispose() => handle.DangerousRelease();
}

internal static class Utf8
{
    public static byte[]? NulTerminated(string? s)
    {
        if (s is null)
        {
            return null;
        }
        if (s.Contains('\0', StringComparison.Ordinal))
        {
            throw new ArgumentException("string contains NUL");
        }
        byte[] b = new byte[Encoding.UTF8.GetByteCount(s) + 1];
        Encoding.UTF8.GetBytes(s, b);
        return b;
    }
}
