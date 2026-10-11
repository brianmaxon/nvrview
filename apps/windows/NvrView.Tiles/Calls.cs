// Calls (docs/UX.md "Doorbell calls"): a doorbell press, or the user's Start
// Conversation on a camera's tile, opens a call with live captions, Talk, and
// a typed reply. The decisions are pure and tested without a window: whether
// the camera becomes audible, where talk-back stands, the live transcript,
// when the microphone's transcription is gated against the speakers' echo,
// and when the call ends. The same rules as the Mac app's DoorbellCall.

using System.Globalization;

namespace NvrView.Tiles;

/// <summary>How a call opened.</summary>
public enum CallOrigin
{
    /// <summary>A doorbell press (the device's <c>visitor</c> alarm).</summary>
    Press,

    /// <summary>The user chose Start Conversation: the same call with no press.</summary>
    Conversation,
}

/// <summary>What opening a call does to the audio.</summary>
/// <param name="Audible">The camera plays at once.</param>
/// <param name="ShowsUnmute">The large Unmute control covers the call (silent).</param>
/// <param name="MutedForCall">Cameras that were playing; muted for the call, audible again after it.</param>
public sealed record CallAudioDecision(bool Audible, bool ShowsUnmute, IReadOnlyList<CameraRef> MutedForCall);

/// <summary>Whether a call makes the camera audible.</summary>
public static class CallAudio
{
    /// <summary>A press: audible if the user was listening to anything (it is muted meanwhile), else the Unmute control.</summary>
    public static CallAudioDecision Press(CameraRef doorbell, IReadOnlyCollection<CameraRef> listening, bool unmuteOnRing = false)
    {
        CameraRef[] others = [.. listening.Where(c => c != doorbell)];
        if (listening.Count > 0)
        {
            return new CallAudioDecision(true, false, others);
        }
        return unmuteOnRing ? new CallAudioDecision(true, false, []) : new CallAudioDecision(false, true, []);
    }

    /// <summary>The user started the conversation: the camera plays, and whatever else played is muted for the call.</summary>
    public static CallAudioDecision Conversation(CameraRef camera, IReadOnlyCollection<CameraRef> listening) =>
        new(true, false, [.. listening.Where(c => c != camera)]);
}

/// <summary>One <c>talk</c> event of the core (docs/FFI.md "Talk-back").</summary>
public sealed record TalkEvent(string State, string? Reason = null, long RetryMs = 0, long SentMs = 0);

/// <summary>
/// Where the call's talk-back stands, from what the app asked and the core's
/// <c>talk</c> events. Talk-back is camera motion control: it starts only when
/// the user presses Talk (or sends a typed reply), never on its own.
/// </summary>
public sealed class TalkState
{
    public enum Phase
    {
        Idle,

        /// <summary>Asked for; no event yet.</summary>
        Requested,
        Starting,
        Active,
        Refused,
        Stopped,
    }

    public Phase Current { get; private set; } = Phase.Idle;

    /// <summary>Why it was refused or stopped (the core's reason name).</summary>
    public string Reason { get; private set; } = "";

    public long RetryMs { get; private set; }

    /// <summary>The audio the core sent in the last talk that ended, in ms (the stopped event's <c>sent_ms</c>).</summary>
    public long SentMs { get; private set; }

    /// <summary>
    /// Starts sent to the core whose <c>refused</c> or <c>stopped</c> has not arrived (each start
    /// ends with exactly one). The events of an earlier start that arrive late change nothing.
    /// </summary>
    public int Outstanding { get; private set; }

    /// <summary>Talk is asked for, starting, or running.</summary>
    public bool IsTalking => Current is Phase.Requested or Phase.Starting or Phase.Active;

    public bool IsActive => Current == Phase.Active;

    /// <summary>The user pressed Talk: true when the core should be asked to start (asking while one runs changes nothing).</summary>
    public bool RequestStart()
    {
        if (IsTalking)
        {
            return false;
        }
        Current = Phase.Requested;
        SentMs = 0;
        return true;
    }

