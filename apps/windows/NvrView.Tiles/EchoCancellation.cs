// Echo cancellation for talk-back (docs/AUDIO.md "Echo cancellation"): while the microphone is on
// in a call, the system's echo canceller removes what the computer's own speakers play (the camera)
// from what the microphone hears, so the visitor does not hear themselves sent back and the user
// can talk while the visitor is heard. These are the platform-neutral rules, the same as the Mac
// app's (NVRCore/EchoCancellation.swift), with the same test vectors.

namespace NvrView.Tiles;

/// <summary>What the system reports about its echo canceller on the capture stream.</summary>
public enum EchoEffect
{
    /// <summary>Present and on.</summary>
    On,

    /// <summary>Present but turned off (by the user or the driver).</summary>
    Off,

    /// <summary>Not offered by the microphone's processing.</summary>
    Absent,

    /// <summary>The system cannot say (an older system without the query).</summary>
    Unknown,
}

/// <summary>What the platform reported when the microphone started.</summary>
/// <param name="Processing">The voice-processing (communications) stream was set up.</param>
/// <param name="Failure">Why it was not, in words.</param>
/// <param name="Effect">The system's echo canceller on the capture stream.</param>
/// <param name="ReferenceIsPlayback">The echo canceller's reference is the output that plays the camera.</param>
public sealed record EchoProbe(bool Processing, string? Failure = null, EchoEffect Effect = EchoEffect.Unknown, bool ReferenceIsPlayback = false)
{
    /// <summary>The system's voice processing could not be used at all.</summary>
    public static EchoProbe Failed(string why) => new(false, why);
}

/// <summary>Whether the system cancels the camera's echo in the microphone, or the app falls back to the caption gate.</summary>
/// <param name="Active">The system's echo canceller is on, with the camera's output as its reference.</param>
/// <param name="Why">When not active: why, in words.</param>
public sealed record EchoCancellation(bool Active, string? Why = null)
{
    public static readonly EchoCancellation System = new(true);

    public static EchoCancellation Gated(string why) => new(false, why);

    /// <summary>Only what the system confirms counts: an echo canceller the system cannot report on is treated as absent.</summary>
    public static EchoCancellation Decide(EchoProbe p)
    {
        if (!p.Processing)
        {
            return Gated(p.Failure ?? "the system's voice processing did not start");
        }
        switch (p.Effect)
        {
            case EchoEffect.Absent:
                return Gated("the microphone has no echo cancellation");
            case EchoEffect.Off:
                return Gated("echo cancellation is turned off for the microphone");
            case EchoEffect.Unknown:
                return Gated("the system does not report echo cancellation");
        }
        return p.ReferenceIsPlayback ? System : Gated("echo cancellation follows another output device");
    }

    /// <summary>The call's status note while the microphone is on (why there is none, and what it means for the captions, is in the log line).</summary>
    public string Note => Active ? "Echo cancellation on" : "No echo cancellation";

    /// <summary>The log line (see <see cref="EchoCancellationLog"/>).</summary>
    public string LogLine => Active
        ? "Talk-back microphone: echo cancellation on"
        : $"Talk-back microphone: echo cancellation unavailable ({Why}); captions of the microphone pause while the camera plays";
}

/// <summary>Logs each distinct outcome once per run, so a fallback that repeats on every press of Talk is not logged every time.</summary>
public sealed class EchoCancellationLog
{
    private readonly HashSet<string> _logged = [];

    /// <summary>The line to log for <paramref name="state"/>, or null when it was logged already.</summary>
    public string? Line(EchoCancellation state) => _logged.Add(state.LogLine) ? state.LogLine : null;
}

/// <summary>
/// Decides when the microphone's own captions pause because the camera plays through the speakers.
/// Talk-back itself is never gated: what the microphone hears always goes to the camera
/// (echo-cancelled when the system can).
/// <list type="bullet">
/// <item>Without echo cancellation: the half-duplex gate (<see cref="EchoGate"/>, a peak over 0.03
/// and 0.4 s after), so the visitor is not captioned twice.</item>
/// <item>With it: only a short safety duck when the camera is very loud (a peak over 0.5, 0.15 s
/// after). A canceller is weakest exactly then: the speakers distort and the echo is no longer a
/// linear copy of the reference, and a residue would be captioned as the user's words.</item>
/// </list>
/// </summary>
public sealed class MicrophoneEchoGate
{
    public const float LoudThreshold = 0.5f;

    public const double LoudHold = 0.15;

    private readonly EchoGate _gate = new();
    private readonly EchoGate _duck = new() { Threshold = LoudThreshold, Hold = LoudHold };

    /// <summary>Whether the microphone may be captioned at <paramref name="now"/>. Both rules see every block, so switching between them mid-call keeps their holds.</summary>
    public bool AllowsMicrophone(double now, bool cameraAudible, float cameraPeak, bool echoCancelled)
    {
        bool gated = _gate.AllowsMicrophone(now, cameraAudible, cameraPeak);
        bool ducked = _duck.AllowsMicrophone(now, cameraAudible, cameraPeak);
        return echoCancelled ? ducked : gated;
    }
}
