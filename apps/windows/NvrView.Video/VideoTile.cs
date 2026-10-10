// One core tile on Windows: carries out the core's actions for every
// generation of the tile (docs/FFI.md, "Switching a tile's stream tier")
// with one MfDecoder each, answers every Decode through the core, and
// exposes the picture of the generation being shown.
//
// Actions arrive on the core's thread for this tile, one at a time
// (NvCoreCallbacks.OnAction); the renderer reads ShownPicture from its own
// thread under the device lock. Nothing is allocated per frame.

using System.Diagnostics;
using NvrView.Interop;

namespace NvrView.Video;

/// <summary>Counters for one tile (all generations), readable from any thread.</summary>
public struct TileCounters
{
    /// <summary>Decode actions.</summary>
    public long Decodes;

    /// <summary>Decodes answered with nv_tile_decoded.</summary>
    public long Decoded;

    /// <summary>Answers with nv_tile_decode_failed, any kind except NoPicture.</summary>
    public long Failures;

    /// <summary>Answers with NoPicture.</summary>
    public long NoPictures;

    /// <summary>Configure actions.</summary>
    public long Configures;

    /// <summary>Configure actions that failed.</summary>
    public long ConfigureFailures;

    /// <summary>Flush actions.</summary>
    public long Flushes;

    /// <summary>Reset actions.</summary>
    public long Resets;

    /// <summary>Clear actions.</summary>
    public long Clears;

    /// <summary>Generations the tile has had (1 without tier switches).</summary>
    public long Generations;

    /// <summary>Time spent in Decode actions (submit, decode, copy), Stopwatch ticks.</summary>
    public long DecodeTicks;

    /// <summary>Longest Decode action, Stopwatch ticks.</summary>
    public long MaxDecodeTicks;

    /// <summary>Input samples created by the tile's decoders.</summary>
    public long InputAllocations;
}

/// <summary>Carries out the core's actions for one tile (see the file comment).</summary>
public sealed class VideoTile : IDisposable
{
    private const int MaxGenerations = 4;

    private GpuDevice _gpu;
    private readonly NvCore _core;
    private readonly bool _allowHardware;
    private readonly Generation?[] _generations = new Generation?[MaxGenerations];
    private Generation? _shown;
    private TileCounters _counters;
    private readonly Lock _countersLock = new();
    private byte[] _parameterSets = new byte[256];
    private bool _disposed;

    /// <summary>A tile whose decoders use <paramref name="gpu"/> and answer to <paramref name="core"/>.</summary>
    public VideoTile(GpuDevice gpu, NvCore core, ulong tile, bool allowHardware = true)
    {
        _gpu = gpu;
        _core = core;
        Id = tile;
        _allowHardware = allowHardware;
    }

    /// <summary>The core's tile ID.</summary>
    public ulong Id { get; }

    /// <summary>The status the core last reported for the shown generation.</summary>
    public NvTileStatus Status { get; private set; }

    /// <summary>A copy of the counters.</summary>
    public TileCounters Counters
    {
        get
        {
            lock (_countersLock)
            {
                return _counters;
            }
        }
    }

    /// <summary>Name of the shown generation's decoder MFT, once configured.</summary>
    public string DecoderName => _shown?.Decoder?.Name ?? _shown?.LastName ?? "";

    /// <summary>Where the shown generation decodes.</summary>
    public DecoderKind DecoderKind => _shown?.Decoder?.Kind ?? _shown?.LastKind ?? DecoderKind.Unknown;

    /// <summary>The generation being shown (null before the first action).</summary>
    public uint? ShownGeneration => _shown?.Number;

    /// <summary>Why the last Configure failed, if it did.</summary>
    public string? ConfigureError { get; private set; }

    /// <summary>
    /// The codec of the last Configure when it failed because no decoder for
    /// it is installed (H.265 without the HEVC Video Extension); null otherwise.
    /// </summary>
    public NvCodec? MissingDecoder { get; private set; }

    /// <summary>Decoders rebuilt after a GPU device change.</summary>
    public long Rebuilds { get; private set; }

    /// <summary>
    /// The picture to draw: the shown generation's. Read it only under the
    /// device lock; null before the first action.
    /// </summary>
    public TilePicture? ShownPicture => _shown?.Picture;

