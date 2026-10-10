// Plays the core's audio mix through WASAPI in shared mode (docs/FFI.md,
// "Audio"; docs/AUDIO.md). The core decodes, times, and drift-corrects every
// tile; this only asks it for samples:
//
// - Format: 32-bit float stereo at the default output device's mix rate
//   (AUTOCONVERTPCM lets the engine take it whatever the device's own
//   format), so nv_audio_set_output is called once per device.
// - Event-driven: a thread with the "Pro Audio" MMCSS class wakes when the
//   engine wants data, fills everything free in the buffer with
//   NvCore.RenderAudio, and never allocates or locks while running.
// - Host time: when the first frame written now will be heard, on the
//   core's clock (QPC in ns). IAudioClock.GetPosition gives the device
//   position and the QPC time (100 ns units) it was sampled at; the frames
//   between that position and the end of what was written so far play
//   first. So host_ns = qpc * 100 + (written - position) / rate.
// - A device that goes away (AUDCLNT_E_DEVICE_INVALIDATED, unplugged or
//   changed) restarts the stream on the new default device after a second.

using System.Runtime.InteropServices;
using NvrView.Interop;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;
using static TerraFX.Interop.Windows.CLSCTX;
using static TerraFX.Interop.Windows.AUDCLNT;

namespace NvrView.App;

/// <summary>WASAPI output of the core's mix (see the file comment).</summary>
internal sealed unsafe partial class AudioOutput : IDisposable
{
    private const uint Channels = 2;

    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
    private static readonly Guid IeeeFloat = new("00000003-0000-0010-8000-00aa00389b71");

    private readonly NvCore _core;
    private readonly Thread _thread;
    private volatile bool _stop;

    public AudioOutput(NvCore core)
    {
        _core = core;
        _thread = new Thread(Run) { Name = "audio", IsBackground = true };
    }

    /// <summary>The output rate, once a device is open (0 before or without one).</summary>
    public uint SampleRate { get; private set; }

    /// <summary>Frames rendered by the core so far.</summary>
    public long FramesRendered => Interlocked.Read(ref _framesRendered);

    private long _framesRendered;

    /// <summary>Why there is no output, if there is none (no device, refused format).</summary>
    public string? Problem { get; private set; }

    /// <summary>How far ahead of now the last block was timed (host time minus the core's clock), ms.</summary>
    public double LeadMs { get; private set; }

    /// <summary>Times the stream was (re)started.</summary>
    public int Starts { get; private set; }

    public void Start() => _thread.Start();

    private void Run()
    {
        _ = CoInitializeEx(null, (uint)COINIT.COINIT_MULTITHREADED);
        uint task = 0;
        nint mmcss = AvSetMmThreadCharacteristicsW("Pro Audio", ref task);
        while (!_stop)
        {
            string? problem = PlayUntilError();
            if (_stop)
            {
                break;
            }
            Problem = problem;
            if (problem is not null)
            {
                AppLog.Limited("audio", 30, $"audio output: {problem}");
            }
            // A missing or changed device: try the (new) default device again.
            for (int i = 0; i < 10 && !_stop; i++)
            {
                Thread.Sleep(100);
            }
        }
        if (mmcss != 0)
        {
            _ = AvRevertMmThreadCharacteristics(mmcss);
        }
        CoUninitialize();
    }

