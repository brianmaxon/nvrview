// Tests of the call rules (NvrView.Tiles/Calls.cs) on simulated time: the audio decisions, the
// talk-back state machine from talk events, the transcript, the echo gate, and how a call ends.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class CallTests
{
    private static readonly CameraRef Door = new("dev-example", 3);
    private static readonly CameraRef Yard = new("dev-example", 1);
    private static readonly DateTime T0 = new(2026, 10, 10, 14, 0, 0);

    public static IEnumerable<(string, Action)> All() =>
    [
        ("calls: a press is audible when listening, else Unmute", PressAudio),
        ("calls: a conversation is audible and mutes what else played", ConversationAudio),
        ("calls: talk goes through starting to active and stops", TalkLifecycle),
        ("calls: a late refusal of an earlier start changes nothing", LateRefusal),
        ("calls: refusal reasons in words", RefusalWords),
        ("calls: the talk-back prompt gate and its outcomes", TalkPrompts),
        ("calls: partials are replaced and finals appended", TranscriptModel),
        ("calls: the echo gate holds the microphone while the camera plays", EchoGateTest),
        ("calls: a call ends a minute after its last activity unless talking", CallExpiry),
        ("calls: a conversation is a call with no press", ConversationCall),
        ("calls: captions and why there are none", CaptionLines),
        ("calls: WAV and 16-bit sample conversion", WavReading),
    ];

    private static byte[] Wav(int channels, uint rate, short[] samples, uint? dataSizeClaim = null)
    {
        using var ms = new MemoryStream();
        using var w = new BinaryWriter(ms);
        w.Write("RIFF"u8);
        w.Write(36 + samples.Length * 2);
        w.Write("WAVE"u8);
        w.Write("fmt "u8);
        w.Write(16);
        w.Write((short)1);
        w.Write((short)channels);
        w.Write(rate);
        w.Write(rate * (uint)channels * 2);
        w.Write((short)(channels * 2));
        w.Write((short)16);
        w.Write("data"u8);
        w.Write(dataSizeClaim ?? (uint)(samples.Length * 2));
        foreach (short s in samples)
        {
            w.Write(s);
        }
        w.Flush();
        return ms.ToArray();
    }

    private static void WavReading()
    {
        Check.True(Pcm.TryReadWav(Wav(1, 22050, [0, 16384, -16384, 32767]), out float[] mono, out uint rate), "mono parses");
        Check.True(rate == 22050 && mono.Length == 4, "rate and length");
        Check.True(Math.Abs(mono[1] - 0.5f) < 1e-4 && Math.Abs(mono[2] + 0.5f) < 1e-4, "16-bit scale");
        Check.True(Pcm.TryReadWav(Wav(2, 16000, [1000, 3000, -2000, 0]), out float[] stereo, out _), "stereo parses");
        Check.True(stereo.Length == 2 && Math.Abs(stereo[0] - 2000 / 32768f) < 1e-4 && Math.Abs(stereo[1] + 1000 / 32768f) < 1e-4, "channels averaged");
        Check.True(Pcm.TryReadWav(Wav(1, 16000, [1, 2, 3], dataSizeClaim: uint.MaxValue), out float[] open, out _) && open.Length == 3, "an unknown data length reads to the end");
        Check.True(!Pcm.TryReadWav([1, 2, 3], out _, out _), "not a WAV");
        Check.True(!Pcm.TryReadWav(Wav(1, 16000, [])[..30], out _, out _), "no data chunk");
        var bytes = new byte[6];
        Pcm.ToInt16([0f, 1f, -2f], bytes);
        Check.True(BitConverter.ToInt16(bytes, 0) == 0 && BitConverter.ToInt16(bytes, 2) == 32767 && BitConverter.ToInt16(bytes, 4) == -32767, "float to 16-bit, clipped");
    }

    private static void PressAudio()
    {
        CallAudioDecision listening = CallAudio.Press(Door, [Yard]);
        Check.True(listening.Audible && !listening.ShowsUnmute && listening.MutedForCall.SequenceEqual([Yard]), "listening: audible, the other muted");
        CallAudioDecision same = CallAudio.Press(Door, [Door]);
        Check.True(same.Audible && same.MutedForCall.Count == 0, "listening to the doorbell itself mutes nothing");
        CallAudioDecision setting = CallAudio.Press(Door, [], unmuteOnRing: true);
        Check.True(setting.Audible && setting.MutedForCall.Count == 0, "the setting unmutes when not listening");
        CallAudioDecision silent = CallAudio.Press(Door, []);
        Check.True(!silent.Audible && silent.ShowsUnmute, "otherwise the Unmute control shows");
        var call = new Call(Door, T0, 0, silent);
        call.Unmuted();
        Check.True(call.Audible && !call.ShowsUnmute, "Unmute plays it");
    }

    private static void ConversationAudio()
    {
        CallAudioDecision none = CallAudio.Conversation(Door, []);
        Check.True(none.Audible && !none.ShowsUnmute && none.MutedForCall.Count == 0, "audible with nothing else playing");
        CallAudioDecision some = CallAudio.Conversation(Door, [Yard, Door]);
        Check.True(some.Audible && some.MutedForCall.SequenceEqual([Yard]), "the other camera is muted for the call");
    }

    private static void TalkLifecycle()
    {
        var talk = new TalkState();
        Check.Equal(TalkState.Phase.Idle, talk.Current, "idle");
        Check.True(!talk.Apply(new TalkEvent("active")) && talk.Current == TalkState.Phase.Idle, "an event with no start of ours is ignored");
        Check.True(talk.RequestStart(), "first request");
        Check.True(!talk.RequestStart(), "asking while one runs changes nothing");
        talk.StartSent();
        talk.Apply(new TalkEvent("starting"));
        Check.True(talk.IsTalking && !talk.IsActive, "starting");
        talk.Apply(new TalkEvent("active"));
        Check.True(talk.IsActive, "active");
        Check.Equal("Talking", talk.Text, "status while active");
        talk.Apply(new TalkEvent("stopped", "stopped", SentMs: 4096));
        Check.True(!talk.IsTalking && talk.Current == TalkState.Phase.Stopped, "stopped");
        Check.Equal("Talk ended", talk.Text, "status after a plain stop");
        Check.Equal(4096L, talk.SentMs, "the audio sent is kept");
        Check.True(talk.RequestStart(), "a new start after a stop");
        talk.StopRequested();
        Check.Equal(TalkState.Phase.Idle, talk.Current, "released before any event: idle");
    }

    private static void LateRefusal()
    {
        var talk = new TalkState();
        talk.RequestStart();
        talk.StartFailed(notPermitted: true);
        Check.Equal(TalkState.Phase.Refused, talk.Current, "refused at once");
        Check.True(talk.RequestStart(), "the user tries again");
        talk.StartSent();
        // The first start's refused event arrives while the second is outstanding.
        Check.True(!talk.Apply(new TalkEvent("refused", "not_permitted")), "the late event is ignored");
        Check.True(talk.IsTalking, "the second start is still going");
        talk.Apply(new TalkEvent("active"));
        Check.True(talk.IsActive, "its own events count");
    }

    private static void TalkPrompts()
    {
        Check.Equal(TalkGate.Ask, TalkPermission.Gate(false), "asks when talk-back is off");
        Check.Equal(TalkGate.Proceed, TalkPermission.Gate(true), "goes on when it is on");
        Check.Equal(new TalkOutcome(true, true, false), TalkPermission.Outcome(true, TalkAction.Microphone), "Allow for Talk");
        Check.Equal(new TalkOutcome(true, true, false), TalkPermission.Outcome(true, TalkAction.Reply), "Allow for a reply");
        Check.Equal(new TalkOutcome(false, false, false), TalkPermission.Outcome(false, TalkAction.Microphone), "Cancel for Talk");
        Check.Equal(new TalkOutcome(false, false, true), TalkPermission.Outcome(false, TalkAction.Reply), "Cancel keeps the typed text");
        TalkPrompt p = TalkPermission.Prompt("Front Door");
        Check.Equal("Allow talk-back for Front Door?", p.Title, "the prompt names the camera");
        Check.Equal("Talk-back sends sound through this camera's speaker. You can turn it off in Settings.", p.Message, "the explanation");
        Check.True(p.Allow == "Allow" && p.Cancel == "Cancel", "the buttons");
        Check.Equal("Talk-back refused: Front Door, another client is talking", TalkPermission.Refusal("Front Door", "busy"), "a refusal names the camera");
        Check.Equal("Talk-back refused: Front Door, too many attempts (try again in 2 s)", TalkPermission.Refusal("Front Door", "rate_limited", 2000), "with the wait");
    }

    private static void RefusalWords()
    {
        var talk = new TalkState();
        talk.RequestStart();
        talk.StartSent();
        talk.Apply(new TalkEvent("refused", "rate_limited", RetryMs: 41_000));
        Check.Equal("Talk-back refused: too many attempts (try again in 41 s)", talk.Text, "rate limit");
        talk.StartFailed(notPermitted: true);
        Check.True(talk.Text.Contains("Allow talk-back", StringComparison.Ordinal), "not permitted says where to allow it");
        Check.Equal("this camera has no talk-back", TalkState.Words("not_supported"), "no speaker");
        Check.Equal("some new reason", TalkState.Words("some_new_reason"), "unknown reasons are shown plainly");
    }

    private static void TranscriptModel()
    {
        var t = new Transcript();
        t.Partial("hel", TranscriptSource.Visitor);
        t.Partial("hello th", TranscriptSource.Visitor);
        Check.Equal("Visitor: hello th…", t.Lines[0].Display, "the partial is replaced");
        t.Final("Hello there.", TranscriptSource.Visitor);
        t.Partial("hi", TranscriptSource.You);
        Check.Equal(2, t.Lines.Count, "one final and one partial");
        Check.Equal("You: hi…", t.Lines[1].Display, "the other source's phrase follows the finals");
        t.Final("Hi, one moment.", TranscriptSource.You);
        t.Final("   ", TranscriptSource.You);
        Check.Equal("Visitor: Hello there.\nYou: Hi, one moment.", t.SavedText, "saved form");
    }

    private static void EchoGateTest()
    {
        var gate = new EchoGate();
        Check.True(gate.AllowsMicrophone(0, cameraAudible: false, cameraPeak: 0.5f), "not audible: open");
        Check.True(!gate.AllowsMicrophone(1, cameraAudible: true, cameraPeak: 0.2f), "audible and loud: gated");
        Check.True(!gate.AllowsMicrophone(1.3, cameraAudible: true, cameraPeak: 0.0f), "held 0.4 s after");
        Check.True(gate.AllowsMicrophone(1.41, cameraAudible: true, cameraPeak: 0.0f), "open once quiet for the hold");
    }

    private static void CallExpiry()
    {
        var call = new Call(Door, T0, 0, CallAudio.Press(Door, []));
        Check.True(!call.Expired(59) && call.Expired(60), "a minute of silence");
        call.Activity(30);
        Check.True(!call.Expired(89) && call.Expired(90), "activity restarts the minute");
        call.Activity(10);
        Check.Equal(30.0, call.LastActivity, "activity never moves back");
        call.Talk.RequestStart();
        Check.True(!call.Expired(500), "talking keeps it open");
        call.Talk.StopRequested();
        call.Transcript.Final("Hello.", TranscriptSource.Visitor);
        call.Pressed(40);
        Check.Equal(2, call.Presses, "a second press");
        Check.Equal("Doorbell call ended: channel 4, 2 presses, 95 s", call.LogText(T0.AddSeconds(95), false), "log line");
        Check.True(call.LogText(T0.AddSeconds(95), true).EndsWith("\nVisitor: Hello.", StringComparison.Ordinal), "transcript only when saving");
    }

    private static void ConversationCall()
    {
        var call = new Call(Door, T0, 0, CallAudio.Conversation(Door, [Yard]), CallOrigin.Conversation);
        Check.True(call.Origin == CallOrigin.Conversation && call.Presses == 0, "no press");
        Check.True(call.Audible && !call.ShowsUnmute && call.MutedForCall.SequenceEqual([Yard]), "audible, the other muted");
        Check.True(!call.Expired(59) && call.Expired(60), "ends a minute after its last activity, as for a press");
        Check.Equal("Conversation ended: channel 4, 42 s", call.LogText(T0.AddSeconds(42), false), "log line");
        call.Pressed(5);
        Check.Equal("Doorbell call ended: channel 4, 1 press, 42 s", call.LogText(T0.AddSeconds(42), false), "a press joins it");
    }

    private static void CaptionLines()
    {
        var call = new Call(Door, T0, 0, CallAudio.Conversation(Door, []), CallOrigin.Conversation);
        Check.Equal("Preparing speech recognition…", call.Captions().Note, "preparing");
        call.Transcription = new TranscriptionStatus(TranscriptionPhase.Running);
        Check.Equal("Listening for speech…", call.Captions().Note, "running, nothing heard");
        for (int i = 1; i <= 6; i++)
        {
            call.Transcript.Final($"line {i}", TranscriptSource.Visitor);
        }
        (IReadOnlyList<string> lines, string? note) = call.Captions();
        Check.True(note is null && lines.Count == 4 && lines[0] == "Visitor: line 3", "the last four lines");
        call.Transcription = new TranscriptionStatus(TranscriptionPhase.Unavailable, "no speech recognizer is installed");
        Check.Equal("No transcript: no speech recognizer is installed", call.Captions().Note, "why there is none");
    }
}
