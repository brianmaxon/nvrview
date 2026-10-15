// Moonshine's C API (moonshine-c-api.h, MIT; the library is moonshine.dll on ONNX Runtime) for the
// captions engine (MoonshineTranscriber.cs). Only the streaming transcription calls are bound.
//
// The libraries are not in the app's package: they come with the caption model's download
// (CaptionModel.cs). ONNX Runtime is loaded first, by full path, so moonshine.dll binds to that
// copy and never to an older one in System32 (some Windows versions have one there): the app
// folder's onnxruntime.dll when the app ships one (a feature that runs other models, such as the
// detector on Windows ML, brings ONNX Runtime 1.23 or newer, and the captions share it), else the
// one in the model's folder. Calls go through function pointers from NativeLibrary.GetExport, so
// nothing depends on DllImport probing and nothing is left for the trimmer to remove.

using System.Runtime.InteropServices;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>The loaded Moonshine library and one loaded model, shared by every caption session of the process.</summary>
internal sealed unsafe class MoonshineNative
{
    /// <summary>The C API version these bindings were written against (MOONSHINE_HEADER_VERSION).</summary>
    public const int HeaderVersion = 30000;

    private static readonly Lock s_gate = new();
    private static MoonshineNative? s_loaded;
    private static int s_users;

    private readonly delegate* unmanaged[Cdecl]<int, IntPtr> _errorToString;
    private readonly delegate* unmanaged[Cdecl]<int, void> _freeTranscriber;
    private readonly delegate* unmanaged[Cdecl]<int, uint, int> _createStream;
    private readonly delegate* unmanaged[Cdecl]<int, int, int> _freeStream;
    private readonly delegate* unmanaged[Cdecl]<int, int, int> _startStream;
    private readonly delegate* unmanaged[Cdecl]<int, int, int> _stopStream;
    private readonly delegate* unmanaged[Cdecl]<int, int, float*, ulong, int, uint, int> _addAudio;
    private readonly delegate* unmanaged[Cdecl]<int, int, uint, Transcript**, int> _transcribe;
    private readonly int _transcriber;

    [StructLayout(LayoutKind.Sequential)]
    private struct Option
    {
        public IntPtr Name;
        public IntPtr Value;
    }

    /// <summary>transcript_line_t (x64 layout).</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct Line
    {
        public IntPtr Text;
        public IntPtr AudioData;
        public nuint AudioDataCount;
        public float StartTime;
        public float Duration;
        public ulong Id;
        public sbyte IsComplete;
        public sbyte IsUpdated;
        public sbyte IsNew;
        public sbyte HasTextChanged;
        public sbyte HaveSpeakersChanged;
        public IntPtr SpeakerSpans;
        public ulong SpeakerSpanCount;
        public uint LastTranscriptionLatencyMs;
        public IntPtr Words;
        public ulong WordCount;
    }

    /// <summary>transcript_t.</summary>
    [StructLayout(LayoutKind.Sequential)]
    public struct Transcript
    {
        public Line* Lines;
        public ulong LineCount;
    }

    private MoonshineNative(IntPtr lib, string modelFolder, uint arch)
    {
        _errorToString = (delegate* unmanaged[Cdecl]<int, IntPtr>)NativeLibrary.GetExport(lib, "moonshine_error_to_string");
        var load = (delegate* unmanaged[Cdecl]<byte*, uint, Option*, ulong, int, int>)NativeLibrary.GetExport(lib, "moonshine_load_transcriber_from_files");
        _freeTranscriber = (delegate* unmanaged[Cdecl]<int, void>)NativeLibrary.GetExport(lib, "moonshine_free_transcriber");
        _createStream = (delegate* unmanaged[Cdecl]<int, uint, int>)NativeLibrary.GetExport(lib, "moonshine_create_stream");
        _freeStream = (delegate* unmanaged[Cdecl]<int, int, int>)NativeLibrary.GetExport(lib, "moonshine_free_stream");
        _startStream = (delegate* unmanaged[Cdecl]<int, int, int>)NativeLibrary.GetExport(lib, "moonshine_start_stream");
        _stopStream = (delegate* unmanaged[Cdecl]<int, int, int>)NativeLibrary.GetExport(lib, "moonshine_stop_stream");
        _addAudio = (delegate* unmanaged[Cdecl]<int, int, float*, ulong, int, uint, int>)NativeLibrary.GetExport(lib, "moonshine_transcribe_add_audio_to_stream");
        _transcribe = (delegate* unmanaged[Cdecl]<int, int, uint, Transcript**, int>)NativeLibrary.GetExport(lib, "moonshine_transcribe_stream");
        var version = (delegate* unmanaged[Cdecl]<int>)NativeLibrary.GetExport(lib, "moonshine_get_version");
        Version = version();
        byte[] path = System.Text.Encoding.UTF8.GetBytes(modelFolder + "\0");
        fixed (byte* p = path)
        {
            _transcriber = load(p, arch, null, 0, HeaderVersion);
        }
        if (_transcriber < 0)
        {
            throw new MoonshineException($"the model could not be loaded: {ErrorText(_transcriber)}");
        }
    }

