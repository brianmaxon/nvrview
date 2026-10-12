// Self-test area "layouts": layouts, second windows, and the sidebar.

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
    // 11. Layouts, windows, and the sidebar (docs/UX.md "Layouts", "Sidebar").
    private async Task LayoutsAndWindows(Viewer v)
    {
        ViewerWindow w1 = app.Windows[0];
        StageView stage = w1.Stage;
        StageRenderer r = w1.Renderer;

        // parity: layout-save
        // Save the arrangement as a stored layout; the window now shows it.
        int leaves = stage.Tiles.Length;
        string? id = w1.SaveAsLayout(v.UniqueLayoutName("Self-test layout"));
        SavedLayout? stored = id is null ? null : v.Layout(id);
        Say($"layout saved: {id ?? "refused"}, {stored?.Root.LeafCount ?? 0} tiles, window shows it: {w1.State.Content is WindowContent.Layout(string shown) && shown == id}");
        Expect(stored is not null && stored.Root == w1.State.Tree, "Save as layout did not store the window's tree");
        if (id is null || stored is null)
        {
            return;
        }
        Expect(v.Devices[0].Config.CoreKey is { } coreKey && stored.Root.Cameras.All(c => c.Device == coreKey),
            "the layout does not name cameras by the core's device key");
        try
        {
            // parity: layout-designer-edits
            // Split the first tile, put a camera in the new tile, swap two tiles: each edit is stored.
            stage.SetEditing(true);
            NodePath first = stage.Tiles[0].Path;
            stage.Select(first);
            stage.Split(SplitAxis.Row);
            NodePath? added = w1.State.Selected;
            Expect(v.Layout(id)?.Root.LeafCount == leaves + 1, "splitting a tile was not stored");
            CameraRef extra = new(v.Devices[0].Key, 0);
            w1.CameraPicked(extra);
            Expect(added is { } p && v.Layout(id)?.Root[p]?.Camera == extra, "a camera picked in the sidebar did not fill the selected tile");
            NodePath a = stage.Tiles[0].Path, b = stage.Tiles[^1].Path;
            CameraRef? ca = stage.Tiles[0].CameraOrNone, cb = stage.Tiles[^1].CameraOrNone;
            stage.Swap(a, b);
            LayoutNode? after = v.Layout(id)?.Root;
            Say($"designer: split to {after?.LeafCount} tiles, swapped {ca} and {cb}: stored {after?[a]?.Camera} and {after?[b]?.Camera}");
            Expect(after?[a]?.Camera == cb && after?[b]?.Camera == ca, "swapping two tiles was not stored");
            // parity: layout-quality-saved
            // A tile's quality is saved in its leaf ("quality"), and Auto removes it.
            stage.SetQuality(stage.Tiles[0], TileQuality.Fluent);
            string? member = v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality");
            stage.SetQuality(stage.Tiles[0], TileQuality.Auto);
            Expect(member == "\"fluent\"" && v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality") is null,
                "a tile's quality was not kept in its layout leaf");
            stage.SetEditing(false);
            // parity: layout-roundtrip
            // The stored JSON reads back to the same layout.
            Expect(SavedLayout.Parse(v.Layout(id)!.ToJson()) == v.Layout(id), "the stored layout does not round-trip");

            bool playing = await Until(() => Tiles.All(t => (t.Video?.Counters.Decoded ?? 0) > 3 && t.PictureWidth > 0), 15);
            Expect(playing, $"tiles did not play after the edits: {string.Join("; ", Tiles.Select(Describe))}");

            // parity: focus-content-change
            // Changing the content while a tile is focused ends the focus only: the renderer keeps presenting.
            StageTile focusTile = Tiles[0];
            stage.FocusOn(focusTile);
            long focusFrames = r.Frames;
            await Until(() => r.Frames - focusFrames >= 5, 5);
            int chains = r.SwapChains;
            long frames = r.Frames;
            w1.Show(new WindowContent.Layout(id));
            await Until(() => r.Frames - frames > 12, 5);
            Say($"content changed while focused: focus {(r.Focus is null ? "ended" : "kept")}, swap chains {chains} -> {r.SwapChains}, frames +{r.Frames - frames}");
            Expect(r.Focus is null && !focusTile.Focused, "changing the content did not end the focus");
            Expect(r.SwapChains == chains && r.Frames - frames > 10, "changing the content while focused disturbed the renderer");
            await CheckFrame("after a content change while focused", Tiles);

            // parity: second-window
            // The same layout in a second window: its own render thread, shared core tiles.
            ViewerWindow w2 = app.OpenWindow(new WindowContent.Layout(id));
            bool drawn = await Until(() => w2.Renderer.Frames > 30 && w2.Stage.Tiles.Where(t => !t.IsEmpty).All(t => t.PictureWidth > 0), 15);
            StageTile[] t2 = [.. w2.Stage.Tiles.Where(t => !t.IsEmpty)];
            bool shared = t2.All(t => Tiles.FirstOrDefault(o => o.CameraOrNone == t.CameraOrNone) is { } o && o.Id == t.Id && t.Id != 0);
            CameraRef sample = t2[0].Camera;
            Say($"second window: {w2.Renderer.Frames} frames, {t2.Length} tiles, same core tiles {shared}, leases of {sample} {v.Leases.LeaseCount(sample)}, core tiles {v.Tiles.Length}");
            Expect(drawn, "the second window did not draw its tiles");
            Expect(shared, "the second window did not share the first window's core tiles");
            Expect(v.Tiles.Length == v.Leases.Cameras.Count, "a camera has more than one core tile");
            if (drawn)
            {
                await CheckFrame("second window", t2, w2.Renderer);
            }

            // parity: second-window-follows
            // An edit in one window reaches the other (layouts_changed), not during a drag.
            stage.Edit(t => t.SettingRatio(0.3, NodePath.Root));
            bool followed = await Until(() => w2.State.Tree == w1.State.Tree, 5);
            Say($"second window followed the edit: {followed}");
            Expect(followed, "the second window did not follow the stored layout");

            // parity: second-window-close
            // Closing it gives its leases back; the first window keeps playing.
            int before = v.Leases.LeaseCount(sample);
            w2.Window.Close();
            await Until(() => app.Windows.Count == 1, 3);
            Say($"second window closed: windows {app.Windows.Count}, leases of {sample} {before} -> {v.Leases.LeaseCount(sample)}");
            Expect(app.Windows.Count == 1 && v.Leases.LeaseCount(sample) == before - t2.Count(t => t.Camera == sample), "closing a window did not give its leases back");

            // parity: sidebar-reveal
            // The sidebar: collapse it (the stage grows), then reveal it at the
            // edge on a manual clock: the overlay slides over the stage, whose
            // buffers are not resized.
            w1.SetSidebarDocked(false);
            await Task.Delay(700);
            (uint bw, uint bh) = r.BufferSize;
            long resizes = r.TotalResizes;
            var clock = new ManualViewerClock(1000);
            w1.RevealClock = clock;
            w1.PointerAt(inEdge: true, inOverlay: false);
            clock.Advance(0.1);
            w1.TickReveal();
            bool early = w1.RevealShown;
            clock.Advance(0.05);
            w1.TickReveal();
            bool shownNow = w1.RevealShown;
            long frames2 = r.Frames;
            await Until(() => r.Frames - frames2 > 5, 5);
            Say($"reveal: after 0.10 s {early}, after 0.15 s {shownNow}; buffers {bw}x{bh} -> {r.BufferSize.Width}x{r.BufferSize.Height}, resizes +{r.TotalResizes - resizes}, frames +{r.Frames - frames2}");
            Expect(!early && shownNow, "the edge reveal did not wait about 150 ms and then show");
            Expect(r.TotalResizes == resizes && r.BufferSize == (bw, bh) && !w1.SidebarDocked, "revealing the sidebar resized the stage");
            w1.PointerAt(inEdge: false, inOverlay: true);
            w1.PointerAt(inEdge: false, inOverlay: false);
            clock.Advance(0.49);
            w1.TickReveal();
            bool stays = w1.RevealShown;
            clock.Advance(0.02);
            w1.TickReveal();
            Expect(stays && !w1.RevealShown, "the overlay did not leave half a second after the pointer");
            // parity: window-restore
            // The window remembers the collapsed sidebar (window restore).
            List<SavedWindow> saved = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson(app.Windows.Select(w => w.ToSaved()))));
            Say($"window restore: {saved.Count} window(s), content {saved.FirstOrDefault()?.Content}, sidebar collapsed {saved.FirstOrDefault()?.SidebarCollapsed}, frame {saved.FirstOrDefault()?.Frame}");
            Expect(saved.Count == 1 && saved[0].Content == new WindowContent.Layout(id) && saved[0].SidebarCollapsed && saved[0].Frame.Width >= ViewerWindow.MinWidth,
                "the window's restore record is wrong");
            w1.RevealClock = SystemClockInstance;
            w1.SetSidebarDocked(true);
            await Task.Delay(300);
        }
        finally
        {
            // The store lives on in the self-test's data directory.
            v.DeleteLayout(id);
        }
        Expect(v.Layout(id) is null && w1.State.Content is WindowContent.Scratch, "deleting the layout left the window showing it");
    }
}