    /// <summary>
    /// Carries out one action. Call from NvCoreCallbacks.OnAction for this
    /// tile (the core serializes them); answers go to the core before this
    /// returns, or later for pictures the decoder holds back.
    /// </summary>
    public void OnAction(in NvAction action)
    {
        if (_disposed)
        {
            return;
        }
        Generation? g = Find(action.Generation) ?? Begin(action.Generation);
        if (g is null)
        {
            // Too many generations at once (the core runs at most two).
            if (action.Kind == NvActionKind.Decode)
            {
                _core.DecodeFailed(Id, action.Generation, NvDecodeFailure.NotConfigured);
            }
            return;
        }
        switch (action.Kind)
        {
            case NvActionKind.Configure:
                Configure(g, in action.Configure);
                break;
            case NvActionKind.Decode:
                Decode(g, in action.Decode);
                break;
            case NvActionKind.Flush:
                Count(ref _counters.Flushes);
                g.Decoder?.Flush();
                break;
            case NvActionKind.Reset:
                Count(ref _counters.Resets);
                Reset(g);
                break;
            case NvActionKind.Clear:
                Count(ref _counters.Clears);
                using (_gpu.Lock())
                {
                    g.Picture.Clear();
                }
                break;
            case NvActionKind.Status:
                if (g == _shown)
                {
                    Status = action.Status;
                }
                break;
            default:
                // Unknown kinds never need an answer (docs/FFI.md).
                break;
        }
    }

    private void Configure(Generation g, in NvConfigure c)
    {
        Count(ref _counters.Configures);
        g.Decoder?.Dispose();
        g.Decoder = new MfDecoder(_gpu, g.Picture, _allowHardware);
        int length = AnnexB.Join(c.ParameterSetSpan, ref _parameterSets);
        g.Remember(c.Codec, _parameterSets.AsSpan(0, length), c.CodedSize);
        NvDecodeFailure? failure = g.Decoder.Configure(c.Codec, _parameterSets.AsSpan(0, length), c.CodedSize);
        MissingDecoder = failure is not null && g.Decoder.NoDecoderInstalled ? c.Codec : null;
        if (failure is { } kind)
        {
            ConfigureError = g.Decoder.ConfigureError;
            g.Configured = false;
            g.Decoder.Dispose();
            g.Decoder = null;
            Count(ref _counters.ConfigureFailures);
            _core.DecodeFailed(Id, g.Number, kind);
            return;
        }
        g.LastName = g.Decoder.Name;
        g.Configured = true;
        ConfigureError = null;
        using (_gpu.Lock())
        {
            g.Picture.Visible = c.VisibleRect;
            g.Picture.Color = ColorMatrix.From(in c.Color);
        }
    }

    private void Decode(Generation g, in NvDecode d)
    {
        long start = Stopwatch.GetTimestamp();
        Count(ref _counters.Decodes);
        if (g.Decoder is null && g.Rebuild)
        {
            // The GPU device was replaced (ReplaceDevice): a new decoder on
            // the new device from the generation's last Configure. It can
            // start only at a keyframe; failing a non-keyframe makes the
            // core flush and wait for one (docs/RENDERING.md, "Recovery").
            g.Rebuild = false;
            if (!RebuildDecoder(g) || !d.Keyframe)
            {
                g.Failed(NvDecodeFailure.DecodeError);
                return;
            }
        }
        if (g.Decoder is null)
        {
            g.Failed(NvDecodeFailure.NotConfigured);
            return;
        }
        long before = g.Decoder.Counters.InputAllocations;
        g.Decoder.Decode(d.AccessUnit.AsSpan(), d.Keyframe, d.PtsUs, g);
        long elapsed = Stopwatch.GetTimestamp() - start;
        long allocations = g.Decoder.Counters.InputAllocations - before;
        g.LastKind = g.Decoder.Kind;
        lock (_countersLock)
        {
            _counters.DecodeTicks += elapsed;
            _counters.MaxDecodeTicks = Math.Max(_counters.MaxDecodeTicks, elapsed);
            _counters.InputAllocations += allocations;
        }
    }

    private bool RebuildDecoder(Generation g)
    {
        Rebuilds++;
        g.Decoder = new MfDecoder(_gpu, g.Picture, _allowHardware);
        if (g.Decoder.Configure(g.SavedCodec, g.SavedSets.AsSpan(0, g.SavedSetsLength), g.SavedCoded) is not null)
        {
            ConfigureError = g.Decoder.ConfigureError;
            g.Decoder.Dispose();
            g.Decoder = null;
            return false;
        }
        return true;
    }

    /// <summary>
    /// Moves the tile to a new GPU device after the old one was lost. The
    /// caller makes sure no action for this tile runs meanwhile (it holds
    /// back the core's actions) and that nothing draws. Pictures in flight
    /// are answered DecodeError (the core flushes and waits for a keyframe);
    /// each generation gets an empty picture on the new device, and its
    /// decoder is rebuilt from its last Configure at the next Decode.
    /// </summary>
    public void ReplaceDevice(GpuDevice gpu)
    {
        foreach (Generation? g in _generations)
        {
            if (g?.Decoder is { } decoder)
            {
                decoder.AbandonHeld(g, NvDecodeFailure.DecodeError);
                decoder.Dispose();
                g.Decoder = null;
                g.Rebuild = g.Configured;
            }
        }
        using (_gpu.Lock())
        {
            foreach (Generation? g in _generations)
            {
                g?.Picture.Dispose();
            }
        }
        _gpu = gpu;
        using (gpu.Lock())
        {
            foreach (Generation? g in _generations)
            {
                if (g is not null)
                {
                    var picture = new TilePicture(gpu) { Visible = g.Picture.Visible, Color = g.Picture.Color };
                    g.Picture = picture;
                }
            }
        }
    }

