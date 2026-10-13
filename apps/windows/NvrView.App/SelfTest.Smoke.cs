// --smoke (with --selftest): the short run for the trimmed release build (scripts/windows-worker.sh runs it on the
// publish output in full checks and releases). It runs the startup area and then one wiring check per other area, each
// a quick repeat of a step the debug build's full self-test runs (named in the comments; they carry the parity
// markers there). What trimming can break is code reached only by reflection or by the XAML and JSON paths, and each
// check below goes through such a path once: a window of each kind, a menu, a settings file, a picture file, a core
// request of each kind. That trimming keeps what the app reflects on is also checked statically when the app is
// published (TrimmerRootDescriptor and trim analysis warnings as errors in NvrView.App.csproj).

using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private async Task SmokeSteps(Viewer v, StageView stage)
    {
        StageTile first = Tiles[0];

        // viewer (focus-main-stream, unfocus-sub-stream): focus switches to the main stream and back.
        stage.FocusOn(first);
        bool main = await Until(() => first.ShownTier == "main" && first.PictureWidth > 0, FocusAnimation.Seconds + 8);
        stage.Unfocus();
        bool sub = await Until(() => first.ShownTier == "sub", FocusAnimation.Seconds + 8);
        Say($"smoke viewer: focus main {main}, back to sub {sub}");
        Expect(main && sub, "smoke: focus did not switch to the main stream and back");

        // tiles (quality-clear-auto): the Quality menu's choices and Clear reach the core.
        Expect(QualityMenu.Choices(false).Count > 0, "smoke: the Quality menu has no choices");
        v.SetQuality(first, TileQuality.Clear);
        bool clear = await Until(() => first.ShownTier == "main", 10);
        v.SetQuality(first, TileQuality.Auto);
        bool auto = await Until(() => first.ShownTier == "sub", 10);
        Say($"smoke tiles: Clear main {clear}, Auto sub {auto}");
        Expect(clear && auto, "smoke: Clear and Auto did not switch the stream");

        // header (header-overflow-menu) and layouts (layout-save): the "…" menu builds, and a layout is stored and read back.
        ViewerWindow w1 = app.Windows[0];
        int overflow = w1.Header.BuildOverflow().Items.Count;
        string? id = w1.SaveAsLayout(v.UniqueLayoutName("Smoke layout"));
        bool stored = id is not null && v.Layout(id) is { } layout && layout.Root == w1.State.Tree;
        if (id is not null)
        {
            v.DeleteLayout(id);
        }
        Say($"smoke header and layouts: overflow items {overflow}, layout stored {stored}");
        Expect(overflow > 0, "smoke: the header's … menu is empty");
        Expect(stored, "smoke: Save as layout did not store the window's tree");

        // settings (settings-dialog-opens, updates-bundled-defaults): the Settings window opens and closes, and the
        // bundled update defaults are read.
        SettingsWindow settings = SettingsWindow.Open(v, development: true, SettingsSection.Devices, background: true);
        bool shown = await Until(() => settings.IsLoaded, 10);
        settings.Close();
        bool closed = await Until(() => SettingsWindow.Current is null, 5);
        Say($"smoke settings: window shown {shown}, closed {closed}");
        Expect(shown && closed, "smoke: the Settings window did not open and close");
        _problems.AddRange(UpdateDefaultsSelfTest.Run(v, line => Say($"smoke {line}")));

        // capture (capture-picture-tile): Save Picture writes a file.
        PictureSaveRecord? picture = await Saving(stage, () => stage.PerformHeaderItem(first, TileHeaderItem.SavePicture, null));
        string? file = picture?.Result.Saved?.Path;
        bool saved = file is not null && File.Exists(file);
        Say($"smoke capture: picture saved {saved}");
        Expect(saved, "smoke: Save Picture wrote no file");
        if (file is not null)
        {
            TryDelete(file);
        }

        // calls (call-conversation-start): a conversation opens on the talk-capable camera and ends.
        StageTile? doorbell = Tiles.FirstOrDefault(t => v.ChannelOf(t.Camera)?.TalkOf(t.Camera.Lens) == true);
        if (doorbell is null)
        {
            _problems.Add("smoke: no talk-capable camera");
        }
        else
        {
            v.Calls.StartConversation(doorbell.Camera);
            bool open = await Until(() => v.Calls.CallFor(doorbell.Camera) is not null, 10);
            v.Calls.End(doorbell.Camera, "closed");
            bool ended = await Until(() => v.Calls.CallFor(doorbell.Camera) is null, 10);
            Say($"smoke calls: conversation open {open}, ended {ended}");
            Expect(open && ended, "smoke: a conversation did not open and end");
        }

        // ptz (ptz-preset-recall): the camera's PTZ abilities are read from the core.
        StageTile? mover = Tiles.FirstOrDefault(t => t.CameraOrNone is { Lens: 0 } c && v.ChannelOf(c)?.PtzOf(0).PanTilt == true);
        if (mover is null)
        {
            _problems.Add("smoke: no camera that pans and tilts");
        }
        else
        {
            v.Ptz.Load(mover.Camera);
            bool read = await Until(() => v.Ptz.InfoFor(mover.Camera) is { Presets.Count: > 0 }, 15);
            Say($"smoke ptz: presets read {read}");
            Expect(read, "smoke: the PTZ presets were not read");
        }

        // recordings (recordings-window-close): the Recordings window opens on the service and closes.
        if (app.Recordings is { Available: true } service)
        {
            RecordingsWindow w = RecordingsWindow.Open(v, v.Host, service, background: true);
            bool loaded = await Until(() => w.IsLoaded, 10);
            w.Close();
            bool gone = await Until(() => w.IsClosed, 5);
            Say($"smoke recordings: window loaded {loaded}, closed {gone}");
            Expect(loaded && gone, "smoke: the Recordings window did not open and close");
        }
        else
        {
            _problems.Add("smoke: no recordings service");
        }
    }

    private static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // A leftover picture in the self-test's folder is harmless.
        }
    }
}