    /// <summary><c>nv_talk_start</c> was called and accepted: its events will follow.</summary>
    public void StartSent() => Outstanding++;

    /// <summary>
    /// <c>nv_talk_start</c> failed at once. <paramref name="notPermitted"/> still sends its refused
    /// event, which is counted here; a tile that is gone sends none.
    /// </summary>
    public void StartFailed(bool notPermitted, bool tileClosed = false)
    {
        if (notPermitted)
        {
            Outstanding++;
        }
        Current = Phase.Refused;
        Reason = notPermitted ? "not_permitted" : tileClosed ? "tile_closed" : "error";
        RetryMs = 0;
    }

    /// <summary>Folds in a <c>talk</c> event for the call's tile. False for an event of an earlier start (or none): ignored.</summary>
    public bool Apply(TalkEvent e)
    {
        switch (e.State)
        {
            case "refused":
            case "stopped":
                if (Outstanding == 0)
                {
                    return false;
                }
                Outstanding--;
                if (Outstanding > 0)
                {
                    return false;
                }
                break;
            case "starting":
            case "active":
                if (Outstanding == 0)
                {
                    return false;
                }
                break;
            default:
                return false;
        }
        switch (e.State)
        {
            case "starting":
                Current = Phase.Starting;
                break;
            case "active":
                Current = Phase.Active;
                break;
            case "refused":
                Current = Phase.Refused;
                Reason = e.Reason ?? "refused";
                RetryMs = e.RetryMs;
                break;
            case "stopped":
                Current = Phase.Stopped;
                Reason = e.Reason ?? "stopped";
                RetryMs = 0;
                SentMs = e.SentMs;
                break;
        }
        return true;
    }

    /// <summary>The user let go of Talk: back to idle once the stop is asked for.</summary>
    public void StopRequested()
    {
        if (Current == Phase.Requested)
        {
            Current = Phase.Idle;
        }
    }

    /// <summary>The call's status line.</summary>
    public string Text => Current switch
    {
        Phase.Idle => "Hold Talk to speak",
        Phase.Requested or Phase.Starting => "Connecting talk-back…",
        Phase.Active => "Talking",
        Phase.Refused => "Talk-back refused: " + Words(Reason, RetryMs),
        _ => Reason is "stopped" or "finished" ? "Talk ended" : "Talk ended: " + Words(Reason),
    };

    /// <summary>A refusal or stop reason in words.</summary>
    public static string Words(string reason, long retryMs = 0) => reason switch
    {
        "not_permitted" => "not enabled for this camera (right-click the tile > Allow talk-back)",
        "rate_limited" => "too many attempts" + (retryMs > 0 ? string.Create(CultureInfo.InvariantCulture, $" (try again in {(retryMs + 999) / 1000} s)") : ""),
        "not_supported" => "this camera has no talk-back",
        "unsupported_ability" => "this camera's talk-back format is not supported",
        "busy" => "another client is talking",
        "device_refused" => "the device refused",
        "session_closed" or "connection_lost" => "the connection was lost",
        "timed_out" => "the device did not answer",
        "permission_revoked" => "talk-back was turned off",
        "idle" => "no audio for a while",
        "max_duration" => "the longest talk allowed",
        "tile_closed" => "the video closed",
        _ => reason.Replace('_', ' '),
    };
}

/// <summary>Who spoke: the camera's audio or this computer's microphone. One transcription session per source.</summary>
public enum TranscriptSource
{
    Visitor,
    You,
}

/// <summary>One line of the transcript.</summary>
public sealed record TranscriptLine(TranscriptSource Source, string Text, bool IsFinal)
{
    public static string Label(TranscriptSource source) => source == TranscriptSource.Visitor ? "Visitor" : "You";

    /// <summary>"Visitor: hello" (a phrase still in progress ends with an ellipsis).</summary>
    public string Display => $"{Label(Source)}: {Text}{(IsFinal ? "" : "…")}";
}

