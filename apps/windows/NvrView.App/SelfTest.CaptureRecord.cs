// The recording steps of the self-test (docs/CAPTURE.md "Live recording", "Recording calls"). They run on
// the real core against the mock-nvr, with the real AAC encoder: the files are written into the run's own
// Recordings folder and read back with Mp4Inspector. Each step carries the macOS step's parity ID.

using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.System;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    /// <summary>
    /// The encoder (a tone through it and back with the system's AAC decoder: pitch, level, and the delay the file will
    /// say), then R on a tile: it records from the next keyframe, the pill stays when the header fades, the header's
    /// Record becomes Stop and the window header lists the recording, Stop ends it ("Recording saved") and the file
    /// reads back; the free-space guard and the main-stream limit say why; Stop all ends what runs.
    /// </summary>
    private async Task CaptureRecordSteps(Viewer v, StageView stage, StageTile tile)
    {
        CaptureController cap = v.Capture;
        if (stage.ChromeFor(tile) is not { } chrome)
        {
            _problems.Add("capture: no chrome for the tile to record");
            return;
        }
        CameraRef camera = tile.Camera;

        // parity: capture-encoder-priming
        uint reported = 0;
        int? measured = null;
        AacRoundTripResult? tone = null;
        await Task.Run(() =>
        {
            reported = AacRoundTrip.Priming;
            measured = AacRoundTrip.MeasureImpulse();
            tone = AacRoundTrip.Run(ToneCheck.Sine(1000, 0.25, 47 * AacCaptureEncoder.FrameSamples, 48_000), 48_000, flush: true);
        });
        double hz = 0, db = -120;
        if (tone is not null && reported <= 4096)
        {
            hz = ToneCheck.Frequency(tone.Decoded, (int)reported + 4800, 24_000, 48_000);
            db = ToneCheck.Decibels(ToneCheck.Rms(tone.Decoded, (int)reported + 4800, 24_000));
        }
        Say($"capture: AAC priming reported {reported}, impulse back at {measured?.ToString() ?? "?"}; a 1 kHz tone at -15 dB comes back at {hz:0.0} Hz, {db:0.0} dB " +
            $"({tone?.Units.Count ?? 0} units for 47 frames, {tone?.Decoded.Length ?? 0} samples decoded, config {Convert.ToHexString(tone?.AudioSpecificConfig ?? [])}); encoder installed {cap.Encoder is not null}, core configured {cap.Configured}");
        Expect(measured is { } m && Math.Abs(m - (int)reported) <= 48 && reported <= 4096, $"capture: the encoder's priming {reported} is not where the audio comes back ({measured})");
        Expect(tone is not null && Math.Abs(hz - 1000) < 10, $"capture: the tone came back at {hz:0.0} Hz");
        Expect(tone is not null && Math.Abs(db - -15.05) < 2, $"capture: the tone came back at {db:0.0} dB");
        Expect(tone is { AudioSpecificConfig.Length: >= 2 } t && t.AudioSpecificConfig[0] >> 3 == 2, "capture: the encoder's AudioSpecificConfig is not AAC-LC");
        Expect(cap.Encoder is not null && cap.Configured, "capture: the encoder was not installed or the core not configured");

        // parity: capture-record-tile
        stage.Select(tile.Path);
        stage.HandleKey(VirtualKey.R, false, false);
        BoardEntry? started = cap.Board.LiveOf(camera);
        bool recording = await Until(() => cap.Board.LiveOf(camera)?.State == "recording", 30);
        stage.Tick();
        BoardEntry? entry = cap.Board.LiveOf(camera);
        string? label = stage.HeaderTileOf(tile) is { } model ? TileHeaderLayout.Menu(model).FirstOrDefault(e => e.Item == TileHeaderItem.Record)?.Label : null;
        Say($"capture: R started {(started is null ? "nothing" : Path.GetFileName(started.File))}, state {entry?.State ?? "none"}, Record says {label ?? "?"}");
        Expect(started is not null, $"capture: R did not start a recording ({stage.ToastText ?? "no message"})");
        Expect(recording, "capture: the recording did not reach its first keyframe");
        Expect(label == "Stop Recording", "capture: the header's Record did not become Stop");
        Expect(started is not null && started.File.StartsWith(cap.Settings.LiveFolder, StringComparison.OrdinalIgnoreCase), "capture: the recording is not in the recordings folder");
        await Until(() => cap.Board.LiveOf(camera) is { DurationMs: >= 1500 }, 30);

        // parity: capture-record-indicator
        stage.Tick();
        string? pill = chrome.PillText;
        bool wasShown = chrome.HeaderShown;
        chrome.SetHover(false, false);
        bool stays = chrome.PillText is not null;
        chrome.SetHover(wasShown, false);
        bool narrow = stage.HeaderTileOf(tile) is { } narrowModel && TileHeaderLayout.Compute(100, 200, narrowModel).Visible.Contains(TileHeaderItem.RecordingPill);
        HeaderBar header = app.Windows[0].Header;
        header.Update();
        string? spot = header.RecordingSpotText;
        List<string> menu = Texts(header.BuildRecordingMenu().Items);
        int stops = menu.Count(t => t.StartsWith("Stop ", StringComparison.Ordinal) && t != "Stop all recordings");
        List<string> overflow = Texts(header.BuildOverflow().Items);
        Say($"capture: pill {pill ?? "none"} in the {chrome.PillCorner} corner, stays when the header fades {stays}, at 100 DIP {narrow}; window header \"{spot ?? "none"}\", " +
            $"its menu [{string.Join(" / ", menu)}], the … menu has Stop all {overflow.Contains("Stop all recordings")} and Open folder {overflow.Contains("Open recordings folder")}");
        Expect(pill?.StartsWith("● REC", StringComparison.Ordinal) == true && stays, "capture: the tile's pill did not show or did not stay");
        Expect(narrow, "capture: a narrow header dropped the recording pill");
        Expect(spot == "● 1 recording", $"capture: the window header said \"{spot ?? "nothing"}\"");
        Expect(stops == 1 && menu.Contains("Stop all recordings") && menu.Contains("Open recordings folder"), "capture: the recordings menu does not list the recording, Stop all, and the folder");
        Expect(overflow.Contains("Stop all recordings") && overflow.Contains("Open recordings folder"), "capture: the … menu lacks Stop all recordings or Open recordings folder");

        // The header's Record, now Stop, ends it.
        stage.PerformHeaderItem(tile, TileHeaderItem.Record, null);
        bool stopped = await Until(() => cap.Board.Running.Count == 0, 20);
        stage.Tick();
        RecordingEvent? end = cap.Board.Ended.LastOrDefault();
        string? toast = stage.ToastText;
        string? action = stage.ToastActionTitle;
        header.Update();
        Say($"capture: Stop ended it {end?.State ?? "no"} ({end?.Reason ?? "-"}), message \"{toast ?? "none"}\" with {action ?? "no button"}, pill {chrome.PillText ?? "gone"}, window header {(header.RecordingSpotVisible ? "still shows" : "hidden")}");
        Expect(stopped && end is { State: "finished", Reason: "stopped" }, "capture: Stop did not finish the recording");
        Expect(toast == "Recording saved" && action == "Show in folder", $"capture: the end said \"{toast}\" with {action}");
        Expect(chrome.PillText is null && !header.RecordingSpotVisible, "capture: the pill or the window header's count stayed after the recording ended");

        // parity: capture-record-real-core
        await CaptureFileCheck(end, "the live recording", video: 1, audio: end?.Audio == "passthrough" ? 1 : 0, enabled: null);

        await CaptureLimitSteps(v, stage, tile);
    }

    /// <summary>The free-space guard and the main-stream limit, as messages.</summary>
    private async Task CaptureLimitSteps(Viewer v, StageView stage, StageTile tile)
    {
        CaptureController cap = v.Capture;

        // parity: capture-record-disk-low
        cap.MinFreeOverride = 1UL << 62;
        stage.PerformHeaderItem(tile, TileHeaderItem.Record, null);
        string? refused = stage.ToastText;
        bool none = cap.Board.Running.Count == 0;
        cap.MinFreeOverride = null;
        Say($"capture: low disk says \"{refused ?? "nothing"}\", nothing started {none}");
        Expect(refused?.Contains("free disk space", StringComparison.Ordinal) == true && none, "capture: low disk did not refuse with its reason");

        // parity: capture-record-main-limit
        CameraRef[] cameras = [.. Tiles.Where(t => t.Camera.Lens == 0 && t.Camera.Device == tile.Camera.Device && t.Shown > 0).Select(t => t.Camera).Distinct().Take(3)];
        if (cameras.Length < 3)
        {
            Say("capture: fewer than three cameras of one device are playing; the main-stream limit is not checked");
            return;
        }
        cap.StartLive(cameras[0], RecordQuality.Main);
        cap.StartLive(cameras[1], RecordQuality.Main);
        cap.StartLive(cameras[2], RecordQuality.Main);
        string message = stage.ToastText ?? "nothing";
        string? offer = stage.ToastActionTitle;
        if (offer == "Record Sub-stream")
        {
            stage.PressToastAction();
        }
        BoardEntry? third = cap.Board.LiveOf(cameras[2]);
        Say($"capture: a third main stream says \"{message}\", offers {offer ?? "nothing"}, then {third?.Tier ?? "nothing"}");
        Expect(message.Contains("two main streams", StringComparison.Ordinal) && offer == "Record Sub-stream" && third?.Tier == "sub", "capture: the main-stream limit did not offer the sub-stream");
        bool allRecording = await Until(() => cap.Board.List.Count == 3 && cap.Board.List.All(r => r.State == "recording"), 30);
        Expect(allRecording, "capture: three recordings (two main, one sub) did not all reach their keyframe");
        app.Windows[0].RunHeaderAction(HeaderAction.StopAllRecordings);
        Expect(await Until(() => cap.Board.Running.Count == 0, 20), "capture: Stop all recordings left recordings running");
    }

    /// <summary>
    /// Reads a finished recording back (docs/CAPTURE.md "Files"): one video track starting on a keyframe that lasts what the
    /// events said, and the tracks asked for; <paramref name="enabled"/> (a call's) lists which sound tracks are enabled.
    /// </summary>
    private async Task CaptureFileCheck(RecordingEvent? end, string what, int video, int audio, bool[]? enabled)
    {
        if (end is null || !File.Exists(end.File))
        {
            _problems.Add($"capture (real core): {what} has no file to read back");
            return;
        }
        Mp4Info? info = null;
        try
        {
            byte[] bytes = await File.ReadAllBytesAsync(end.File);
            info = Mp4Inspector.Inspect(bytes);
        }
        catch (IOException e)
        {
            _problems.Add($"capture (real core): {what} could not be read ({e.GetType().Name})");
            return;
        }
        if (info is null)
        {
            _problems.Add($"capture (real core): {what} is not a finished MP4");
            return;
        }
        Mp4Track? first = info.Video.FirstOrDefault();
        double seconds = first?.Seconds ?? 0;
        Say($"capture (real core): {what} {Path.GetFileName(end.File)}, {info.Video.Count()} video, {info.Audio.Count()} sound ({end.Audio}), " +
            $"{seconds:0.00} s for {end.DurationMs} ms, keyframe start {first?.FirstSampleIsSync}, sound samples [{string.Join(", ", info.Audio.Select(a => a.Samples))}], " +
            $"enabled [{string.Join(", ", info.Audio.Select(a => a.Enabled))}]");
        Expect(!info.Fragmented && info.Video.Count() == video && first is { Samples: > 0, FirstSampleIsSync: true }, $"capture (real core): {what}'s video is wrong");
        Expect(Math.Abs(seconds - end.DurationMs / 1000.0) <= 0.5, $"capture (real core): {what} lasts {seconds:0.00} s, the events said {end.DurationMs} ms");
        Expect(info.Audio.Count() == audio && info.Audio.All(a => a.Codec == "mp4a" && a.Samples > 0), $"capture (real core): {what}'s sound tracks are wrong ({info.Audio.Count()})");
        if (enabled is not null)
        {
            Expect(info.Audio.Select(a => a.Enabled).SequenceEqual(enabled), $"capture (real core): {what}'s enabled sound tracks are [{string.Join(", ", info.Audio.Select(a => a.Enabled))}]");
        }
    }

    /// <summary>
    /// Ctrl+Shift+R in a call window records the call (docs/CAPTURE.md "Recording calls"): beside the transcripts under
    /// the stem the transcript will take, the four tracks through the app's AAC encoder, the call bar's "● Recording"
    /// pill, and the key again stops it.
    /// </summary>
    private async Task CallRecordButton(Viewer v, CallWindow cw, CameraRef camera, StageTile tile)
    {
        StageView stage = cw.Stage;
        CaptureController cap = v.Capture;
        // parity: capture-call-record-button
        long answeredBefore = cap.Encoder?.Answered ?? 0;
        stage.ApplyAction(TileAction.ToggleRecording, tile);
        BoardEntry? entry = cap.CallRecording(camera);
        string? stem = cap.ReservedCallStem(camera);
        bool recording = await Until(() => cap.CallRecording(camera)?.State == "recording", 30);
        stage.Tick();
        cw.Refresh();
        entry = cap.CallRecording(camera);
        string? pill = cw.Card.RecordingPillText;
        string? label = stage.HeaderTileOf(tile) is { } model ? TileHeaderLayout.Menu(model).FirstOrDefault(e => e.Item == TileHeaderItem.Record)?.Label : null;
        bool beside = entry is not null && string.Equals(Path.GetDirectoryName(entry.File), v.Transcripts.Folder, StringComparison.OrdinalIgnoreCase);
        bool named = stem is not null && entry is not null && Path.GetFileName(entry.File) == stem + ".mp4";
        await Until(() => cap.CallRecording(camera) is { DurationMs: >= 1000 }, 20);
        stage.ApplyAction(TileAction.ToggleRecording, tile);
        bool ended = await Until(() => cap.CallRecording(camera) is null, 20);
        RecordingEvent? end = cap.Board.Ended.LastOrDefault(e => e.Kind == "call");
        long answered = (cap.Encoder?.Answered ?? 0) - answeredBefore;
        stage.Tick();
        cw.Refresh();
        Say($"capture: call recording {(entry is null ? "none" : Path.GetFileName(entry.File))}, beside the transcripts {beside}, named for the transcript {named}, pill {pill ?? "none"}, " +
            $"Record says {label ?? "?"}, tracks {end?.Tracks ?? "?"}, ended {end?.State ?? "no"}, encoder answered {answered} refused {cap.Encoder?.Refused}");
        Expect(entry is not null && recording, $"capture: Ctrl+Shift+R did not start the call recording ({stage.ToastText ?? "no message"}, {cap.LastRefusal})");
        Expect(beside && named, "capture: the call recording is not beside the transcripts under its stem");
        Expect(pill?.StartsWith("● Recording", StringComparison.Ordinal) == true, "capture: the call bar showed no pill");
        Expect(label == "Stop Recording Call", "capture: Record Call did not become Stop");
        Expect(ended && end is { Kind: "call", Tracks: "video,Conversation,Visitor,You" }, $"capture: the call recording's tracks were {end?.Tracks}");
        Expect(answered > 0 && (cap.Encoder?.Refused ?? 1) == 0, $"capture: the AAC encoder answered {answered} units");
        Expect(cw.Card.RecordingPillText is null, "capture: the pill stayed after the call recording stopped");
    }

    /// <summary>
    /// A conversation's recording ends with the call (docs/CAPTURE.md "Recording calls"), and the file reads back: the
    /// video and three sound tracks, only the Conversation enabled, each with samples.
    /// </summary>
    private async Task CallRecordingEndsWithCall(Viewer v, CameraRef camera)
    {
        CaptureController cap = v.Capture;
        // parity: capture-call-record-real-core
        v.Calls.StartConversation(camera);
        bool tileOpen = await Until(() => v.Calls.ConversationTileFor(camera) is not null, 10);
        cap.StartCall(camera);
        BoardEntry? entry = cap.CallRecording(camera);
        if (entry is null)
        {
            _problems.Add($"capture (real core): the call recording did not start ({cap.LastRefusal ?? "?"}, tile open {tileOpen})");
            v.Calls.End(camera, "closed");
            return;
        }
        ulong id = entry.Id;
        bool long_ = await Until(() => cap.Board.Running.TryGetValue(id, out BoardEntry? e) && e.DurationMs >= 1500, 30);
        // The recording ends with the call.
        v.Calls.End(camera, "closed");
        bool ended = await Until(() => !cap.Board.Running.ContainsKey(id), 20);
        RecordingEvent? end = cap.Board.Ended.LastOrDefault(e => e.Recording == id);
        Say($"capture (real core): call recording ran {long_}, ended with the call {ended} ({end?.State ?? "no"}, {end?.Reason ?? "-"}), tracks {end?.Tracks ?? "?"}");
        Expect(ended && end is { State: "finished", Reason: "stopped" }, $"capture (real core): the call recording ended {end?.State} ({end?.Reason ?? "-"}) or not with the call");
        await CaptureFileCheck(end, "the call recording", video: 1, audio: 3, enabled: [true, false, false]);
    }
}