    // Opens the default device and renders until an error or Dispose.
    private string? PlayUntilError()
    {
        IMMDeviceEnumerator* enumerator = null;
        IMMDevice* device = null;
        IAudioClient* client = null;
        IAudioRenderClient* render = null;
        IAudioClock* clock = null;
        WAVEFORMATEX* mix = null;
        HANDLE ready = CreateEventW(null, BOOL.FALSE, BOOL.FALSE, null);
        try
        {
            HRESULT hr = CoCreateInstance(Ref(in CLSID.CLSID_MMDeviceEnumerator), null, (uint)CLSCTX_ALL,
                __uuidof<IMMDeviceEnumerator>(), (void**)&enumerator);
            if (hr.FAILED)
            {
                return $"no device enumerator (0x{hr.Value:X8})";
            }
            hr = enumerator->GetDefaultAudioEndpoint(EDataFlow.eRender, ERole.eConsole, &device);
            if (hr.FAILED)
            {
                return $"no output device (0x{hr.Value:X8})";
            }
            hr = device->Activate(__uuidof<IAudioClient>(), (uint)CLSCTX_ALL, null, (void**)&client);
            if (hr.FAILED)
            {
                return $"output device refused (0x{hr.Value:X8})";
            }
            hr = client->GetMixFormat(&mix);
            if (hr.FAILED)
            {
                return $"no mix format (0x{hr.Value:X8})";
            }
            uint rate = mix->nSamplesPerSec;
            WAVEFORMATEXTENSIBLE format = default;
            format.Format.wFormatTag = 0xFFFE; // WAVE_FORMAT_EXTENSIBLE
            format.Format.nChannels = (ushort)Channels;
            format.Format.nSamplesPerSec = rate;
            format.Format.wBitsPerSample = 32;
            format.Format.nBlockAlign = (ushort)(Channels * 4);
            format.Format.nAvgBytesPerSec = rate * Channels * 4;
            format.Format.cbSize = (ushort)(sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX));
            format.Samples.wValidBitsPerSample = 32;
            format.dwChannelMask = 0x1 | 0x2; // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
            format.SubFormat = IeeeFloat;
            uint flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // 30 ms of buffer; the engine wakes us every period (10 ms by default).
            hr = client->Initialize(AUDCLNT_SHAREMODE.AUDCLNT_SHAREMODE_SHARED, flags, 300_000, 0, (WAVEFORMATEX*)&format, null);
            if (hr.FAILED)
            {
                return $"float stereo at {rate} Hz refused (0x{hr.Value:X8})";
            }
            uint bufferFrames;
            _ = client->SetEventHandle(ready);
            _ = client->GetBufferSize(&bufferFrames);
            hr = client->GetService(__uuidof<IAudioRenderClient>(), (void**)&render);
            if (hr.FAILED)
            {
                return $"no render client (0x{hr.Value:X8})";
            }
            hr = client->GetService(__uuidof<IAudioClock>(), (void**)&clock);
            if (hr.FAILED)
            {
                return $"no audio clock (0x{hr.Value:X8})";
            }
            ulong frequency;
            _ = clock->GetFrequency(&frequency);
            try
            {
                _core.SetAudioOutput(rate, Channels);
            }
            catch (NvException e)
            {
                return $"the core refused {rate} Hz: {e.Status}";
            }
            SampleRate = rate;
            Problem = null;
            long written = 0;
            hr = Fill(client, render, clock, bufferFrames, rate, frequency, ref written);
            if (hr.FAILED)
            {
                return Describe(hr);
            }
            hr = client->Start();
            if (hr.FAILED)
            {
                return $"start failed (0x{hr.Value:X8})";
            }
            Starts++;
            while (!_stop)
            {
                _ = WaitForSingleObject(ready, 200);
                hr = Fill(client, render, clock, bufferFrames, rate, frequency, ref written);
                if (hr.FAILED)
                {
                    _ = client->Stop();
                    return Describe(hr);
                }
            }
            _ = client->Stop();
            return null;
        }
        finally
        {
            SampleRate = 0;
            if (mix != null)
            {
                CoTaskMemFree(mix);
            }
            if (clock != null)
            {
                clock->Release();
            }
            if (render != null)
            {
                render->Release();
            }
            if (client != null)
            {
                client->Release();
            }
            if (device != null)
            {
                device->Release();
            }
            if (enumerator != null)
            {
                enumerator->Release();
            }
            _ = CloseHandle(ready);
        }
    }

    private static string Describe(HRESULT hr) =>
        hr.Value == AUDCLNT_E_DEVICE_INVALIDATED ? "the output device went away" : $"rendering failed (0x{hr.Value:X8})";

    // Fills every free frame of the buffer with the core's mix.
    private HRESULT Fill(IAudioClient* client, IAudioRenderClient* render, IAudioClock* clock,
        uint bufferFrames, uint rate, ulong frequency, ref long written)
    {
        uint padding;
        HRESULT hr = client->GetCurrentPadding(&padding);
        if (hr.FAILED)
        {
            return hr;
        }
        uint free = bufferFrames - padding;
        if (free == 0)
        {
            return S.S_OK;
        }
        byte* data;
        hr = render->GetBuffer(free, &data);
        if (hr.FAILED)
        {
            return hr;
        }
        ulong position, qpc;
        ulong hostNs;
        if (clock->GetPosition(&position, &qpc).SUCCEEDED && frequency > 0 && qpc > 0)
        {
            // Frames between what plays now and the first frame written now.
            double playedFrames = position * (double)rate / frequency;
            double ahead = written - playedFrames;
            hostNs = (ulong)Math.Max(0, qpc * 100.0 + ahead * 1e9 / rate);
        }
        else
        {
            hostNs = NvCore.ClockNowNs() + (ulong)(padding * 1e9 / rate);
        }
        LeadMs = ((long)hostNs - (long)NvCore.ClockNowNs()) / 1e6;
        var output = new Span<float>(data, (int)(free * Channels));
        _ = _core.RenderAudio(output, Channels, hostNs);
        hr = render->ReleaseBuffer(free, 0);
        written += free;
        Interlocked.Add(ref _framesRendered, free);
        return hr;
    }

    private static Guid* Ref(in Guid g) =>
        (Guid*)System.Runtime.CompilerServices.Unsafe.AsPointer(ref System.Runtime.CompilerServices.Unsafe.AsRef(in g));

    [LibraryImport("avrt.dll", StringMarshalling = StringMarshalling.Utf16)]
    private static partial nint AvSetMmThreadCharacteristicsW(string taskName, ref uint taskIndex);

    [LibraryImport("avrt.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool AvRevertMmThreadCharacteristics(nint handle);

    /// <summary>Stops the stream (call before the core is freed).</summary>
    public void Dispose()
    {
        _stop = true;
        if (_thread.IsAlive)
        {
            _thread.Join();
        }
    }
}