    private void Reset(Generation g)
    {
        g.Decoder?.Dispose();
        g.Decoder = null;
        g.Rebuild = false;
        g.Configured = false;
        if (g != _shown)
        {
            return;
        }
        // The shown generation's final Reset while a newer one runs is the
        // switch: show the newer one from now on (docs/FFI.md).
        Generation? newer = null;
        foreach (Generation? other in _generations)
        {
            if (other is not null && other.Number > g.Number && (newer is null || other.Number > newer.Number))
            {
                newer = other;
            }
        }
        if (newer is null)
        {
            return;
        }
        using (_gpu.Lock())
        {
            _shown = newer;
            g.Picture.Dispose();
        }
        Remove(g);
    }

    private Generation? Find(uint number)
    {
        foreach (Generation? g in _generations)
        {
            if (g is not null && g.Number == number)
            {
                return g;
            }
        }
        return null;
    }

    private Generation? Begin(uint number)
    {
        // A generation between the shown one and this newer one was
        // superseded: nothing of it follows.
        if (_shown is not null && number > _shown.Number)
        {
            foreach (Generation? other in _generations)
            {
                if (other is not null && other != _shown && other.Number < number)
                {
                    Retire(other);
                }
            }
        }
        int free = Array.IndexOf(_generations, null);
        if (free < 0)
        {
            return null;
        }
        var g = new Generation(this, number, new TilePicture(_gpu));
        _generations[free] = g;
        Count(ref _counters.Generations);
        if (_shown is null)
        {
            using (_gpu.Lock())
            {
                _shown = g;
            }
        }
        return g;
    }

    private void Retire(Generation g)
    {
        g.Decoder?.Dispose();
        g.Decoder = null;
        using (_gpu.Lock())
        {
            g.Picture.Dispose();
        }
        Remove(g);
    }

    private void Remove(Generation g)
    {
        int i = Array.IndexOf(_generations, g);
        if (i >= 0)
        {
            _generations[i] = null;
        }
    }

    private void Count(ref long counter)
    {
        lock (_countersLock)
        {
            counter++;
        }
    }

    /// <summary>
    /// Releases every decoder and picture. Call after the tile is closed
    /// (nv_tile_close returned, so no action runs any more).
    /// </summary>
    public void Dispose()
    {
        _disposed = true;
        foreach (Generation? g in _generations)
        {
            if (g is not null)
            {
                g.Decoder?.Dispose();
                g.Decoder = null;
            }
        }
        using (_gpu.Lock())
        {
            _shown = null;
            foreach (Generation? g in _generations)
            {
                g?.Picture.Dispose();
            }
        }
        Array.Clear(_generations);
    }

    // One generation: its decoder (none between Reset and Configure) and
    // the picture it decodes into. It is also the answer path, so answers
    // carry its generation number.
    private sealed class Generation(VideoTile tile, uint number, TilePicture picture) : IDecodeResults
    {
        public readonly uint Number = number;
        public TilePicture Picture = picture;
        public MfDecoder? Decoder;
        public string? LastName;
        public DecoderKind LastKind;

        // The last Configure, to rebuild the decoder on a new device.
        public bool Configured;
        public bool Rebuild;
        public NvCodec SavedCodec;
        public byte[] SavedSets = [];
        public int SavedSetsLength;
        public NvSize SavedCoded;

        public void Remember(NvCodec codec, ReadOnlySpan<byte> sets, NvSize coded)
        {
            SavedCodec = codec;
            if (SavedSets.Length < sets.Length)
            {
                SavedSets = new byte[sets.Length];
            }
            sets.CopyTo(SavedSets);
            SavedSetsLength = sets.Length;
            SavedCoded = coded;
            Rebuild = false;
        }

        public void Decoded(long ptsUs)
        {
            tile.Count(ref tile._counters.Decoded);
            _ = tile._core.Decoded(tile.Id, Number, ptsUs);
        }

        public void Failed(NvDecodeFailure kind)
        {
            if (kind == NvDecodeFailure.NoPicture)
            {
                tile.Count(ref tile._counters.NoPictures);
            }
            else
            {
                tile.Count(ref tile._counters.Failures);
            }
            _ = tile._core.DecodeFailed(tile.Id, Number, kind);
        }
    }
}