/// <summary>
/// The call's live transcript: finished phrases in order, and per source the phrase still being
/// recognized (replaced by each new partial result, then by the final one).
/// </summary>
public sealed class Transcript
{
    private readonly List<TranscriptLine> _finals = [];
    private readonly Dictionary<TranscriptSource, string> _partials = [];

    public IReadOnlyList<TranscriptLine> Finals => _finals;

    public bool IsEmpty => _finals.Count == 0 && _partials.Count == 0;

    /// <summary>A volatile result: replaces the source's phrase in progress.</summary>
    public void Partial(string text, TranscriptSource source)
    {
        string t = text.Trim();
        if (t.Length == 0)
        {
            _partials.Remove(source);
        }
        else
        {
            _partials[source] = t;
        }
    }

    /// <summary>A final result: appended; the source's phrase in progress is done.</summary>
    public void Final(string text, TranscriptSource source)
    {
        _partials.Remove(source);
        string t = text.Trim();
        if (t.Length > 0)
        {
            _finals.Add(new TranscriptLine(source, t, true));
        }
    }

    /// <summary>What the call shows: the finished lines, then the phrases in progress.</summary>
    public IReadOnlyList<TranscriptLine> Lines
    {
        get
        {
            var lines = new List<TranscriptLine>(_finals);
            foreach (TranscriptSource s in Enum.GetValues<TranscriptSource>())
            {
                if (_partials.TryGetValue(s, out string? p))
                {
                    lines.Add(new TranscriptLine(s, p, false));
                }
            }
            return lines;
        }
    }

    /// <summary>The saved form (only when the user turned saving on): finished lines, "Visitor: ..." and "You: ...".</summary>
    public string SavedText => string.Join("\n", _finals.Select(l => $"{TranscriptLine.Label(l.Source)}: {l.Text}"));
}

/// <summary>
/// Keeps the microphone's transcription from hearing the visitor twice: while the camera's audio
/// plays through the speakers and carries sound, and a moment after, the microphone's samples are
/// not transcribed.
/// </summary>
public sealed class EchoGate
{
    /// <summary>Peak level (0..1) above which the camera counts as playing.</summary>
    public float Threshold { get; init; } = 0.03f;

    /// <summary>How long after the camera goes quiet the microphone stays gated (room echo and the audio path's delay), s.</summary>
    public double Hold { get; init; } = 0.4;

    private double _gatedUntil = double.NegativeInfinity;

    /// <summary>Whether the microphone may be transcribed at <paramref name="now"/>, given whether the camera is audible and its level.</summary>
    public bool AllowsMicrophone(double now, bool cameraAudible, float cameraPeak)
    {
        if (cameraAudible && cameraPeak > Threshold)
        {
            _gatedUntil = now + Hold;
        }
        return now >= _gatedUntil;
    }
}

/// <summary>Where a transcription session stands, for the call's status line.</summary>
public enum TranscriptionPhase
{
    Preparing,
    Running,
    Unavailable,
    Finished,
}

/// <summary>A transcription session's status, with why it is unavailable.</summary>
public sealed record TranscriptionStatus(TranscriptionPhase Phase, string? Why = null)
{
    public static readonly TranscriptionStatus Preparing = new(TranscriptionPhase.Preparing);
}

/// <summary>
/// A speech recognizer for one source (docs/ARCHITECTURE.md "Live transcription"): one engine
/// interface per app, so the engine can change and tests can fake it. <see cref="Start"/> prepares
/// and begins a session; <see cref="Feed"/> takes 16 kHz mono samples from any thread;
/// <see cref="Finish"/> ends the session after what was fed. Results arrive through the callbacks,
/// on any thread.
/// </summary>
public interface ITranscriptionEngine : IDisposable
{
    /// <summary>A volatile result: the phrase so far (replaces the previous partial).</summary>
    Action<string>? OnPartial { get; set; }

