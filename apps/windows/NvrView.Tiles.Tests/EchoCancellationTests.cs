// Tests of echo cancellation for talk-back (NvrView.Tiles/EchoCancellation.cs): when the system's
// canceller counts as on, and the caption gate in each mode. The Mac app's EchoCancellationTests
// has the same vectors.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class EchoCancellationTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("echo: only a confirmed canceller on the playback output counts", Decide),
        ("echo: the note and the log say which", Words),
        ("echo: each outcome is logged once", LoggedOnce),
        ("echo: without cancellation the captions pause while the camera plays", Gated),
        ("echo: with cancellation only a very loud camera ducks briefly", Ducked),
        ("echo: losing cancellation mid-call keeps the gate's hold", Switching),
    ];

    private static void Decide()
    {
        Check.Equal(EchoCancellation.System, EchoCancellation.Decide(new EchoProbe(true, Effect: EchoEffect.On, ReferenceIsPlayback: true)), "on");
        Check.Equal(EchoCancellation.Gated("no input device"), EchoCancellation.Decide(EchoProbe.Failed("no input device")), "failed");
        Check.Equal(EchoCancellation.Gated("the system's voice processing did not start"), EchoCancellation.Decide(new EchoProbe(false)), "not set up");
        Check.Equal(EchoCancellation.Gated("the microphone has no echo cancellation"),
            EchoCancellation.Decide(new EchoProbe(true, Effect: EchoEffect.Absent, ReferenceIsPlayback: true)), "absent");
        Check.Equal(EchoCancellation.Gated("echo cancellation is turned off for the microphone"),
            EchoCancellation.Decide(new EchoProbe(true, Effect: EchoEffect.Off, ReferenceIsPlayback: true)), "off");
        Check.Equal(EchoCancellation.Gated("the system does not report echo cancellation"),
            EchoCancellation.Decide(new EchoProbe(true, Effect: EchoEffect.Unknown, ReferenceIsPlayback: true)), "unknown");
        Check.Equal(EchoCancellation.Gated("echo cancellation follows another output device"),
            EchoCancellation.Decide(new EchoProbe(true, Effect: EchoEffect.On, ReferenceIsPlayback: false)), "another output");
    }

    private static void Words()
    {
        Check.Equal("Echo cancellation on", EchoCancellation.System.Note, "on note");
        Check.Equal("No echo cancellation", EchoCancellation.Gated("x").Note, "off note");
        Check.Equal("Talk-back microphone: echo cancellation on", EchoCancellation.System.LogLine, "on log");
        Check.Equal("Talk-back microphone: echo cancellation unavailable (no input device); captions of the microphone pause while the camera plays",
            EchoCancellation.Gated("no input device").LogLine, "off log");
        Check.True(EchoCancellation.System.Active && !EchoCancellation.Gated("x").Active, "active");
    }

    private static void LoggedOnce()
    {
        var log = new EchoCancellationLog();
        Check.True(log.Line(EchoCancellation.Gated("a")) is not null, "first a");
        Check.True(log.Line(EchoCancellation.Gated("a")) is null, "second a");
        Check.True(log.Line(EchoCancellation.Gated("b")) is not null, "first b");
        Check.True(log.Line(EchoCancellation.System) is not null, "first on");
        Check.True(log.Line(EchoCancellation.System) is null, "second on");
    }

    private static void Gated()
    {
        var gate = new MicrophoneEchoGate();
        Check.True(gate.AllowsMicrophone(0, true, 0.01f, false), "quiet camera: open");
        Check.True(!gate.AllowsMicrophone(1, true, 0.2f, false), "camera speaks: gated");
        Check.True(!gate.AllowsMicrophone(1.3, true, 0, false), "held 0.4 s after");
        Check.True(gate.AllowsMicrophone(1.41, true, 0, false), "open after the hold");
        Check.True(gate.AllowsMicrophone(2, false, 0.9f, false), "muted camera: open");
    }

    private static void Ducked()
    {
        var gate = new MicrophoneEchoGate();
        Check.True(gate.AllowsMicrophone(0, true, 0.3f, true), "ordinary speech: full duplex");
        Check.True(gate.AllowsMicrophone(0.1, true, 0.5f, true), "at the threshold: open");
        Check.True(!gate.AllowsMicrophone(1, true, 0.8f, true), "very loud: ducked");
        Check.True(!gate.AllowsMicrophone(1.14, true, 0.1f, true), "held 0.15 s");
        Check.True(gate.AllowsMicrophone(1.16, true, 0.1f, true), "open after the duck");
        Check.True(gate.AllowsMicrophone(2, false, 0.9f, true), "muted camera: open");
    }

    private static void Switching()
    {
        var gate = new MicrophoneEchoGate();
        Check.True(gate.AllowsMicrophone(0, true, 0.2f, true), "cancelled: open");
        Check.True(!gate.AllowsMicrophone(0.2, true, 0, false), "cancellation lost: the gate already holds");
        Check.True(gate.AllowsMicrophone(0.41, true, 0, false), "open after the hold");
    }
}
