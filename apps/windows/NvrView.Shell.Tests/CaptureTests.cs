using System.Text;
using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// Capture (minor 9): the recording event as the core writes it, the request
/// builders, and FakeCapture from configure to finished, with an encoder that
/// answers from inside its own calls.
/// </summary>
internal static class CaptureTests
{
    /// <summary>Answers every frame (and two more on a flush) through the capture calls.</summary>
    private sealed class AnsweringEncoder : ICaptureEncoder
    {
        public ICapture? Capture;
        public readonly List<(ulong, uint)> Opened = [];
        public readonly List<(ulong, uint, bool)> Closed = [];
        public int Refused;

        public EncoderOpened? Open(ulong recording, uint track, uint sampleRate, uint channels, uint bitRate)
        {
            lock (Opened)
            {
                Opened.Add((recording, track));
            }
            return new EncoderOpened([0x11, 0x88], 2112);
        }

        public void Encode(ulong recording, uint track, ReadOnlySpan<float> pcm)
        {
            if (pcm.Length != 1024)
            {
                Interlocked.Increment(ref Refused);
            }
            Answer(recording, track);
        }

        public void Close(ulong recording, uint track, bool flush)
        {
            if (flush)
            {
                Answer(recording, track);
                Answer(recording, track);
            }
            lock (Closed)
            {
                Closed.Add((recording, track, flush));
            }
        }

        private void Answer(ulong recording, uint track)
        {
            if (Capture?.RecordAudioEncoded(recording, track, [0, 0, 0, 7]) != NvStatus.Ok)
            {
                Interlocked.Increment(ref Refused);
            }
        }
    }

    private static (NvStatus, string?) StatusOf(Action call)
    {
        try
        {
            call();
            return (NvStatus.Ok, null);
        }
        catch (NvException e)
        {
            return (e.Status, e.Detail);
        }
    }

    public static void Run()
    {
        T.Run("capture: the recording event", () =>
        {
            RecordingEvent e = T.NotNull(RecordingEvent.Parse(Encoding.UTF8.GetBytes(
                """{"type":"recording","recording":12,"kind":"live","device":1,"channel":3,"lens":0,"tier":"main","state":"finished","reason":"max_duration","file":"/x/a.mp4","segment":1,"bytes":1048576000,"duration_ms":3600000,"gaps":1,"audio":"passthrough","tracks":["video","audio"],"encode_dropped_ms":0,"dropped_frames":2,"fragmented":false,"future":1}""")));
            T.Equal(new RecordingEvent(12, "live", 1, 3, 0, "main", "finished", "max_duration", "/x/a.mp4", 1, 1048576000,
                3600000, 1, "passthrough", "video,audio", 0, 2, false), e);
            T.Check(e.Ended);
            T.Equal(null, RecordingEvent.Parse(Encoding.UTF8.GetBytes("""{"type":"recordings_changed"}""")));
        });

        T.Run("capture: requests", () =>
        {
            T.Equal("""{"kind":"live","tile":7,"utc_offset_s":3600,"quality":"sub","name":"Camera 1"}""",
                RecordRequest.Live(7, 3600, quality: "sub", name: "Camera 1"));
            using JsonDocument call = JsonDocument.Parse(RecordRequest.Call(7, "/tmp/a b.mp4"));
            T.Equal("/tmp/a b.mp4", call.RootElement.GetProperty("file").GetString());
            T.Check(NvNative.AbiMinor >= 9, "capture is minor 9");
            T.Equal(64, NvNative.AscMax);
        });

        T.Run("capture: the fake from configure to finished", () =>
        {
            var events = new List<string>();
            var fake = new FakeCapture(e => { lock (events) { events.Add(e); } },
                t => t == 5 ? (1UL, 0, true) : null) { RecordingWorkIsManual = true };
            ICapture cap = fake;
            List<RecordingEvent> Of(ulong id)
            {
                lock (events)
                {
                    return [.. events.Select(e => RecordingEvent.Parse(Encoding.UTF8.GetBytes(e))).OfType<RecordingEvent>().Where(e => e.Recording == id)];
                }
            }
            string live = RecordRequest.Live(5, 0, name: "Camera 1");

            T.Equal((NvStatus.NotConfigured, (string?)null), StatusOf(() => cap.RecordStart(live)));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => cap.CaptureConfigure("""{"live_dir":"rel"}""")).Item1);
            string dir = Path.Combine(Path.GetTempPath(), "nvrview-captures");
            cap.CaptureConfigure(new System.Text.Json.Nodes.JsonObject { ["live_dir"] = dir, ["max_live"] = 1 }.ToJsonString());

            fake.FreeBytes = 1;
            T.Equal((NvStatus.Limit, (string?)"disk_low"), StatusOf(() => cap.RecordStart(live)));
            fake.FreeBytes = ulong.MaxValue;
            T.Equal(NvStatus.UnknownId, StatusOf(() => cap.RecordStart(RecordRequest.Live(6, 0))).Item1);

            RecordingStarted s = cap.RecordStart(live);
            T.Equal("waiting_keyframe", s.State);
            T.Check(s.File.StartsWith(dir, StringComparison.Ordinal) && s.File.EndsWith(" Camera 1.mp4", StringComparison.Ordinal));
            T.Equal((NvStatus.Limit, (string?)"live"), StatusOf(() => cap.RecordStart(live)));
            T.Equal(0, Of(s.Recording).Count);
            fake.CompleteCaptureWork();
            T.Equal("waiting_keyframe,recording", string.Join(",", Of(s.Recording).Select(e => e.State)));
            T.Check(cap.RecordStatus().Contains($"\"recording\":{s.Recording},", StringComparison.Ordinal));
            cap.RecordStop(s.Recording);
            T.Equal(NvStatus.UnknownId, StatusOf(() => cap.RecordStop(s.Recording)).Item1);
            fake.CompleteCaptureWork();
            RecordingEvent end = Of(s.Recording)[^1];
            T.Check(end.State == "finished" && end.Reason == "stopped" && end.Ended);
            T.Equal("""{"recordings":[]}""", cap.RecordStatus());

            // A call needs an encoder; with one, its tracks go through it.
            string call = RecordRequest.Call(5, Path.Combine(dir, "call.mp4"));
            T.Equal(NvStatus.NotConfigured, StatusOf(() => cap.RecordStart(call)).Item1);
            var encoder = new AnsweringEncoder { Capture = cap };
            cap.CaptureSetEncoder(encoder);
            RecordingStarted c = cap.RecordStart(call);
            T.Equal((NvStatus.Limit, (string?)"calls"), StatusOf(() => cap.RecordStart(call)));
            T.Equal("0,2", string.Join(",", encoder.Opened.Where(o => o.Item1 == c.Recording).Select(o => o.Item2)));
            T.Check(fake.EncodedUnits(c.Recording).Values.All(n => n == 1));
            cap.RecordStop(c.Recording);
            T.Check(encoder.Closed.All(x => x.Item1 == c.Recording && x.Item3));
            T.Equal(0, encoder.Refused);
            fake.CompleteCaptureWork();
            RecordingEvent callEnd = Of(c.Recording)[^1];
            T.Equal("video,Conversation,Visitor,You", callEnd.Tracks);
            T.Equal(NvStatus.UnknownId, cap.RecordAudioEncoded(c.Recording, 0, [1, 2, 3, 4]));
            cap.CaptureSetEncoder(null);
        });
    }
}