    /// <summary>A final result for a phrase.</summary>
    Action<string>? OnFinal { get; set; }

    /// <summary>Progress, and why the engine cannot go on (the call continues without a transcript).</summary>
    Action<TranscriptionStatus>? OnStatus { get; set; }

    void Start(TranscriptSource source);

    void Feed(ReadOnlySpan<float> pcm16k);

    void Finish();
}

/// <summary>
/// One call: from the press (or the user's Start Conversation) to the close or about a minute
/// after the last activity (a press, speech, talk-back).
/// </summary>
public sealed class Call
{
    public const double IdleTimeout = 60.0;

    public Call(CameraRef camera, DateTime startedAt, double now, CallAudioDecision audio, CallOrigin origin = CallOrigin.Press)
    {
        Camera = camera;
        StartedAt = startedAt;
        LastActivity = now;
        Origin = origin;
        Presses = origin == CallOrigin.Press ? 1 : 0;
        Audible = audio.Audible;
        ShowsUnmute = audio.ShowsUnmute;
        MutedForCall = [.. audio.MutedForCall];
    }

    public CameraRef Camera { get; }

    public DateTime StartedAt { get; }

    public CallOrigin Origin { get; }

    /// <summary>When the last press, speech, or talk happened, on the viewer clock.</summary>
    public double LastActivity { get; private set; }

    public int Presses { get; private set; }

    /// <summary>The camera plays through the speakers.</summary>
    public bool Audible { get; private set; }

    /// <summary>The Unmute control covers the call.</summary>
    public bool ShowsUnmute { get; private set; }

    /// <summary>Cameras muted for the call; audible again when it ends (unless the user changed what plays meanwhile).</summary>
    public List<CameraRef> MutedForCall { get; }

    public TalkState Talk { get; } = new();

    public Transcript Transcript { get; } = new();

    public TranscriptionStatus Transcription { get; set; } = TranscriptionStatus.Preparing;

    /// <summary>Activity keeps the call open.</summary>
    public void Activity(double now) => LastActivity = Math.Max(LastActivity, now);

    /// <summary>Another press while the call is open.</summary>
    public void Pressed(double now)
    {
        Presses++;
        Activity(now);
    }

    /// <summary>The Unmute control was clicked.</summary>
    public void Unmuted()
    {
        Audible = true;
        ShowsUnmute = false;
    }

    /// <summary>Whether the call has ended by itself at <paramref name="now"/> (talking keeps it open).</summary>
    public bool Expired(double now) => !Talk.IsTalking && now - LastActivity >= IdleTimeout;

    /// <summary>The event log's line when it ends; the transcript only when saving is on.</summary>
    public string LogText(DateTime endedAt, bool saveTranscript)
    {
        int seconds = (int)Math.Round((endedAt - StartedAt).TotalSeconds, MidpointRounding.AwayFromZero);
        int channel = Camera.Channel + 1;
        string text = Presses == 0
            ? $"Conversation ended: channel {channel}, {seconds} s"
            : $"Doorbell call ended: channel {channel}, {Presses} press{(Presses == 1 ? "" : "es")}, {seconds} s";
        if (saveTranscript && Transcript.Finals.Count > 0)
        {
            text += "\n" + Transcript.SavedText;
        }
        return text;
    }

    /// <summary>The caption lines the call shows: the last few, and why there is no transcript yet.</summary>
    public (IReadOnlyList<string> Lines, string? Note) Captions(int maxLines = 4)
    {
        string[] lines = [.. Transcript.Lines.TakeLast(maxLines).Select(l => l.Display)];
        string? note = Transcription.Phase switch
        {
            TranscriptionPhase.Preparing => "Preparing speech recognition…",
            TranscriptionPhase.Unavailable => $"No transcript: {Transcription.Why}",
            _ => Transcript.IsEmpty ? "Listening for speech…" : null,
        };
        return (lines, note);
    }
}