    /// <summary>The library's version (MAJOR * 10000 + MINOR * 100 + PATCH).</summary>
    public int Version { get; }

    /// <summary>
    /// The shared model (<paramref name="arch"/> is a MOONSHINE_MODEL_ARCH_* value; the caption
    /// model's manifest names it), loading the libraries and the model on first use (on the calling thread;
    /// about a second). Every call is matched by <see cref="Release"/>; the model is freed when the
    /// last caption session ends, so no memory stays held between calls.
    /// </summary>
    public static MoonshineNative Acquire(string modelFolder, string appFolder, uint arch)
    {
        lock (s_gate)
        {
            if (s_loaded is null)
            {
                string ort = File.Exists(Path.Combine(appFolder, "onnxruntime.dll"))
                    ? Path.Combine(appFolder, "onnxruntime.dll")
                    : Path.Combine(modelFolder, "onnxruntime.dll");
                if (!NativeLibrary.TryLoad(ort, out _))
                {
                    throw new MoonshineException("ONNX Runtime could not be loaded (is the Visual C++ runtime installed?)");
                }
                if (!NativeLibrary.TryLoad(Path.Combine(modelFolder, "moonshine.dll"), out IntPtr lib))
                {
                    throw new MoonshineException("the Moonshine library could not be loaded");
                }
                SingleThreaded();
                s_loaded = new MoonshineNative(lib, modelFolder, arch);
            }
            s_users++;
            return s_loaded;
        }
    }

    // One ONNX Runtime thread per model session, without the thread pool's spinning: by default
    // ONNX Runtime runs every call on a pool as wide as the computer, whose threads spin between
    // calls, and a caption stream (a short call every 200 ms) then costs well over a core while
    // doing little (measured; apps/windows/README.md "Captions"). Moonshine reads the switch from
    // the C runtime's environment when it creates its sessions, so it is set there (_putenv_s
    // also sets the process environment).
    private static void SingleThreaded()
    {
        if (NativeLibrary.TryLoad("ucrtbase.dll", out IntPtr crt) && NativeLibrary.TryGetExport(crt, "_putenv_s", out IntPtr putenv))
        {
            byte[] name = "MOONSHINE_ORT_SINGLE_THREAD\0"u8.ToArray();
            byte[] value = "1\0"u8.ToArray();
            fixed (byte* n = name, v = value)
            {
                _ = ((delegate* unmanaged[Cdecl]<byte*, byte*, int>)putenv)(n, v);
            }
        }
    }

    /// <summary>Gives back what <see cref="Acquire"/> handed out; the last user frees the model (the libraries stay loaded).</summary>
    public static void Release()
    {
        lock (s_gate)
        {
            if (--s_users == 0 && s_loaded is { } m)
            {
                m._freeTranscriber(m._transcriber);
                s_loaded = null;
            }
        }
    }

    public string ErrorText(int code) => Marshal.PtrToStringUTF8(_errorToString(code)) ?? $"error {code}";

    private void Check(int code, string what)
    {
        if (code != 0)
        {
            throw new MoonshineException($"{what}: {ErrorText(code)}");
        }
    }

    public int CreateStream()
    {
        int s = _createStream(_transcriber, 0);
        if (s < 0)
        {
            throw new MoonshineException($"no stream: {ErrorText(s)}");
        }
        Check(_startStream(_transcriber, s), "the stream could not start");
        return s;
    }

    public void AddAudio(int stream, ReadOnlySpan<float> pcm16k)
    {
        fixed (float* p = pcm16k)
        {
            Check(_addAudio(_transcriber, stream, p, (ulong)pcm16k.Length, 16_000, 0), "audio was refused");
        }
    }

    /// <summary>The stream's whole transcript now, copied out (the library's memory is valid only until its next call).</summary>
    public List<StreamingCaptions.Line> Transcribe(int stream)
    {
        Transcript* t;
        Check(_transcribe(_transcriber, stream, 0, &t), "transcription failed");
        var lines = new List<StreamingCaptions.Line>((int)t->LineCount);
        for (ulong i = 0; i < t->LineCount; i++)
        {
            Line l = t->Lines[i];
            lines.Add(new StreamingCaptions.Line(l.Id, Marshal.PtrToStringUTF8(l.Text) ?? "", l.IsComplete != 0));
        }
        return lines;
    }

    /// <summary>Ends the input; the next <see cref="Transcribe"/> analyzes what is left and marks every line complete.</summary>
    public void Stop(int stream) => Check(_stopStream(_transcriber, stream), "the stream could not stop");

    public void FreeStream(int stream) => _ = _freeStream(_transcriber, stream);
}

internal sealed class MoonshineException(string message) : Exception(message);
