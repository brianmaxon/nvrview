// Self-test area "calls": doorbell calls, the call window, captions, talk-back, and the call insets.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Graphics;
using NvThumbKind = NvrView.Interop.NvThumbKind;
using Point = Windows.Foundation.Point;
using RecordedThumbnail = NvrView.Interop.RecordedThumbnail;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    // 11b. Calls.
    private async Task Calls(Viewer v, StageView stage)
    {
        DeviceEntry device = v.Devices[0];
        var camera = new CameraRef(device.Key, 0);
        var mic = new FakeMicrophone();
        var voice = new FakeSpeech { Rate = 22_050 };
        var engines = new List<FakeTranscriber>();
        var prompter = new FakeTalkPrompter();
        v.Calls.Services = new CallServices(mic, voice, () =>
        {
            var engine = new FakeTranscriber();
            engines.Add(engine);
            return engine;
        }, prompter);
        var clock = new ManualViewerClock(3000);
        v.UseClock(clock);
        v.SetAudible([]);
        int leasesBefore = v.Leases.LeaseCount(camera);

        // parity: call-tile-menu
        // The tile menu offers the conversation and the permission.
        StageTile menuTile = Tiles.First(t => t.Camera == camera);
        var menu = stage.BuildTileMenu(menuTile)!;
        List<string> items = Texts(menu.Items);
        bool allowChecked = menu.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Allow talk-back")?.IsChecked == true;
        Say($"calls: tile menu {string.Join(" / ", items)}, allow checked {allowChecked}");
        Expect(items.Contains("Start Conversation") && items.Contains("Allow talk-back") && !allowChecked, "the tile menu does not offer Start Conversation and an unchecked Allow talk-back");

        // parity: call-conversation-start
        // 1. A conversation opens with no press and sends nothing by itself.
        v.Calls.StartConversation(camera);
        Call? call = v.Calls.CallFor(camera);
        Expect(call is { Origin: CallOrigin.Conversation, Presses: 0 }, "Start Conversation did not open a call with no press");
        Expect(v.IsAudible(camera) && call is { Audible: true, ShowsUnmute: false }, "a conversation did not make the camera audible");
        // The call holds a lease, and so does its card in this window (for the picture).
        Expect(v.Leases.LeaseCount(camera) == leasesBefore + 2 && stage.CallCards.LeasesHeld == 1, "the call and its card did not each hold a lease on the camera");
        Expect(call?.Talk.Current == TalkState.Phase.Idle && call.Talk.Outstanding == 0, "a conversation started talk-back by itself");
        stage.CallCards.Sync();
        CallCard? card = stage.CallCards.Cards.FirstOrDefault();
        Say($"calls: conversation card \"{card?.TitleText}\", {stage.CallCards.Cards.Count} shown");
        Expect(stage.CallCards.Cards.Count == 1 && card?.TitleText.StartsWith("Conversation", StringComparison.Ordinal) == true, "no conversation card over the stage");
        if (card is not null)
        {
            await CallCardPicture(v, stage, card, camera);
        }
        // parity: call-talk-conversation
        // The core is told the call is open on the camera's tile (its talk starts skip the per-minute limit).
        CameraTile? conversationTile = v.Calls.ConversationTileFor(camera);
        Say($"calls: core told the call is open on tile {conversationTile?.Id.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? "none"}");
        Expect(conversationTile is not null && conversationTile == v.Leases.Tile(camera), "opening a conversation did not tell the core (no talk-back limit during a call)");

        // parity: call-camera-audio-recognized
        Expect(engines.Count == 1 && engines[0].Source == TranscriptSource.Visitor, "the camera's audio has no recognizer");
        bool tapped = engines.Count > 0 && await Until(() => engines[0].SamplesFed > 16_000, 10);
        Say($"calls: camera audio fed to the recognizer: {engines.FirstOrDefault()?.SamplesFed ?? 0} samples");
        Expect(tapped, "the camera's tapped audio did not reach the recognizer");
        menu = stage.BuildTileMenu(menuTile)!;
        Expect(Texts(menu.Items).Contains("End Conversation"), "the tile menu does not offer End Conversation during a call");

        // parity: call-talk-permission-prompt
        // 2. Without the permission Talk asks first; Cancel sends nothing, the call stays open, and the setting stays off.
        string cameraName = v.CameraName(camera);
        prompter.Answers.Enqueue(false);
        await v.Calls.StartTalkingAsync(camera);
        Say($"calls: talk without permission: asked \"{prompter.Asked.FirstOrDefault()?.Title}\", {call!.Talk.Text}");
        Expect(prompter.Asked.Count == 1 && prompter.Asked[0].Title == $"Allow talk-back for {cameraName}?", "Talk without the permission did not ask to allow talk-back for the camera");
        Expect(call.Talk.Current == TalkState.Phase.Idle && call.Talk.Outstanding == 0 && mic.Starts == 0, "a canceled prompt started talk-back (or opened the microphone)");
        Expect(!v.Settings.TalkAllowed(camera) && v.Calls.CallFor(camera) is not null, "Cancel turned talk-back on or ended the call");
        // A typed reply asks too; Cancel keeps the text (not accepted) and sends nothing.
        prompter.Answers.Enqueue(false);
        bool replyAccepted = await v.Calls.SendReplyAsync("Not yet", camera);
        Expect(!replyAccepted && prompter.Asked.Count == 2, "a typed reply without the permission did not ask and keep its text");
        Expect(voice.Rendered.Count == 0 && call.Talk.Outstanding == 0, "a canceled typed reply was rendered or sent");

        // parity: call-talk-allowed
        // 3. Allow saves the setting and starts the talk without pressing again; the core reports the audio it sent.
        prompter.Answers.Enqueue(true);
        v.Calls.StartTalking(camera);
        Expect(v.Settings.TalkAllowed(camera), "Allow did not keep the permission");
        bool active = await Until(() => call.Talk.IsActive, 10);
        Expect(active && mic.Running, "Talk did not become active with the microphone running");
        // A second of microphone blocks at once: the core sends them at the mock's speedup (MockSpeedup), so the step
        // checks the wiring, not the real-time pace (the core's talk tests prove that on a simulated clock).
        for (int i = 0; i < 10; i++)
        {
            mic.Speak(1600);
        }
        bool youListening = engines.Count > 1 && engines[1].Source == TranscriptSource.You;
        // What the call took from the (fake) microphone's echo cancellation, checked below.
        (EchoCancellation? State, string? Note, bool? Gate) echoOn = (v.Calls.EchoFor(camera), v.Calls.NoteFor(camera), v.Calls.EchoGateCancelled(camera));
        v.Calls.StopTalking(camera);
        bool stopped = await Until(() => call.Talk.Current == TalkState.Phase.Stopped, 10);
        Say($"calls: talk with permission: active {active}, then {call.Talk.Text}, {call.Talk.SentMs} ms sent");
        Expect(stopped && !mic.Running && call.Talk.SentMs >= 500, "Talk did not send the microphone's audio");
        Expect(youListening, "the microphone had no recognizer while talking");

        // parity: call-echo-cancellation
        // 3b. Echo cancellation (docs/AUDIO.md "Echo cancellation"): the talk above had the system's
        // canceller on; now a microphone without one. Real echo cannot be heard in a self-test: this
        // checks what the call shows and which caption rule its gate runs (the rules themselves are
        // EchoCancellationTests); the microphone is sent either way.
        Say($"calls: echo cancellation on: {echoOn.State}, note \"{echoOn.Note}\", cancelled gate {echoOn.Gate}");
        Expect(echoOn.State == EchoCancellation.System && echoOn.Gate == true, "the microphone's echo cancellation was not taken up by the call");
        Expect(echoOn.Note == "Echo cancellation on", "the call does not show that echo cancellation is on");
        mic.Echo = EchoCancellation.Gated("self-test: no echo canceller");
        v.Calls.StartTalking(camera);
        bool gatedTalk = await Until(() => call.Talk.IsActive && mic.Running, 10);
        (EchoCancellation? State, string? Note, bool? Gate) echoOff = (v.Calls.EchoFor(camera), v.Calls.NoteFor(camera), v.Calls.EchoGateCancelled(camera));
        for (int i = 0; i < 5; i++)
        {
            mic.Speak(1600);
        }
        v.Calls.StopTalking(camera);
        bool gatedStopped = await Until(() => call.Talk.Current == TalkState.Phase.Stopped, 10);
        mic.Echo = EchoCancellation.System;
        Say($"calls: echo cancellation off: {echoOff.State}, note \"{echoOff.Note}\", cancelled gate {echoOff.Gate}, {call.Talk.SentMs} ms sent");
        Expect(gatedTalk && gatedStopped && call.Talk.SentMs >= 200, "without echo cancellation the microphone was not sent");
        Expect(echoOff.State == EchoCancellation.Gated("self-test: no echo canceller") && echoOff.Gate == false, "without echo cancellation the caption gate did not take over");
        Expect(echoOff.Note == "No echo cancellation", "the call does not show that echo cancellation is off");

        // parity: call-typed-reply
        // 4. A typed reply: rendered by the voice (never played here), pushed in one go, ended by a draining stop.
        await v.Calls.SendReplyAsync("Hello, one moment please", camera);
        // Letting go of Talk above also drained ("finished"), so the reply must start a talk of its own first.
        bool replyStarted = call.Talk.IsTalking;
        bool replied = replyStarted && await Until(() => call.Talk is { Current: TalkState.Phase.Stopped, Reason: "finished" }, 15);
        Say($"calls: typed reply: voice rendered {string.Join("|", voice.Rendered)}, talk ended \"{call.Talk.Reason}\", {call.Talk.SentMs} ms sent");
        Expect(voice.Rendered.SequenceEqual(["Hello, one moment please"]), "the voice did not render exactly the typed text");
        Expect(replied && call.Talk.SentMs >= 300, "the typed reply was not sent and finished");

        // parity: call-caption-echo
        // The camera's microphone hears its speaker play the reply: the visitor caption that repeats it
        // is dropped; other speech is kept.
        engines[0].Say("Hello, one moment", "Hello, one moment please.");
        engines[0].Say("I have a", "I have a delivery for the house");
        bool echoChecked = await Until(() => call.Transcript.Finals.Any(l => l.Text.StartsWith("I have a delivery", StringComparison.Ordinal)), 5);
        Say($"calls: caption echo: {string.Join("|", call.Transcript.Finals.Select(l => l.Text))}, dropped {call.Echo.Dropped}");
        Expect(echoChecked, "other visitor speech was not captioned");
        Expect(!call.Transcript.Finals.Any(l => l.Text.StartsWith("Hello, one moment", StringComparison.Ordinal)), "the reply came back as a visitor caption (echo not dropped)");
        Expect(call.Echo.Dropped == 2, $"expected two dropped echo captions, saw {call.Echo.Dropped}");

        // parity: call-captions
        // 5. Captions reach the call and its card.
        int finalsBefore = call.Transcript.Finals.Count;
        engines[0].Say("hello th", "Hello there.");
        bool captioned = await Until(() => call.Transcript.Finals.Count == finalsBefore + 1, 5);
        stage.CallCards.Sync();
        Say($"calls: captions \"{card?.CaptionText}\"");
        Expect(captioned && card?.CaptionText.Contains("Visitor: Hello there.", StringComparison.Ordinal) == true, "captions did not reach the card");

        if (card is not null)
        {
            await CallOverTheStage(v, stage, card, camera, leasesBefore);
        }

        if (card is not null)
        {
            await CallInsetDrag(v, stage, card, camera);
        }

        // parity: call-press-joins-conversation
        // 6. A doorbell press joins the call.
        v.Apply(Alarm(device, 0, "visitor", true));
        Expect(call.Presses == 1, "a press during the conversation did not join it");
        stage.CallCards.Sync();
        Expect(card?.TitleText.StartsWith("Doorbell", StringComparison.Ordinal) == true, "the card does not say Doorbell after a press");
        v.Apply(Alarm(device, 0, "visitor", false));

        // parity: call-transcript-file
        // With "Save call transcripts" on (a temporary folder here), the call's transcript is written as a
        // text file when it ends (checked after step 7); off, the next call writes none (step 8).
        string transcriptFolder = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-transcripts-{Guid.NewGuid():N}");
        string transcriptsBefore = v.Transcripts.Folder;
        v.Transcripts.Folder = transcriptFolder;
        v.Settings.SaveCallTranscripts = true;

        // parity: call-auto-close
        // 7. A call ends a minute after its last activity and gives the audio back.
        clock.Advance(59);
        v.Tick();
        Expect(v.Calls.CallFor(camera) is not null, "the call ended before a minute of silence");
        clock.Advance(2);
        v.Tick();
        stage.CallCards.Sync();
        Say($"calls: after a minute: call open {v.Calls.CallFor(camera) is not null}, cards {stage.CallCards.Cards.Count}, audible {v.IsAudible(camera)}, leases {v.Leases.LeaseCount(camera)} (were {leasesBefore})");
        Expect(v.Calls.CallFor(camera) is null && stage.CallCards.Cards.Count == 0, "the call did not end a minute after its last activity");
        Expect(!v.IsAudible(camera) && v.Leases.LeaseCount(camera) == leasesBefore && stage.CallCards.LeasesHeld == 0, "ending the call left the camera audible or a lease held");
        bool undrawn = await Until(() => app.Renderer!.Overlays.Length == 0 && stage.CallCards.Overlays.Count == 0, 2);
        Expect(undrawn, "the ended call's picture is still drawn over the grid");
        Expect(engines.All(e => e.Finished), "ending the call left a recognizer running");
        string[] transcriptNames = v.Transcripts.Names();
        string transcriptText = transcriptNames.Length == 1 ? File.ReadAllText(Path.Combine(transcriptFolder, transcriptNames[0])) : "";
        string safeName = TranscriptFiles.Sanitized(v.CameraName(camera));
        Say($"calls: transcript file {string.Join("|", transcriptNames)}, {transcriptText.Split('\n').Length} lines");
        Expect(transcriptNames.Length == 1 && transcriptNames[0].EndsWith($" {safeName}.txt", StringComparison.Ordinal), "ending a call with saving on did not write one transcript file named for the camera");
        Expect(
            transcriptText.StartsWith($"NVRView call transcript\nCamera: {v.CameraName(camera)}\n", StringComparison.Ordinal)
                && transcriptText.Contains("\nCall: doorbell, 1 press\n", StringComparison.Ordinal)
                && transcriptText.Contains(" Visitor: Hello there.\n", StringComparison.Ordinal),
            "the transcript file lacks its header or the visitor's line");
        v.Settings.SaveCallTranscripts = false;

        // parity: call-press-opens-call
        // 8. A press alone opens a silent call with Unmute; Unmute plays it; closing gives it back.
        v.Apply(Alarm(device, 0, "visitor", true));
        Call? ring = v.Calls.CallFor(camera);
        stage.CallCards.Sync();
        Expect(ring is { Origin: CallOrigin.Press, Presses: 1, ShowsUnmute: true, Audible: false } && !v.IsAudible(camera), "a press did not open a silent call with Unmute");
        Expect(stage.CallCards.Cards.FirstOrDefault()?.UnmuteShown == true, "the card does not show Unmute for a silent call");
        // parity: call-unmute
        v.Calls.Unmute(camera);
        Expect(v.IsAudible(camera) && ring is { ShowsUnmute: false }, "Unmute did not play the camera");
        // (Saving is off now: what this call transcribes is not written.)
        engines[^1].Say("hi", "Hi.");
        bool ringCaptioned = await Until(() => ring?.Transcript.Finals.Count == 1, 5);
        // parity: call-close-ends
        v.Calls.End(camera, "closed");
        Expect(v.Calls.CallFor(camera) is null && !v.IsAudible(camera), "closing the call did not end it and mute the camera");
        Expect(ringCaptioned && v.Transcripts.Names().Length == 1, "a call with saving off wrote a transcript file");

        // parity: unmute-on-ring
        // The doorbell's "Unmute on ring even when not listening": with it on, a press plays the doorbell at once
        // (no Unmute control); it is a view setting that reads back from view-settings.json.
        v.Settings.SetUnmuteOnRing(camera, true);
        v.Apply(Alarm(device, 0, "visitor", true));
        Call? loud = v.Calls.CallFor(camera);
        bool loudAudible = loud is { ShowsUnmute: false, Audible: true } && v.IsAudible(camera);
        v.Calls.End(camera, "closed");
        bool loudEnded = v.Calls.CallFor(camera) is null && !v.IsAudible(camera);
        v.Apply(Alarm(device, 0, "visitor", false));
        v.Settings.SetUnmuteOnRing(camera, false);
        v.Apply(Alarm(device, 0, "visitor", true));
        bool quietAgain = v.Calls.CallFor(camera) is { ShowsUnmute: true, Audible: false } && !v.IsAudible(camera);
        v.Calls.End(camera, "closed");
        v.Apply(Alarm(device, 0, "visitor", false));
        string unmuteFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
        bool unmuteSaved;
        try
        {
            new ViewSettings(unmuteFile).SetUnmuteOnRing(camera, true);
            var reread = new ViewSettings(unmuteFile);
            unmuteSaved = reread.UnmuteOnRing(camera) && !reread.UnmuteOnRing(new CameraRef(camera.Device, camera.Channel + 1));
        }
        finally
        {
            File.Delete(unmuteFile);
        }
        Say($"calls: unmute on ring: audible at once {loudAudible}, ended {loudEnded}, silent again when off {quietAgain}, setting reads back {unmuteSaved}");
        Expect(loudAudible, "with Unmute on ring on, a press did not play the doorbell at once");
        Expect(loudEnded, "ending the unmuted call left the doorbell audible");
        Expect(quietAgain, "with Unmute on ring off again, a press did not open a silent call");
        Expect(unmuteSaved, "the unmute-on-ring setting did not read back from view-settings.json");
        try
        {
            Directory.Delete(transcriptFolder, recursive: true);
        }
        catch (IOException)
        {
            // Left for the system's temporary-file cleanup.
        }
        v.Transcripts.Folder = transcriptsBefore;

        await CallWindowSteps(v, stage, camera, device, clock, mic);
        if (Runs("capture"))
        {
            await CallRecordingEndsWithCall(v, camera);
        }

        // The alert insets: swap with the stage, smoke and CO, the return inset, order, grid, Entrance.
        await AlertInsetSteps(v, stage, device, camera, clock);

        // parity: call-permission-revoke
        // Revoking the permission is the user's too.
        v.SetTalkAllowed(camera, false);
        Expect(!v.Settings.TalkAllowed(camera), "the permission was not withdrawn");
        v.UseClock(new SystemViewerClock());

        await RealCallEngines();
    }

    private static bool NearFrame(WindowRect a, WindowRect b) =>
        Math.Abs(a.X - b.X) <= 3 && Math.Abs(a.Y - b.Y) <= 3 && Math.Abs(a.Width - b.Width) <= 3 && Math.Abs(a.Height - b.Height) <= 3;

    // Calls in their own windows (docs/UX.md "A call in its own window"), against the doorbell channel:
    // a press shown as a card in two viewer windows pops out into a call window that draws exactly the
    // doorbell's shared tile; talk, "Keep talking", and the unsent reply survive popping in and out; a
    // viewer window opened meanwhile gets its card back on Pop In; closing the window or Esc ends the
    // call, and it never ends by inactivity while popped out; the window's frame is remembered or falls
    // back to the top right; the setting and the doorbell's own choice route a ring and a conversation.
    // The windows stay behind the others and never float or take the focus in a test run.
    private async Task CallWindowSteps(Viewer v, StageView stage, CameraRef camera, DeviceEntry device, ManualViewerClock clock, FakeMicrophone mic)
    {
        CallWindows cws = v.CallWindows!;
        CameraRef other = new(device.Key, 1);
        ViewerWindow second = app.OpenWindow(new WindowContent.Scratch("Second", LayoutNode.Leaf(other)));
        var opened = new List<ViewerWindow> { second };
        StageView[] stages = [stage, second.Stage];
        int baseline = v.Leases.LeaseCount(camera);
        StreamTier baselineTier = v.Planner.WantedTier(camera);
        WindowRect? frameBefore = cws.RememberedFrame;
        CallWindowPlacement.Mode modeBefore = cws.PlacementMode;
        bool pinBefore = cws.Pinned;
        try
        {
            // parity: call-popout-open
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            Call? call = v.Calls.CallFor(camera);
            bool bothShow = await Until(() => stages.All(s => s.CallCards.Cards.Count == 1 && s.CallCards.LeasesHeld == 1), 3);
            if (call is null || !bothShow || v.Leases.Tile(camera) is not { } tile)
            {
                _problems.Add("a press did not open the call's card in both viewer windows");
                return;
            }
            int insetsBefore = stages.Sum(s => s.CallCards.Cards.Count);
            int leasesBefore = v.Leases.LeaseCount(camera);
            bool popButtons = stages.All(s => s.CallCards.Cards[0] is { PopShown: true, PopTip: "Pop Out Call" });
            long starts = mic.Starts;
            int presses = call.Presses, finals = call.Transcript.Finals.Count;
            cws.PopOut(camera);
            if (cws.WindowFor(camera) is not { } cw)
            {
                _problems.Add($"Pop Out opened no call window: {cws.LastError}");
                return;
            }
            bool gone = stages.All(s => s.CallCards.Cards.Count == 0 && s.CallCards.LeasesHeld == 0);
            // The window's picture is a regular tile: the one tile of its own stage (no overlay), drawn from the shared tile.
            bool drawn = await Until(() => cw.Stage.Tiles.Length == 1 && cw.Renderer.Layout.Tiles.Length == 1 && cw.Renderer.Layout.Tiles[0] == cw.Tile && cw.Tile.Shown > 2 && cw.Tile.PictureWidth > 0, 15);
            bool exact = drawn && cw.Renderer.Overlays.Length == 0 && cw.Tile.Shared == tile && v.Tiles.Length == v.Leases.Cameras.Count;
            bool undrawn = await Until(() => stages.All(s => s.CallCards.Overlays.Count == 0 && s.Renderer.Overlays.Length == 0), 3);
            var engine = v.Calls.VisitorEngine(camera) as FakeTranscriber;
            long fed = engine?.SamplesFed ?? 0;
            bool tapped = engine is not null && await Until(() => engine.SamplesFed > fed + 8000, 8);
            bool same = v.Calls.CallFor(camera) == call && call.Presentation == CallPresentation.Window && call.Presses == presses && call.Transcript.Finals.Count == finals;
            int leases = v.Leases.LeaseCount(camera);
            int wantedLeases = leasesBefore - insetsBefore + 1;
            Say($"calls: pop out: cards {insetsBefore} -> gone {gone} (picture undrawn {undrawn}), window draws the doorbell tile {exact} ({cw.Tile.Shown} pictures), " +
                $"same call {same}, tapped {tapped}, leases {leasesBefore} -> {leases}, main stream {cw.AsksForMainStream}");
            Expect(popButtons, "a call's card did not offer Pop Out Call in its header");
            Expect(gone && undrawn, "Pop Out left the call's card (or its picture or lease) in a viewer window");
            Expect(exact, "the call window does not draw exactly the doorbell's shared tile");
            Expect(same, "Pop Out changed the call");
            Expect(tapped, "Pop Out stopped the doorbell's audio tap");
            Expect(mic.Starts == starts && call.Talk.Outstanding == 0, "Pop Out started talk-back");
            Expect(leases == wantedLeases && v.Leases.Tile(camera) == tile, $"Pop Out leases: {leases}, expected {wantedLeases} on the same tile");
            Expect(cws.LastOpenActivated == true, "an explicit Pop Out did not ask for the focus");
            if (drawn)
            {
                await CheckPictureFrame("call window", cw.Tile, cw.Renderer);
                // The window joins the device-loss recovery with the viewer windows: its swap chain and picture come back.
                int recoveries = cw.Renderer.Recoveries;
                long shown = cw.Tile.Shown;
                cw.Renderer.SimulateDeviceLoss();
                bool recovered = await Until(() => cw.Renderer.Recoveries > recoveries, 10);
                bool back = recovered && await Until(() => cw.Tile.Shown > shown + 5 && cw.Tile.PictureWidth > 0, 15);
                Say($"calls: call window after a device loss: recovered {recovered}, pictures {shown} -> {cw.Tile.Shown}");
                Expect(back, "the call window's picture did not come back after a device loss");
                if (back)
                {
                    await CheckPictureFrame("call window after device loss", cw.Tile, cw.Renderer);
                }
                await CallWindowTile(v, cw, camera, clock);
            }

            // parity: call-popout-talk
            // "Keep talking" and an unsent reply belong to the call: popping in and out stops nothing and starts nothing, and any card built for it shows them.
            v.SetTalkAllowed(camera, true);
            v.Calls.SetLatched(camera, true);
            v.Calls.SetDraft(camera, "Be right there");
            bool talking = await Until(() => call.Talk.IsActive && mic.Running, 10);
            long talkStarts = mic.Starts;
            cws.PopIn(camera);
            bool insetsShow = stages.All(s => s.CallCards.Cards.FirstOrDefault() is { KeepTalkingShown: true } c && c.ReplyText == "Be right there");
            cws.PopOut(camera);
            CallWindow? cw2 = cws.WindowFor(camera);
            bool windowShows = cw2 is { Card: { KeepTalkingShown: true } card2 } && card2.ReplyText == "Be right there";
            await Task.Delay(300); // proves that nothing happens: the talk is not stopped or restarted after the pops
            bool kept = call.Talk.IsActive && mic.Running && mic.Starts == talkStarts;
            Say($"calls: pop in and out while talking: talking {talking}, talk kept {kept}, latch {call.Latched}, draft \"{call.Draft}\", cards show them {insetsShow}, window shows them {windowShows}");
            Expect(talking, "Keep talking did not start the talk before the pop");
            Expect(kept, "popping in and out stopped or restarted talk-back");
            Expect(call.Latched && call.Draft == "Be right there", "popping lost Keep talking or the draft");
            Expect(insetsShow, "the cards did not show Keep talking and the draft after Pop In");
            Expect(windowShows, "the call window did not show Keep talking and the draft");
            v.Calls.SetLatched(camera, false);
            Expect(await Until(() => !call.Talk.IsTalking, 10), "turning Keep talking off did not end the talk");
            v.SetTalkAllowed(camera, false);
            v.Calls.SetDraft(camera, "");
            if (cw2 is null)
            {
                return;
            }

            // parity: call-popin-new-window
            // A viewer window opened while the call is popped out has no card for it, and shows it after Pop In like the others.
            ViewerWindow third = app.OpenWindow(new WindowContent.Scratch("Third", LayoutNode.Leaf(other)));
            opened.Add(third);
            await Task.Delay(300); // proves that nothing happens: the new window does not build a card for the popped-out call
            bool hidden = third.Stage.CallCards.Cards.Count == 0 && third.Stage.CallCards.LeasesHeld == 0 && cws.WindowFor(camera) is not null;
            Say($"calls: new window while popped out: no card {hidden}, leases {v.Leases.LeaseCount(camera)}");
            Expect(hidden, "a viewer window opened while the call was popped out shows its card");
            stages = [.. stages, third.Stage];

            // parity: call-popin
            // Pop In: the window closes, every viewer window shows the card again with a fresh minute, and the leases follow.
            CallWindow popped = cws.WindowFor(camera)!;
            WindowRect size = popped.CurrentFrame;
            popped.Place(new WindowRect(size.X, size.Y, 1100, 800));
            bool wantsMain = await Until(() => popped.AsksForMainStream && v.Planner.WantedTier(camera) == StreamTier.Main, 8);
            int leasesPopped = v.Leases.LeaseCount(camera);
            clock.Advance(30);
            double popInAt = clock.Now;
            cws.PopIn(camera);
            bool back2 = stages.All(s => s.CallCards.Cards.Count == 1 && s.CallCards.Cards[0].Mode == CallCardMode.Inset && s.CallCards.Cards[0].PopTip == "Pop Out Call");
            bool minute = call.LastActivity >= popInAt;
            bool closed = cws.WindowFor(camera) is null && popped.IsReleased;
            int leasesIn = v.Leases.LeaseCount(camera);
            StreamTier demand = v.Planner.WantedTier(camera);
            Say($"calls: pop in: window closed {closed}, cards back {back2}, fresh minute {minute}, leases {leasesPopped} -> {leasesIn}, window asked for main {wantsMain}, now wants {demand}");
            Expect(wantsMain, "a large call window did not ask for the main stream");
            Expect(closed, "Pop In left the call window open");
            Expect(back2, "Pop In did not bring the card back in every viewer window");
            Expect(minute, "Pop In did not give the call a fresh minute");
            Expect(leasesIn == leasesPopped - 1 + stages.Length && call.Presentation == CallPresentation.Inset, $"Pop In leases: {leasesIn}, expected {leasesPopped - 1 + stages.Length}");
            Expect(demand == baselineTier, "the closed call window's stream demand stayed");

            // parity: call-popout-end
            // Closing the call window and Esc end the call, and the window and the viewer windows' cards go with it; a call in its own window never ends by inactivity.
            void Ended(string how)
            {
                bool clean = v.Calls.CallFor(camera) is null && cws.WindowFor(camera) is null && stages.All(s => s.CallCards.Cards.Count == 0)
                    && v.Leases.LeaseCount(camera) == baseline;
                Say($"calls: call window {how}: call ended and window closed {clean}, leases {v.Leases.LeaseCount(camera)} (baseline {baseline})");
                Expect(clean, $"{how} did not end the call, close its window and let its cards and leases go");
            }
            cws.PopOut(camera);
            CallWindow? closing = cws.WindowFor(camera);
            closing?.Window.Close();
            await Until(() => v.Calls.CallFor(camera) is null, 5);
            Ended("closed");
            // Esc while the reply field is not being edited.
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            bool escHandled = cws.WindowFor(camera)?.HandleKey(Windows.System.VirtualKey.Escape) == true;
            await Until(() => v.Calls.CallFor(camera) is null, 5);
            Expect(escHandled, "Esc in the call window was not handled");
            Ended("Esc");
            // Ten minutes without activity leave a popped-out call open.
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            clock.Advance(10 * Call.IdleTimeout);
            v.Tick();
            bool stayed = cws.WindowFor(camera) is not null && v.Calls.CallFor(camera) is not null;
            Say($"calls: call window after 10 min without activity: still open {stayed}");
            Expect(stayed, "a call in its own window ended by itself without activity");
            // Pop In restarts the minute.
            cws.PopIn(camera);
            clock.Advance(Call.IdleTimeout - 1);
            v.Tick();
            bool fresh = v.Calls.CallFor(camera) is not null;
            clock.Advance(2);
            v.Tick();
            Say($"calls: popped in: open after 59 s {fresh}, ended after 61 s {v.Calls.CallFor(camera) is null}");
            Expect(fresh, "Pop In did not give the call a fresh minute");
            Ended("expiry after Pop In");

            // parity: call-window-frame
            // The remembered frame is reused and follows the user's moves; one whose screen is gone falls back to the top right of
            // the active viewer window's screen; "Top right of the active screen" ignores it; the pin is remembered.
            WindowRect visible = cws.ActiveWorkArea() is { } active ? active : new WindowRect(0, 0, 1024, 728);
            const double W = 520, H = 440;
            WindowRect rememberedFrame = new(visible.X + 40, visible.Y + 60, W, H);
            cws.PlacementMode = CallWindowPlacement.Mode.Remembered;
            cws.RememberedFrame = rememberedFrame;
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? reused = cws.WindowFor(camera)?.CurrentFrame;
            if (cws.WindowFor(camera) is { } mover)
            {
                mover.AppWindow.Move(new Windows.Graphics.PointInt32((int)rememberedFrame.X + 30, (int)rememberedFrame.Y + 20));
            }
            var movedTo = new WindowRect(rememberedFrame.X + 30, rememberedFrame.Y + 20, W, H);
            bool follows = await Until(() => cws.RememberedFrame is { } f && NearFrame(f, movedTo), 5);
            v.Calls.End(camera, "closed");
            // A frame on a screen that is gone.
            cws.RememberedFrame = new WindowRect(-100_000, -100_000, W, H);
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? fallback = cws.WindowFor(camera)?.CurrentFrame;
            v.Calls.End(camera, "closed");
            var topRight = new WindowRect(visible.X + visible.Width - CallWindowPlacement.Margin - W, visible.Y + CallWindowPlacement.Margin, W, H);
            // The other setting ignores a frame that is still on a screen.
            cws.RememberedFrame = rememberedFrame;
            cws.PlacementMode = CallWindowPlacement.Mode.ActiveScreen;
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? atTopRight = cws.WindowFor(camera)?.CurrentFrame;
            v.Calls.End(camera, "closed");
            cws.PlacementMode = CallWindowPlacement.Mode.Remembered;
            // The pin is remembered (a test run never floats the window, so the native flag is not read).
            cws.Pinned = false;
            bool pinSaved = !v.Settings.CallWindowOnTop;
            cws.Pinned = true;
            pinSaved &= v.Settings.CallWindowOnTop;
            // The frame, the placement, and the pin are kept in view-settings.json.
            string settingsFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
            try
            {
                var written = new ViewSettings(settingsFile) { CallWindowFrame = rememberedFrame, CallWindowPlacementMode = CallWindowPlacement.Mode.ActiveScreen, CallWindowOnTop = false };
                var read = new ViewSettings(settingsFile);
                bool persisted = read.CallWindowFrame == rememberedFrame && read.CallWindowPlacementMode == CallWindowPlacement.Mode.ActiveScreen && !read.CallWindowOnTop && written.CallWindowOnTop == false;
                bool reusedOk = reused is { } r1 && NearFrame(r1, rememberedFrame);
                bool fallbackOk = fallback is { } r2 && NearFrame(r2, topRight);
                bool settingOk = atTopRight is { } r3 && NearFrame(r3, topRight);
                Say($"calls: call window frame: remembered reused {reusedOk}, move remembered {follows}, gone screen -> top right {fallbackOk}, " +
                    $"top right setting {settingOk}, pin saved {pinSaved}, settings file reads back {persisted}");
                Expect(reusedOk, $"the call window did not open at the remembered frame: {reused}");
                Expect(follows, $"moving the call window did not remember its frame: {cws.RememberedFrame}");
                Expect(fallbackOk, $"a remembered frame off every screen did not fall back to the top right: {fallback} (wanted {topRight})");
                Expect(settingOk, $"Top right of the active screen did not ignore the remembered frame: {atTopRight} (wanted {topRight})");
                Expect(pinSaved, "the pin was not remembered");
                Expect(persisted, "the call window's frame, placement, and pin did not read back from view-settings.json");
            }
            finally
            {
                File.Delete(settingsFile);
            }

            // parity: call-default-ring
            // "Open calls in their own window": a ring opens the call in a call window that is not brought forward, the viewer windows keep no
            // card for it, a press that joins keeps it there, and the doorbell's own choice (Inset) beats the setting.
            v.Settings.CallsOpenInWindow = true;
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool ringWindow = await Until(() => cws.WindowFor(camera) is not null, 5);
            Call? ringCall = v.Calls.CallFor(camera);
            CallWindow? ringWin = cws.WindowFor(camera);
            bool quiet = cws.LastOpenActivated == false;
            bool noCards = stages.All(s => s.CallCards.Cards.Count == 0 && s.CallCards.LeasesHeld == 0);
            v.Settings.CallsOpenInWindow = false;
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool joined = ringCall?.Presses == 2 && ringCall.Presentation == CallPresentation.Window && cws.WindowFor(camera) == ringWin && stages.All(s => s.CallCards.Cards.Count == 0);
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = true;
            v.Settings.SetCallPresentation(camera, CallPresentation.Inset);
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool inset = await Until(() => stages.All(s => s.CallCards.Cards.Count == 1), 3) && v.Calls.CallFor(camera)?.Presentation == CallPresentation.Inset && cws.WindowFor(camera) is null;
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = false;
            v.Settings.SetCallPresentation(camera, null);
            Say($"calls: ring with calls in windows: window {ringWindow && ringCall?.Presentation == CallPresentation.Window}, opened without focus {quiet}, no cards {noCards}, " +
                $"a joining press kept it {joined}; the doorbell set to Inset: cards in every window {inset}");
            Expect(ringWindow && ringCall?.Presentation == CallPresentation.Window, "with the setting on, a ring did not open a call window");
            Expect(quiet, "a call window opened by a ring asked for the focus");
            Expect(noCards, "a ring into a call window left a card in a viewer window");
            Expect(joined, "a press joining a call in its window moved it");
            Expect(inset, "the doorbell's Inset choice did not win over Open calls in their own window");

            // parity: call-default-conversation
            // The doorbell's Window choice opens Start Conversation in a call window (brought forward: the user asked for it) with the
            // setting off; Default again leaves the choice out of the file.
            v.Settings.SetCallPresentation(camera, CallPresentation.Window);
            v.Calls.StartConversation(camera);
            Call? conversation = v.Calls.CallFor(camera);
            bool conversationWindow = conversation is { Presentation: CallPresentation.Window, Origin: CallOrigin.Conversation } && cws.WindowFor(camera) is not null;
            bool activates = cws.LastOpenActivated == true;
            bool noCard = stages.All(s => s.CallCards.Cards.Count == 0);
            v.Calls.End(camera, "closed");
            v.Settings.SetCallPresentation(camera, null);
            bool cleared = v.Settings.CallPresentationFor(camera) is null;
            string choiceFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
            bool readsBack;
            try
            {
                var choices = new ViewSettings(choiceFile) { CallsOpenInWindow = true };
                choices.SetCallPresentation(camera, CallPresentation.Window);
                var reread = new ViewSettings(choiceFile);
                readsBack = reread.CallsOpenInWindow && reread.CallPresentationFor(camera) == CallPresentation.Window;
                choices.SetCallPresentation(camera, null);
                readsBack &= new ViewSettings(choiceFile).CallPresentationFor(camera) is null;
            }
            finally
            {
                File.Delete(choiceFile);
            }
            Say($"calls: conversation with the doorbell set to Window: call window {conversationWindow}, brought forward {activates}, no card {noCard}, Default clears the choice {cleared}, saved choices read back {readsBack}");
            Expect(conversationWindow, "the doorbell's Window choice did not open Start Conversation in a call window");
            Expect(activates, "a call window opened by Start Conversation was not brought forward");
            Expect(noCard, "Start Conversation into a call window left a card in a viewer window");
            Expect(cleared, "choosing Default did not clear the doorbell's call choice");
            Expect(readsBack, "the setting and the doorbell's call choice did not save and read back");
        }
        finally
        {
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = false;
            v.Settings.SetCallPresentation(camera, null);
            v.SetTalkAllowed(camera, false);
            cws.RememberedFrame = frameBefore;
            cws.PlacementMode = modeBefore;
            cws.Pinned = pinBefore;
            foreach (ViewerWindow w in opened)
            {
                w.Window.Close();
            }
            await Until(() => app.Windows.Count == 1, 5);
        }
    }

    // The call window's picture is a regular tile (docs/CAPTURE.md "The call window is a regular tile"): a pinch zooms it, the
    // zoom map shows, the renderer samples the zoomed region, the zoomed tile asks for the main stream, and the header's Reset
    // Zoom brings it back; its menu is the header model's (Pop In Call, End Conversation, Record Call) with no layout items, and
    // focus and split do nothing there.
    private async Task CallWindowTile(Viewer v, CallWindow cw, CameraRef camera, ManualViewerClock clock)
    {
        StageView stage = cw.Stage;
        StageTile tile = cw.Tile;
        TileChrome? chrome = stage.ChromeFor(tile);
        if (!stage.FixedTile || chrome is null)
        {
            _problems.Add("the call window's picture is not a stage in single fixed tile mode");
            return;
        }
        // parity: call-window-tile-zoom
        stage.Hover(tile);
        stage.PinchDigital(tile, tile.X + tile.Width / 2, tile.Y + tile.Height / 2, 2);
        stage.Tick();
        bool zoomed = Math.Abs(tile.Zoom - 2) < 0.01;
        UnitRect seen = tile.DigitalZoom.Visible(tile.ZoomBase);
        bool cropped = seen.Width < 0.9 || seen.Height < 0.9;
        bool wantsMain = v.Planner.WantedTier(camera) == StreamTier.Main;
        // Switching to the main stream takes the picture away for a while (the header then shows only the title and the more button),
        // so the header is read before the switch happens and the zoom is reset, which sends the tile back to the sub-stream.
        // At the window's real size the header may keep Reset Zoom in its "..." menu: it must be offered either way.
        await Until(() => chrome.MapShown && (chrome.ResetShown || chrome.HeaderLayout?.MoreMenu.Any(e => e.Item == TileHeaderItem.ResetZoom) == true), 5);
        bool map = chrome.MapShown;
        bool resetButton = chrome.ResetShown;
        bool resetShown = resetButton || chrome.HeaderLayout?.MoreMenu.Any(e => e.Item == TileHeaderItem.ResetZoom) == true;
        wantsMain |= await Until(() => v.Planner.WantedTier(camera) == StreamTier.Main, 8);
        if (resetButton)
        {
            chrome.PressReset();
        }
        else
        {
            stage.PerformHeaderItem(tile, TileHeaderItem.ResetZoom, null);
        }
        bool back = await Until(() => tile.Zoom <= 1.0001 && !chrome.ResetShown && !chrome.MapShown, 5);
        Say($"call window tile: status \"{stage.StatusText(tile)}\" ({tile.Shared?.Video.Status}); pinch to {tile.Zoom:0.00}x then reset, zoom map {map}, source cropped {cropped} ({seen.Width:0.00}x{seen.Height:0.00}), main stream {wantsMain}, header Reset Zoom shown {resetShown} and back to 1x {back}");
        Expect(zoomed, "a pinch did not zoom the call window's tile");
        Expect(map, "the call window's zoomed tile showed no zoom map");
        Expect(cropped, "the call window's renderer did not sample the zoomed region");
        Expect(wantsMain, "the zoomed call window tile did not ask for the main stream");
        Expect(resetShown && back, "the call window's header did not offer Reset Zoom or did not reset");

        // parity: call-window-tile-menu
        await Until(() => stage.HeaderTileOf(tile) is { Picture: true }, 30);
        List<string> menu = Texts(stage.BuildTileMenu(tile)!.Items);
        List<string> want = stage.HeaderTileOf(tile) is { } header ? [.. TileHeaderLayout.Menu(header).Select(e => e.Label)] : [];
        bool callItems = new[] { "Pop In Call", "End Conversation", "Record Call", "Save Picture" }.All(menu.Contains);
        string[] layoutItems = [.. new[] { "Focus", "Split Side by Side", "Remove Tile", "Clear Tile", "Show Camera", "Record Main Stream" }.Where(menu.Contains)];
        stage.ToggleFocus(tile);
        stage.Split(SplitAxis.Row);
        bool fixedStill = !stage.IsFocused && stage.State.Tree.LeafCount == 1 && stage.Selected == tile;
        IReadOnlyList<TileHeaderItem> shownButtons = chrome.ShownButtons;
        Say($"call window tile menu: the model's {want.Count} items {menu.Take(want.Count).SequenceEqual(want)}, call items {callItems}, layout items [{string.Join(", ", layoutItems)}], " +
            $"focus and split ignored {fixedStill}, header [{string.Join(", ", shownButtons)}]");
        Expect(menu.SequenceEqual(want) && callItems, $"the call window tile's menu is not the header model's call menu: {string.Join(" / ", menu)}");
        Expect(layoutItems.Length == 0, $"the call window tile's menu offers {string.Join(", ", layoutItems)}");
        Expect(fixedStill, "focus or split changed the call window's single tile");
        // The shared model decides (the speaker never hides before Save Picture; Quality may sit in "..."), and both stay reachable.
        IReadOnlyList<TileHeaderMenuEntry> overflow = chrome.HeaderLayout?.MoreMenu ?? [];
        bool Reachable(TileHeaderItem i) => shownButtons.Contains(i) || overflow.Any(e => e.Item == i);
        Expect(Reachable(TileHeaderItem.Speaker) && Reachable(TileHeaderItem.Quality), "the call window's header has no speaker or quality control, in the header or its \"...\" menu");
        Expect(!shownButtons.Contains(TileHeaderItem.SavePicture) || shownButtons.Contains(TileHeaderItem.Speaker), "the call window's header hides the speaker while Save Picture shows");
        if (Runs("capture"))
        {
            await CallRecordButton(v, cw, camera, tile);
        }
    }

    // The call card's picture: one more tile of this window's grid, drawn last from the camera's
    // shared tile (no second decoder), in the card's hole; it comes back after a device loss; the
    // cards stack from the Settings corner, else opposite the indicators.
    private async Task CallCardPicture(Viewer v, StageView stage, CallCard card, CameraRef camera)
    {
        StageRenderer r = app.Renderer!;
        // parity: alert-inset-in-grid
        bool drawn = await Until(() => card.Placed && stage.CallCards.Overlays.Any(o => o.Tile == card.Tile) && card.Tile.Shown > 2 && card.Tile.PictureWidth > 0, 15);
        StageTile? grid = Tiles.FirstOrDefault(t => t.Camera == camera);
        Say($"calls: card picture drawn {drawn}, {card.Tile.Shown} pictures, tile {card.Tile.Id} (grid tile {grid?.Id}), frame {card.Frame}, " +
            $"hole {stage.CallCards.Overlays.FirstOrDefault()?.Rect}, core tiles {v.Tiles.Length} for {v.Leases.Cameras.Count} cameras");
        Expect(drawn, "the call card's picture was not drawn");
        Expect(card.Tile.Id != 0 && (grid is null || grid.Id == card.Tile.Id) && v.Tiles.Length == v.Leases.Cameras.Count, "the call card does not draw the camera's shared tile");
        await CheckPictureFrame("call card", card.Tile, r);

        // parity: call-inset-device-loss
        long shown = card.Tile.Shown;
        int recoveries = r.Recoveries;
        r.SimulateDeviceLoss();
        bool recovered = await Until(() => r.Recoveries > recoveries, 10);
        bool back = recovered && await Until(() => card.Tile.Shown > shown + 5 && card.Tile.PictureWidth > 0, 15);
        Say($"calls: card picture after a device loss: recovered {recovered}, pictures {shown} -> {card.Tile.Shown}");
        Expect(back, "the call card's picture did not come back after a device loss");
        if (back)
        {
            await CheckPictureFrame("call card after device loss", card.Tile, r);
        }

        // parity: call-inset-corner
        double w = stage.ActualWidth, h = stage.ActualHeight;
        ChromeCorner? choice = v.Settings.InsetCornerChoice;
        v.Settings.InsetCornerChoice = ChromeCorner.TopLeft;
        stage.CallCards.UpdateLayout();
        InsetFrame topLeft = card.Frame;
        bool holeMoved = await Until(() => stage.CallCards.Overlays.FirstOrDefault()?.Rect is { } hole && hole.Y < topLeft.Bottom && hole.X < topLeft.Right, 3);
        v.Settings.InsetCornerChoice = null;
        stage.CallCards.UpdateLayout();
        InsetFrame opposite = card.Frame;
        ChromeCorner expected = v.Settings.IndicatorCorner.Opposite();
        Say($"calls: card at {topLeft} with the top left corner chosen (hole followed {holeMoved}); at {opposite} opposite the indicators ({expected}) in {w:0}x{h:0}");
        Expect(Near(topLeft.X, InsetGeometry.Margin) && Near(topLeft.Y, InsetGeometry.Margin), "the call card did not move to the corner chosen in Settings");
        Expect(holeMoved, "the card's picture did not follow the card to its corner");
        Expect(Near(expected.IsLeft() ? opposite.X : w - opposite.Right, InsetGeometry.Margin) &&
            Near(expected.IsTop() ? opposite.Y : h - opposite.Bottom, InsetGeometry.Margin), "the call card is not opposite the indicators by default");
        v.Settings.InsetCornerChoice = choice;
    }

    // ---- moving the call insets (docs/UX.md "Alert insets", "Moving them") ----
    // The pointer is not injected: the steps drive the card's own DragPress/DragMove/DragRelease,
    // which its pointer handlers call with the pointer's place in the stack.

    private static (double X, double Y) MeasuredAnchor(InsetFrame column, double w, double h)
    {
        double m = InsetGeometry.Margin;
        double fx = w - 2 * m - column.Width, fy = h - 2 * m - column.Height;
        return (fx > 0 ? (column.X - m) / fx : 0, fy > 0 ? (column.Y - m) / fy : 0);
    }

    private static bool NearAnchor((double X, double Y) a, InsetAnchor b) => Math.Abs(a.X - b.X) < 0.01 && Math.Abs(a.Y - b.Y) < 0.01;

    // A drag of the card by (dx, dy) from a point on its header or picture, in steps.
    private static void DragCard(CallCard card, double fromX, double fromY, double dx, double dy, bool onHeader)
    {
        card.DragPress(new Windows.Foundation.Point(fromX, fromY), onHeader);
        const int steps = 5;
        for (int i = 1; i <= steps; i++)
        {
            card.DragMove(new Windows.Foundation.Point(fromX + dx * i / steps, fromY + dy * i / steps));
        }
        card.DragRelease();
    }

    // Moves the column so its top left lands on (x, y), grabbing the card's header.
    private static void DragColumnTo(CallStack cards, CallCard grab, double x, double y)
    {
        InsetFrame start = cards.Column!.Value;
        InsetFrame f = grab.Frame;
        DragCard(grab, f.X + 40, f.Y + 8, x - start.X, y - start.Y, onHeader: true);
        cards.UpdateLayout();
    }

    private async Task CallInsetDrag(Viewer v, StageView stage, CallCard card, CameraRef camera)
    {
        CallStack cards = stage.CallCards;
        ViewerWindow window = app.Windows.First(w => w.Stage == stage);
        cards.UpdateLayout();
        double m = InsetGeometry.Margin;
        int changes = 0;
        void Count() => changes++;
        cards.AnchorChanged += Count;
        try
        {
            // parity: inset-drag-snap
            // The header dragged to 10 DIPs from the right edge snaps there and 40% down does not; a
            // small move on the header is no drag and moves nothing (a short move on the picture is a click: it swaps,
            // alert-inset-swap); the picture past the slop drags.
            double w = cards.ActualWidth, h = cards.ActualHeight;
            InsetFrame column = cards.Column ?? default;
            bool atDefault = cards.Anchor is null && NearAnchor(MeasuredAnchor(column, w, h), InsetAnchor.FromCorner(v.Settings.InsetCorner));
            InsetFrame f0 = card.Frame;
            DragCard(card, f0.X + 60, f0.Y + 10, 1, 1, onHeader: true);
            bool clickMoved = cards.Anchor is not null || changes != 0;
            double freeX = w - 2 * m - column.Width, freeY = h - 2 * m - column.Height;
            DragColumnTo(cards, card, m + freeX - 10, m + 0.4 * freeY);
            InsetAnchor? anchor = cards.Anchor;
            bool snapped = anchor is { X: 1 } a && Math.Abs(a.Y - 0.4) < 0.01;
            bool placed = cards.Column is { } c1 && Math.Abs(c1.Right - (w - m)) < 0.5;
            bool saved = window.ToSaved() is { InsetX: 1, InsetY: { } sy } && Math.Abs(sy - 0.4) < 0.01 && changes == 1;
            InsetFrame pic = card.Frame;
            DragCard(card, pic.X + 100, pic.Y + 120, -40, 0, onHeader: false);
            cards.UpdateLayout();
            bool pictureDrag = cards.Anchor is { } pa && Math.Abs(pa.X - (freeX - 40) / freeX) < 0.01 && Math.Abs(pa.Y - 0.4) < 0.01;
            Say($"inset drag: default corner {atDefault}, small move on the header moves nothing {!clickMoved}, header drag snaps x {snapped} (anchor {anchor}, right edge {placed}), " +
                $"saved with the window {saved}, picture drag past the slop {pictureDrag}");
            Expect(atDefault, "the call card column did not start in the default corner");
            Expect(!clickMoved, "a small move on the card's header moved the column");
            Expect(snapped && placed, "dragging the card's header did not snap to the right edge and keep 40% down");
            Expect(saved, "the moved column was not saved with the window");
            Expect(pictureDrag, "dragging the card's picture past the slop did not move the column");

            // parity: inset-drag-resize
            // Resizing the window keeps the column at the same fraction of the free space.
            InsetAnchor? keep = cards.Anchor;
            SizeInt32 size = window.AppWindow.Size;
            var results = new List<bool>();
            foreach ((int dw, int dh) in new[] { (220, 120), (-120, -60) })
            {
                double before = cards.ActualWidth;
                window.AppWindow.Resize(new SizeInt32(size.Width + dw, size.Height + dh));
                await Until(() => Math.Abs(cards.ActualWidth - before) > 1, 5);
                cards.UpdateLayout();
                results.Add(keep is { } k && cards.Anchor == keep && cards.Column is { } cc && NearAnchor(MeasuredAnchor(cc, cards.ActualWidth, cards.ActualHeight), k));
            }
            double last = cards.ActualWidth;
            window.AppWindow.Resize(size);
            await Until(() => Math.Abs(cards.ActualWidth - last) > 1, 5);
            cards.UpdateLayout();
            Say($"inset drag: the column keeps its place when the window grows {results[0]} and shrinks {results[1]}");
            Expect(results.All(r => r), "resizing the window moved the call column off its fraction");

            // parity: inset-drag-restore
            // The position is saved with the window: the record reads back, a window opened from it puts
            // the column there, and Reset Position returns it to the default corner.
            SavedWindow record = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson([window.ToSaved()])))[0];
            bool readsBack = keep is { } kp && record.InsetX == kp.X && record.InsetY == kp.Y;
            ViewerWindow second = app.OpenWindow(window.State.Content, record);
            bool there = false;
            try
            {
                CallStack other = second.Stage.CallCards;
                other.Sync();
                bool shown = await Until(() => other.Cards.Count == 1 && other.Column is not null, 10);
                other.UpdateLayout();
                there = shown && other.Anchor == keep && other.Column is { } oc && keep is { } ok && NearAnchor(MeasuredAnchor(oc, other.ActualWidth, other.ActualHeight), ok);
            }
            finally
            {
                second.Window.Close();
                await Until(() => app.Windows.Count == 1, 5);
            }
            card.ResetPosition();
            cards.UpdateLayout();
            bool reset = cards.Anchor is null && cards.Column is { } rc && NearAnchor(MeasuredAnchor(rc, cards.ActualWidth, cards.ActualHeight), InsetAnchor.FromCorner(v.Settings.InsetCorner))
                && window.ToSaved() is { InsetX: null, InsetY: null };
            Say($"inset drag: saved record reads back {readsBack}, a window opened from it puts the column there {there}, Reset Position {reset}");
            Expect(readsBack, "the window's saved record lost the call column's position");
            Expect(there, "a window opened from the record did not put its call column there");
            Expect(reset, "Reset Position did not return the call column to the default corner");

            // parity: inset-drag-multi
            // Two cards move as one column: at the bottom the newest is at the bottom and the other stacks
            // upward, at the top the reverse. They need the room for two, so the window is made taller.
            var second2 = new CameraRef(camera.Device, camera.Channel + 1);
            v.Calls.StartConversation(second2);
            cards.Sync();
            SizeInt32 tall = window.AppWindow.Size;
            double heightBefore = cards.ActualHeight;
            window.AppWindow.Resize(new SizeInt32(tall.Width, tall.Height + 600));
            await Until(() => Math.Abs(cards.ActualHeight - heightBefore) > 1, 5);
            bool two = await Until(() => { cards.UpdateLayout(); return cards.Cards.Count == 2 && cards.Cards.All(c => c.Placed); }, 5);
            if (two)
            {
                w = cards.ActualWidth;
                h = cards.ActualHeight;
                CallCard newest = cards.Cards[1], oldest = cards.Cards[0];
                InsetFrame start = cards.Column!.Value;
                DragColumnTo(cards, newest, 15, h - start.Height - 15);
                double gap = InsetGeometry.Gap;
                bool upward = cards.Anchor == new InsetAnchor(0, 1) && Math.Abs(newest.Frame.X - m) < 0.5 && Math.Abs(oldest.Frame.X - m) < 0.5
                    && Math.Abs(newest.Frame.Bottom - (h - m)) < 0.5 && Math.Abs(oldest.Frame.Bottom - (newest.Frame.Y - gap)) < 0.5;
                InsetFrame now = cards.Column!.Value;
                DragColumnTo(cards, newest, m + (w - 2 * m - now.Width) / 3, 20);
                bool downward = cards.Anchor is { Y: 0 } ta && Math.Abs(ta.X - 1.0 / 3) < 0.01 && Math.Abs(newest.Frame.X - oldest.Frame.X) < 0.5
                    && Math.Abs(newest.Frame.Y - m) < 0.5 && Math.Abs(oldest.Frame.Y - (newest.Frame.Bottom + gap)) < 0.5;
                Say($"inset drag with two cards: one column, bottom grows upward {upward}, top grows downward {downward}");
                Expect(upward, $"two cards at the bottom did not stack upward from it in one column: {newest.Frame} {oldest.Frame}");
                Expect(downward, $"two cards at the top did not stack downward from it in one column: {newest.Frame} {oldest.Frame}");
            }
            else
            {
                Say($"inset drag with two cards: both did not fit in {cards.ActualWidth:0}x{cards.ActualHeight:0} ({string.Join(", ", cards.Cards.Select(c => c.Frame))})");
                Expect(false, "two call cards did not fit the stage even after making the window taller");
            }
            v.Calls.End(second2, "closed");
            cards.Sync();
            cards.ResetPosition();
            window.AppWindow.Resize(tall);
            await Until(() => Math.Abs(cards.ActualHeight - heightBefore) < 1, 5);
            cards.UpdateLayout();
        }
        finally
        {
            cards.AnchorChanged -= Count;
        }
    }

    // The doorbell fills the stage (focused): its controls show over the stage instead of a card,
    // and the card's lease and picture go; back in the grid, the card returns.
    private async Task CallOverTheStage(Viewer v, StageView stage, CallCard card, CameraRef camera, int leasesBefore)
    {
        StageRenderer r = app.Renderer!;
        // parity: call-stage-controls
        if (Tiles.FirstOrDefault(t => t.Camera == camera) is not { } tile)
        {
            _problems.Add("no tile shows the call's camera to focus");
            return;
        }
        stage.FocusOn(tile);
        stage.CallCards.UpdateLayout();
        bool overStage = card.Mode == CallCardMode.Stage;
        bool undrawn = await Until(() => r.Overlays.Length == 0, 3);
        InsetFrame f = card.Frame;
        double w = stage.ActualWidth, h = stage.ActualHeight;
        Say($"calls: doorbell focused: controls over the stage {overStage} at {f} of {w:0}x{h:0}, picture drawn {!undrawn}, leases {v.Leases.LeaseCount(camera)} (were {leasesBefore})");
        Expect(overStage && Near(f.Bottom, h - InsetGeometry.Margin) && Near(f.MidX, w / 2), "the call's controls did not move over the stage when the doorbell filled it");
        Expect(undrawn && stage.CallCards.LeasesHeld == 0 && v.Leases.LeaseCount(camera) == leasesBefore + 1, "the card kept its picture or lease while the doorbell filled the stage");
        long shown = card.Tile.Shown;
        stage.Unfocus();
        bool unfocused = await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        stage.CallCards.UpdateLayout();
        bool returned = await Until(() => card.Mode == CallCardMode.Inset && r.Overlays.Any(o => o.Tile == card.Tile) && card.Tile.Shown > shown + 2, 10);
        Say($"calls: back in the grid: card {card.Mode}, picture drawn {returned}, leases {v.Leases.LeaseCount(camera)}");
        Expect(unfocused && returned && v.Leases.LeaseCount(camera) == leasesBefore + 2, "the card did not come back with its picture after the focus ended");
    }

    // The computer's own voice, recognizer, and microphone, which depend on what Windows has installed:
    // reported, and failing only when something claims to work and does not.
    private async Task RealCallEngines()
    {
        // parity: call-system-voice
        (float[] samples, uint rate) = await new WindowsSpeech().RenderAsync("Hello, one moment please.");
        Say($"calls: system voice rendered {samples.Length} samples at {rate} Hz ({(rate == 0 ? 0 : samples.Length / (double)rate):0.0} s)");
        Expect(rate >= 8000 && samples.Length > rate / 2, "the system voice rendered nothing usable");

        if (samples.Length > 0)
        {
            // parity: call-desktop-recognizer
            using var sapi = new SapiTranscriber();
            var statuses = new List<string>();
            var texts = new List<string>();
            var guesses = new List<string>();
            sapi.OnStatus = s => statuses.Add(s.Phase + (s.Why is null ? "" : $" ({s.Why})"));
            sapi.OnFinal = t => texts.Add(t);
            sapi.OnPartial = t => guesses.Add(t);
            sapi.Start(TranscriptSource.Visitor);
            // 16 kHz by linear interpolation (the voice's own rate is higher), fed in 100 ms blocks.
            float[] pcm = new float[(int)(samples.Length * 16_000L / rate)];
            for (int i = 0; i < pcm.Length; i++)
            {
                double at = i * (double)rate / 16_000;
                int j = (int)at;
                double f = at - j;
                pcm[i] = (float)(samples[j] * (1 - f) + samples[Math.Min(j + 1, samples.Length - 1)] * f);
            }
            for (int at = 0; at < pcm.Length; at += 1600)
            {
                sapi.Feed(pcm.AsSpan(at, Math.Min(1600, pcm.Length - at)));
            }
            sapi.Feed(new float[16_000]);
            await Until(() => texts.Count > 0 || statuses.Any(s => s.StartsWith("Unavailable", StringComparison.Ordinal)), 10);
            sapi.Finish();
            // The final result after the end of the audio, or nothing more where the recognizer is unavailable.
            await Until(() => statuses.Any(s => s.StartsWith("Unavailable", StringComparison.Ordinal)) ||
                string.Join(" ", texts).Contains("moment", StringComparison.OrdinalIgnoreCase), 5);
            Say($"calls: desktop recognizer: {string.Join(", ", statuses)}; heard \"{string.Join(" ", texts)}\", guesses \"{string.Join(" | ", guesses.TakeLast(3))}\"; {sapi.Counts}");
            Expect(statuses.Any(x => x.StartsWith("Unavailable", StringComparison.Ordinal)) ||
                string.Join(" ", texts).Contains("moment", StringComparison.OrdinalIgnoreCase),
                "the desktop recognizer is available but did not hear the system voice's sentence");
        }

        // parity: call-microphone
        if (options.MicrophoneTest)
        {
            var mic = new WasapiMicrophone(() => app.Audio?.DeviceId);
            int blocks = 0, samplesSeen = 0;
            float peak = 0;
            string? problem = mic.Start(block =>
            {
                Interlocked.Increment(ref blocks);
                Interlocked.Add(ref samplesSeen, block.Length);
                foreach (float x in block)
                {
                    peak = Math.Max(peak, Math.Abs(x));
                }
            });
            if (problem is null)
            {
                await Until(() => Volatile.Read(ref blocks) >= 5, 3);
            }
            mic.Stop();
            Say($"calls: microphone {(problem is null ? $"opened: {blocks} blocks, {samplesSeen} samples at 16 kHz, peak {peak:0.000}" : $"not available: {problem}")}");
            // What Windows reports about its echo canceller for this microphone and the output that plays.
            Say($"calls: microphone echo cancellation: {(mic.Echo is { } echo ? echo.Active ? "on" : $"off ({echo.Why})" : "not reported")}, playback device {(app.Audio?.DeviceId is null ? "none" : "known")}");
            Expect(problem is not null || mic.Echo is not null, "the microphone opened but did not report on echo cancellation");
            Expect(problem is not null || blocks > 10, "the microphone opened but delivered no audio");
        }
    }
}
