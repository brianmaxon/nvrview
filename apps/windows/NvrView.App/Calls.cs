// The calls in progress (docs/UX.md "Doorbell calls"), shared by every window: a doorbell press
// opens one, and so does the user's Start Conversation on a camera's tile (no press). The windows
// show them as cards over the stage (CallPanel).
//
// A call holds a lease on its camera (the tile stays open whatever the windows show), taps the
// tile's audio for the visitor's transcript, decides whether the camera is audible, and runs
// talk-back only when the user presses Talk or sends a typed reply: talk-back is camera motion
// control, off unless the user allowed it for that camera, and never started on its own. The
// rules (audio, talk state, transcript, expiry) are NvrView.Tiles/Calls.cs; this class connects
// them to the core, the microphone, the system voice, and the recognizer. UI thread only, except
// the tap and the microphone callbacks, which touch the engine and the echo gate alone.

using System.Collections.Concurrent;
using System.Diagnostics;
using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>Routes tapped audio (on the core's tap thread) to the transcription of the call whose tile it is.</summary>
internal sealed class TapRouter
{
    private readonly ConcurrentDictionary<ulong, ITranscriptionEngine> _routes = new();

    public void Add(ulong tile, ITranscriptionEngine engine) => _routes[tile] = engine;

    public void Remove(ulong tile) => _routes.TryRemove(tile, out _);

    /// <summary>From the core's tap thread: 16 kHz mono, borrowed for the call.</summary>
    public unsafe void Deliver(ulong tile, in NvAudioBlock block)
    {
        if (_routes.TryGetValue(tile, out ITranscriptionEngine? engine) && block.Samples != null && block.SampleCount > 0)
        {
            engine.Feed(new ReadOnlySpan<float>(block.Samples, (int)block.SampleCount));
        }
    }
}

/// <summary>The echo gate of one call, used on the microphone's thread; whether the camera is audible is set from the UI thread.</summary>
internal sealed class EchoBox
{
    private readonly EchoGate _gate = new();
    private volatile bool _audible;

    public bool CameraAudible
    {
        get => _audible;
        set => _audible = value;
    }

    public bool Allows(double now, float peak)
    {
        bool audible = _audible;
        lock (_gate)
        {
            return _gate.AllowsMicrophone(now, audible, peak);
        }
    }
}

internal sealed class CallSession(Call call)
{
    public Call Call { get; } = call;

    public ITranscriptionEngine? Visitor { get; set; }

    public ITranscriptionEngine? You { get; set; }

    /// <summary>Talk-back started by holding Talk or the toggle (the microphone).</summary>
    public bool MicTalk { get; set; }

    /// <summary>A typed reply waiting for the talk to become active, and its rate.</summary>
    public List<float>? PendingReply { get; set; }

    public uint PendingRate { get; set; }

    public EchoBox Echo { get; } = new();

    public CameraTile? Tapped { get; set; }

    /// <summary>A status note for the call (microphone refused, ...).</summary>
    public string? Note { get; set; }

    /// <summary>The confirmation to allow talk-back is on screen.</summary>
    public bool Asking { get; set; }

    /// <summary>What the user last started, and the typed reply's audio, so a refusal from the core can be answered by the same prompt.</summary>
    public TalkAction? LastAction { get; set; }

    public List<float>? LastReply { get; set; }
}

internal sealed class ViewerCalls
{
    private readonly Viewer _viewer;
    private readonly Dictionary<CameraRef, CallSession> _sessions = [];

    public ViewerCalls(Viewer viewer, CallServices services)
    {
        _viewer = viewer;
        Services = services;
        viewer.Leases.TileChanged += (camera, _) => TileChanged(camera);
    }

    /// <summary>The microphone, voice, and recognizer (the self-test swaps in fakes).</summary>
    public CallServices Services { get; set; }

    /// <summary>The cameras with a call open, oldest first.</summary>
    public IReadOnlyList<CameraRef> Cameras => [.. _sessions.Values.OrderBy(s => s.Call.StartedAt).Select(s => s.Call.Camera)];

    public bool IsEmpty => _sessions.Count == 0;

    /// <summary>The call for a camera (any lens of its channel), if one is open.</summary>
    public Call? CallFor(CameraRef camera) => _sessions.GetValueOrDefault(camera.ChannelKey)?.Call;

    public string? NoteFor(CameraRef camera) => _sessions.GetValueOrDefault(camera.ChannelKey)?.Note;

    public bool IsTalkingWithMicrophone(CameraRef camera) => _sessions.GetValueOrDefault(camera.ChannelKey)?.MicTalk == true;

    /// <summary>The recognizer of the camera's audio, for the self-test.</summary>
    public ITranscriptionEngine? VisitorEngine(CameraRef camera) => _sessions.GetValueOrDefault(camera.ChannelKey)?.Visitor;

    private double Now => _viewer.Clock.Now;

    private static string Channel(Call call) => (call.Camera.Channel + 1).ToString(System.Globalization.CultureInfo.InvariantCulture);

    // ---- opening ----

    /// <summary>A doorbell press (the device's <c>visitor</c> alarm start).</summary>
    public void Ring(CameraRef doorbell, DateTime at)
    {
        CameraRef camera = doorbell.ChannelKey;
        if (_sessions.TryGetValue(camera, out CallSession? existing))
        {
            existing.Call.Pressed(Now);
            AppLog.Info($"Doorbell pressed again: channel {camera.Channel + 1}");
            _viewer.RaiseCallsChanged();
            return;
        }
        CallAudioDecision decision = CallAudio.Press(camera, _viewer.AudibleCameras);
        Open(camera, at, decision, CallOrigin.Press);
        AppLog.Info($"Doorbell call: channel {camera.Channel + 1}, {(decision.Audible ? "audible" : "muted (Unmute shown)")}");
        _viewer.RaiseCallsChanged();
    }

    /// <summary>
    /// "Start conversation" on a camera's tile: the same call with no press, for captions, Talk, and
    /// typed replies. Nothing is sent by itself: talk-back still starts only when the user presses
    /// Talk or sends a reply, and the core refuses without the camera's permission. A call already
    /// open on the camera is kept as it is.
    /// </summary>
    public void StartConversation(CameraRef camera)
    {
        camera = camera.ChannelKey;
        if (_sessions.TryGetValue(camera, out CallSession? existing))
        {
            existing.Call.Activity(Now);
            _viewer.RaiseCallsChanged();
            return;
        }
        Open(camera, _viewer.WallClock(), CallAudio.Conversation(camera, _viewer.AudibleCameras), CallOrigin.Conversation);
        AppLog.Info($"Conversation started: channel {camera.Channel + 1}");
        _viewer.RaiseCallsChanged();
    }

    // Opens the call: lease, audio, and the visitor's transcript.
    private void Open(CameraRef camera, DateTime at, CallAudioDecision audio, CallOrigin origin)
    {
        var session = new CallSession(new Call(camera, at, Now, audio, origin));
        _sessions[camera] = session;
        _ = _viewer.Leases.Acquire(camera);
        if (audio.Audible)
        {
            // Audible at once: the cameras that were playing are muted for the call.
            _viewer.SetAudible([camera]);
        }
        session.Echo.CameraAudible = _viewer.IsAudible(camera);
        StartVisitorTranscript(session);
        TileChanged(camera);
    }

    /// <summary>The camera's tile opened or closed: its tap follows.</summary>
    private void TileChanged(CameraRef camera)
    {
        if (!_sessions.TryGetValue(camera.ChannelKey, out CallSession? s))
        {
            return;
        }
        CameraTile? tile = _viewer.Leases.Tile(s.Call.Camera);
        if (s.Tapped is { } old && old != tile)
        {
            _viewer.Host.Taps.Remove(old.Id);
            s.Tapped = null;
        }
        if (tile is null || s.Tapped is not null || s.Visitor is not { } visitor)
        {
            return;
        }
        _viewer.Host.Taps.Add(tile.Id, visitor);
        try
        {
            _viewer.Core.SetTileAudioTap(tile.Id, true);
            s.Tapped = tile;
        }
        catch (NvException e)
        {
            AppLog.Warn($"the call could not tap the audio of tile {tile.Id}: {e.Status}");
            _viewer.Host.Taps.Remove(tile.Id);
        }
    }

    private void StartVisitorTranscript(CallSession s)
    {
        ITranscriptionEngine engine = MakeEngine(TranscriptSource.Visitor, s);
        s.Visitor = engine;
        engine.Start(TranscriptSource.Visitor);
    }

    private ITranscriptionEngine MakeEngine(TranscriptSource source, CallSession s)
    {
        ITranscriptionEngine engine = Services.MakeTranscriber();
        CameraRef camera = s.Call.Camera;
        engine.OnPartial = text => _viewer.Host.Post(() => Transcribed(camera, source, text, final: false));
        engine.OnFinal = text => _viewer.Host.Post(() => Transcribed(camera, source, text, final: true));
        engine.OnStatus = status => _viewer.Host.Post(() =>
        {
            if (source == TranscriptSource.Visitor && _sessions.TryGetValue(camera, out CallSession? now))
            {
                now.Call.Transcription = status;
                _viewer.RaiseCallsChanged();
            }
        });
        return engine;
    }

    private void Transcribed(CameraRef camera, TranscriptSource source, string text, bool final)
    {
        if (!_sessions.TryGetValue(camera, out CallSession? s))
        {
            return;
        }
        if (final)
        {
            s.Call.Transcript.Final(text, source);
        }
        else
        {
            s.Call.Transcript.Partial(text, source);
        }
        // Speech is activity: the call stays open.
        s.Call.Activity(Now);
        _viewer.RaiseCallsChanged();
    }

    // The engine stops after what was fed; the wait for its thread is off the UI thread.
    private static void Retire(ITranscriptionEngine? engine)
    {
        if (engine is null)
        {
            return;
        }
        engine.Finish();
        _ = Task.Run(engine.Dispose);
    }

    // ---- audio ----

    /// <summary>The large Unmute control: the camera plays; whatever played is muted for the call.</summary>
    public void Unmute(CameraRef camera)
    {
        if (!_sessions.TryGetValue(camera.ChannelKey, out CallSession? s))
        {
            return;
        }
        foreach (CameraRef other in _viewer.AudibleCameras.Where(c => c != s.Call.Camera))
        {
            if (!s.Call.MutedForCall.Contains(other))
            {
                s.Call.MutedForCall.Add(other);
            }
        }
        s.Call.Unmuted();
        s.Call.Activity(Now);
        _viewer.SetAudible([s.Call.Camera]);
        s.Echo.CameraAudible = true;
        _viewer.RaiseCallsChanged();
    }

    /// <summary>What plays changed: the echo gate needs to know whether the camera is audible.</summary>
    public void AudibleChanged()
    {
        foreach ((CameraRef camera, CallSession s) in _sessions)
        {
            s.Echo.CameraAudible = _viewer.IsAudible(camera);
        }
    }

    // ---- talk-back ----

    /// <summary>
    /// Asks "Allow talk-back for the camera?" and applies the answer: Allow saves the setting as the
    /// tile menu does (which also tells the core). True when the user allowed it and the call is
    /// still open. One question at a time.
    /// </summary>
    private async Task<bool> AskToAllowAsync(CallSession s, TalkAction action)
    {
        if (s.Asking)
        {
            return false;
        }
        s.Asking = true;
        try
        {
            CameraRef camera = s.Call.Camera;
            bool allow = await Services.Prompter.AskAsync(TalkPermission.Prompt(_viewer.CameraName(camera)));
            TalkOutcome outcome = TalkPermission.Outcome(allow, action);
            if (outcome.AllowTalkBack)
            {
                _viewer.SetTalkAllowed(camera, true);
            }
            _viewer.RaiseCallsChanged();
            return outcome.ContinueAction && _sessions.GetValueOrDefault(camera.ChannelKey) == s;
        }
        finally
        {
            s.Asking = false;
        }
    }

    /// <summary>
    /// Talk pressed (held, or the toggle on): starts talk-back with the microphone. When the
    /// camera's talk-back is off the user is asked first (nothing is sent before the answer); a
    /// <c>not_permitted</c> refusal from the core asks the same way.
    /// </summary>
    public void StartTalking(CameraRef camera) => _ = StartTalkingAsync(camera);

    /// <summary>As <see cref="StartTalking"/>, finishing once the prompt (if any) is answered.</summary>
    public async Task StartTalkingAsync(CameraRef camera)
    {
        if (!_sessions.TryGetValue(camera.ChannelKey, out CallSession? s) || s.MicTalk || s.Asking)
        {
            return;
        }
        s.Call.Activity(Now);
        s.LastAction = TalkAction.Microphone;
        if (TalkPermission.Gate(_viewer.Settings.TalkAllowed(s.Call.Camera)) == TalkGate.Ask && !await AskToAllowAsync(s, TalkAction.Microphone))
        {
            return;
        }
        BeginMicrophoneTalk(s);
    }

    private void BeginMicrophoneTalk(CallSession s)
    {
        if (s.MicTalk)
        {
            return;
        }
        s.MicTalk = true;
        s.Note = null;
        if (!BeginTalk(s))
        {
            s.MicTalk = false;
        }
        else
        {
            StartMicrophone(s);
        }
        _viewer.RaiseCallsChanged();
    }

    /// <summary>Talk released (or the toggle off).</summary>
    public void StopTalking(CameraRef camera)
    {
        if (!_sessions.TryGetValue(camera.ChannelKey, out CallSession? s) || !s.MicTalk)
        {
            return;
        }
        s.MicTalk = false;
        Services.Microphone.Stop();
        Retire(s.You);
        s.You = null;
        s.Call.Activity(Now);
        if (s.PendingReply is null && _viewer.Leases.Tile(s.Call.Camera) is { } tile)
        {
            StopTalkQuietly(tile, drain: false);
        }
        s.Call.Talk.StopRequested();
        _viewer.RaiseCallsChanged();
    }

    // Asks the core for talk-back unless it runs already; false if refused at once.
    private bool BeginTalk(CallSession s)
    {
        if (_viewer.Leases.Tile(s.Call.Camera) is not { } tile)
        {
            s.Call.Talk.StartFailed(notPermitted: false, tileClosed: true);
            return false;
        }
        if (!s.Call.Talk.RequestStart())
        {
            return true;
        }
        // The permission is the user's setting; the core keeps it for this run only.
        _viewer.ApplyTalkPermission(s.Call.Camera);
        try
        {
            _viewer.Core.TalkStart(tile.Id);
            s.Call.Talk.StartSent();
            AppLog.Info($"Talk-back started: {_viewer.CameraName(s.Call.Camera)}");
            return true;
        }
        catch (NvException e)
        {
            s.Call.Talk.StartFailed(e.Status == NvStatus.NotPermitted);
            AppLog.Info(TalkPermission.Refusal(_viewer.CameraName(s.Call.Camera), e.Status == NvStatus.NotPermitted ? "not_permitted" : "error"));
            return false;
        }
    }

    private void StartMicrophone(CallSession s)
    {
        if (_viewer.Leases.Tile(s.Call.Camera) is not { } tile)
        {
            return;
        }
        ITranscriptionEngine you = MakeEngine(TranscriptSource.You, s);
        s.You = you;
        you.Start(TranscriptSource.You);
        NvCore core = _viewer.Core;
        EchoBox echo = s.Echo;
        echo.CameraAudible = _viewer.IsAudible(s.Call.Camera);
        string? problem = Services.Microphone.Start(samples =>
        {
            try
            {
                core.TalkPush(tile.Id, samples, 16_000);
                // Not while the camera plays through the speakers (echo).
                float peak = core.TileAudioLevel(tile.Id).Peak;
                if (echo.Allows(Stopwatch.GetTimestamp() / (double)Stopwatch.Frequency, peak))
                {
                    you.Feed(samples);
                }
            }
            catch (NvException)
            {
                // The tile or the core is going away; the talk ends with its own event.
            }
        });
        if (problem is not null)
        {
            s.Note = $"Talk-back: {problem}";
            StopTalking(s.Call.Camera);
        }
    }

    /// <summary>
    /// Send: speaks <paramref name="text"/> (what the user typed in the call, nothing else) through
    /// the camera with the system voice, under the same permission and limits as Talk.
    /// </summary>
    public async Task<bool> SendReplyAsync(string text, CameraRef camera, Action? accepted = null)
    {
        string typed = text.Trim();
        if (!_sessions.TryGetValue(camera.ChannelKey, out CallSession? s) || typed.Length == 0 || s.Asking)
        {
            return false;
        }
        s.Call.Activity(Now);
        s.LastAction = TalkAction.Reply;
        // Asked before anything is rendered or sent. Cancel drops nothing: the caller keeps the text.
        if (TalkPermission.Gate(_viewer.Settings.TalkAllowed(s.Call.Camera)) == TalkGate.Ask && !await AskToAllowAsync(s, TalkAction.Reply))
        {
            return false;
        }
        accepted?.Invoke();
        AppLog.Info($"Typed reply sent: {_viewer.CameraName(s.Call.Camera)}, {typed.Length} characters");
        (float[] samples, uint rate) = await Services.Speech.RenderAsync(typed);
        if (!_sessions.TryGetValue(camera.ChannelKey, out s) || samples.Length == 0)
        {
            return true;
        }
        s.LastReply = [.. samples];
        s.PendingReply ??= [];
        s.PendingReply.AddRange(samples);
        s.PendingRate = rate;
        if (s.Call.Talk.IsActive)
        {
            FlushReply(s);
        }
        else if (!BeginTalk(s))
        {
            s.PendingReply = null;
        }
        _viewer.RaiseCallsChanged();
        return true;
    }

    /// <summary>
    /// The core refused for lack of permission although the setting is on (out of sync) or was off:
    /// the same prompt; Allow tells the core again and repeats what the user asked.
    /// </summary>
    private async Task CoreRefusedPermissionAsync(CallSession s, bool wasMic, List<float>? reply, uint rate)
    {
        TalkAction action = wasMic ? TalkAction.Microphone : s.LastAction ?? TalkAction.Microphone;
        if (!await AskToAllowAsync(s, action))
        {
            return;
        }
        if (action == TalkAction.Reply && reply is not null)
        {
            s.PendingReply = [.. reply];
            s.PendingRate = rate;
            if (!BeginTalk(s))
            {
                s.PendingReply = null;
            }
        }
        else
        {
            BeginMicrophoneTalk(s);
        }
        _viewer.RaiseCallsChanged();
    }

    private void FlushReply(CallSession s)
    {
        if (s.PendingReply is not { } samples || _viewer.Leases.Tile(s.Call.Camera) is not { } tile)
        {
            return;
        }
        s.PendingReply = null;
        try
        {
            _viewer.Core.TalkPush(tile.Id, System.Runtime.InteropServices.CollectionsMarshal.AsSpan(samples), s.PendingRate);
            // Ends after what was pushed has been sent, unless Talk is held.
            if (!s.MicTalk)
            {
                _viewer.Core.TalkStop(tile.Id, true);
            }
        }
        catch (NvException e)
        {
            AppLog.Warn($"the typed reply could not be sent: {e.Status}");
        }
    }

    private void StopTalkQuietly(CameraTile tile, bool drain)
    {
        try
        {
            _viewer.Core.TalkStop(tile.Id, drain);
        }
        catch (NvException)
        {
            // The tile is already gone.
        }
    }

    /// <summary>A <c>talk</c> event for a tile.</summary>
    public void OnTalkEvent(CoreEvent e)
    {
        if (_viewer.FindTile(e.Tile) is not { } tile || _sessions.GetValueOrDefault(tile.Camera.ChannelKey) is not { } s)
        {
            return;
        }
        // The events of an earlier start that arrive late change nothing.
        if (!s.Call.Talk.Apply(new TalkEvent(e.State ?? "", e.Reason, e.RetryMs, e.SentMs)))
        {
            return;
        }
        s.Call.Activity(Now);
        string name = _viewer.CameraName(s.Call.Camera);
        switch (e.State)
        {
            case "active":
                FlushReply(s);
                break;
            case "refused":
            case "stopped":
                List<float>? lastReply = s.LastReply;
                bool wasMic = s.MicTalk;
                s.PendingReply = null;
                if (s.MicTalk)
                {
                    s.MicTalk = false;
                    Services.Microphone.Stop();
                    Retire(s.You);
                    s.You = null;
                }
                AppLog.Info(e.State == "refused"
                    ? TalkPermission.Refusal(name, e.Reason ?? "refused", e.RetryMs)
                    : $"Talk-back stopped: {name}, {e.Reason ?? "stopped"}, {e.SentMs} ms sent");
                if (e.State == "refused" && e.Reason == "not_permitted")
                {
                    _ = CoreRefusedPermissionAsync(s, wasMic, lastReply, s.PendingRate);
                }
                break;
        }
        _viewer.RaiseCallsChanged();
    }

    // ---- time and the end ----

    /// <summary>Ends calls a minute after their last activity (the slow tick; the self-test's clock).</summary>
    public void Tick(double now)
    {
        foreach ((CameraRef camera, CallSession s) in _sessions.ToArray())
        {
            if (s.Call.Expired(now))
            {
                End(camera, "no activity");
            }
        }
    }

    /// <summary>
    /// Ends a call (closed, timed out, or the device went away): talk-back stops, the tap and the
    /// transcription end, the audio goes back to what played before, the lease is released, and the
    /// call is logged (no transcript is saved: Windows has no setting for it yet).
    /// </summary>
    public void End(CameraRef camera, string reason)
    {
        if (!_sessions.Remove(camera.ChannelKey, out CallSession? s))
        {
            return;
        }
        if (s.MicTalk)
        {
            Services.Microphone.Stop();
        }
        if (_viewer.Leases.Tile(s.Call.Camera) is { } tile && (s.Call.Talk.IsTalking || s.PendingReply is not null))
        {
            StopTalkQuietly(tile, drain: false);
        }
        if (s.Tapped is { } tapped)
        {
            _viewer.Host.Taps.Remove(tapped.Id);
            try
            {
                _viewer.Core.SetTileAudioTap(tapped.Id, false);
            }
            catch (NvException)
            {
                // The tile is already gone.
            }
        }
        Retire(s.Visitor);
        Retire(s.You);
        // Back to what played before, if the user did not change it meanwhile.
        if (_viewer.AudibleCameras.Count == 1 && _viewer.IsAudible(s.Call.Camera))
        {
            _viewer.SetAudible(s.Call.MutedForCall);
        }
        _viewer.Leases.Release(s.Call.Camera);
        AppLog.Info(s.Call.LogText(_viewer.WallClock(), saveTranscript: false) + $" ({reason})");
        _viewer.RaiseCallsChanged();
    }

    /// <summary>Ends the calls of a device that is going away.</summary>
    public void EndDevice(string deviceKey)
    {
        foreach (CameraRef camera in _sessions.Keys.Where(c => c.Device == deviceKey).ToArray())
        {
            End(camera, "device removed");
        }
    }

    /// <summary>Ends every call (shutdown).</summary>
    public void EndAll()
    {
        foreach (CameraRef camera in _sessions.Keys.ToArray())
        {
            End(camera, "closed");
        }
    }
}
