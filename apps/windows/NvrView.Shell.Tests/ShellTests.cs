// Geometry, leases, the sidebar reveal (on a manual clock: nothing waits for
// real time), groups, window restore, and drag text.

using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell.Tests;

internal static class GeometryTests
{
    public static void Run()
    {
        var bounds = new Rect(0, 0, 1000, 600);

        T.Run("geometry: frames with a gap", () =>
        {
            LayoutFrames f = Fixtures.Contract.Frames(bounds, 4);
            T.Equal(3, f.Leaves.Count);
            T.Equal(new Rect(0, 0, 618, 600), f.Leaves[0].Rect);
            Rect top = f.Leaves[1].Rect, bottom = f.Leaves[2].Rect;
            T.Equal(622.0, top.X);
            T.Equal(1000.0, top.Right);
            T.Equal(top.Bottom + 4, bottom.Y);
            T.Equal(600.0, bottom.Bottom);
            DividerFrame root = f.Dividers.First(d => d.Path.IsRoot);
            T.Equal(new Rect(618, 0, 4, 600), root.Rect);
        });

        T.Run("geometry: divider hit testing and dragging", () =>
        {
            LayoutFrames f = Fixtures.Contract.Frames(bounds, 2);
            DividerFrame root = T.NotNull(f.DividerAt(620, 300));
            T.Check(root.Path.IsRoot);
            // The stacked divider inside b is preferred where both are near.
            DividerFrame inner = T.NotNull(f.DividerAt(800, f.Dividers.First(d => d.Path.Steps == "b").Rect.Y + 1));
            T.Equal("b", inner.Path.Steps);
            T.Check(f.DividerAt(300, 300) is null);
            T.Equal(0.5, LayoutGeometry.RatioDragging(root, 500, 0, 0));
            T.Equal(LayoutNode.MaxRatio, LayoutGeometry.RatioDragging(root, 5000, 0, 0));
            T.Equal(LayoutNode.MinRatio, LayoutGeometry.RatioDragging(root, -50, 0, 0));
            T.Equal(Fixtures.P("ba"), T.NotNull(f.LeafAt(900, 10)).Path);
        });

        T.Run("geometry: a picture fits its cell letterboxed", () =>
        {
            T.Equal(new Rect(0, 75, 400, 225), LayoutGeometry.Fit(1920, 1080, new Rect(0, 0, 400, 375)));
            T.Equal(new Rect(100, 0, 200, 300), LayoutGeometry.Fit(640, 960, new Rect(0, 0, 400, 300)));
            T.Equal(new Rect(5, 5, 10, 10), LayoutGeometry.Fit(0, 0, new Rect(5, 5, 10, 10)));
        });

        T.Run("geometry: arrow-key neighbors by position", () =>
        {
            // a | (ba over bb)
            IReadOnlyList<Rect> rects = [.. Fixtures.Contract.Frames(bounds, 2).Leaves.Select(l => l.Rect)];
            T.Equal(2, LayoutGeometry.Neighbor(rects, 0, 1, 0), "right of a: both touch; the one across from its center");
            T.Equal(0, LayoutGeometry.Neighbor(rects, 1, -1, 0));
            T.Equal(2, LayoutGeometry.Neighbor(rects, 1, 0, 1));
            T.Equal(-1, LayoutGeometry.Neighbor(rects, 0, 0, 1), "a spans the whole height");
            T.Equal(-1, LayoutGeometry.Neighbor(rects, 1, 1, 0));
        });
    }
}

internal static class LeaseTests
{
    private sealed class FakeTile(int serial)
    {
        public int Serial { get; } = serial;

        public bool Focused;
    }

    private sealed class Backend : ITileBackend<FakeTile>
    {
        public readonly HashSet<string> Connected = ["dev-a"];
        public readonly List<string> Log = [];
        private int _serial;

        public FakeTile? Open(CameraRef camera, bool focused)
        {
            if (!Connected.Contains(camera.Device))
            {
                return null;
            }
            Log.Add($"open {camera}{(focused ? " focused" : "")}");
            return new FakeTile(++_serial) { Focused = focused };
        }

        public void Close(CameraRef camera, FakeTile tile) => Log.Add($"close {camera}");

        public void FocusChanged(CameraRef camera, FakeTile tile, bool focused)
        {
            tile.Focused = focused;
            Log.Add($"focus {camera} {focused}");
        }
    }

    public static void Run()
    {
        CameraRef a = Fixtures.Cam(1), b = Fixtures.Cam(2), lens = Fixtures.Cam(2, lens: 1), other = Fixtures.Cam(0, "dev-b");

        T.Run("leases: one tile per camera, closed with the last lease", () =>
        {
            var backend = new Backend();
            var leases = new TileLeases<FakeTile>(backend);
            FakeTile t1 = T.NotNull(leases.Acquire(a));
            FakeTile t2 = T.NotNull(leases.Acquire(a));
            T.Check(ReferenceEquals(t1, t2), "a second window shares the tile");
            T.Equal(2, leases.LeaseCount(a));
            leases.Release(a);
            T.Check(ReferenceEquals(t1, leases.Tile(a)));
            leases.Release(a);
            T.Check(leases.Tile(a) is null);
            T.Equal(0, leases.LeaseCount(a));
            leases.Release(a); // nothing held: no effect
            T.Equal("open dev-a/1|close dev-a/1", string.Join("|", backend.Log));
            // A second lens is a camera of its own.
            T.Check(!ReferenceEquals(leases.Acquire(b), leases.Acquire(lens)));
        });

        T.Run("leases: an owner's update acquires before it releases", () =>
        {
            var backend = new Backend();
            var leases = new TileLeases<FakeTile>(backend);
            var w1 = new Dictionary<CameraRef, int> { [a] = 2, [b] = 1 };
            leases.Update(new Dictionary<CameraRef, int>(), w1);
            T.Equal(2, leases.LeaseCount(a));
            var w2 = new Dictionary<CameraRef, int> { [b] = 1, [lens] = 1 };
            leases.Update(w1, w2);
            T.Equal(0, leases.LeaseCount(a));
            T.Equal(1, leases.LeaseCount(b));
            T.Equal("open dev-a/1|open dev-a/2|open dev-a/2/lens1|close dev-a/1", string.Join("|", backend.Log));
            // A second window with the same cameras shares them.
            leases.Update(new Dictionary<CameraRef, int>(), w2);
            T.Equal(2, leases.LeaseCount(b));
            leases.Update(w2, new Dictionary<CameraRef, int>());
            T.Check(leases.Tile(b) is not null && leases.Tile(lens) is not null);
        });

        T.Run("leases: focus is counted across windows", () =>
        {
            var backend = new Backend();
            var leases = new TileLeases<FakeTile>(backend);
            FakeTile t = T.NotNull(leases.Acquire(a));
            leases.Acquire(a);
            leases.SetFocused(a, true);
            leases.SetFocused(a, true);
            T.Check(t.Focused && leases.IsFocused(a));
            leases.SetFocused(a, false);
            T.Check(t.Focused, "still focused in the other window");
            leases.SetFocused(a, false);
            T.Check(!t.Focused && !leases.IsFocused(a));
            leases.SetFocused(a, false);
            T.Equal(2, backend.Log.Count(l => l.StartsWith("focus", StringComparison.Ordinal)), "one change each way");
            leases.SetFocused(b, true); // not leased: ignored
            T.Check(!leases.IsFocused(b));
        });

        T.Run("leases: a device's tiles close and reopen with it; the core ending a tile", () =>
        {
            var backend = new Backend();
            var leases = new TileLeases<FakeTile>(backend);
            T.Check(leases.Acquire(other) is null, "not connected yet");
            T.Equal(1, leases.LeaseCount(other));
            backend.Connected.Add("dev-b");
            leases.Reopen("dev-b");
            T.Check(leases.Tile(other) is not null);
            leases.Acquire(a);
            leases.CloseTiles("dev-b");
            T.Check(leases.Tile(other) is null && leases.Tile(a) is not null);
            T.Equal(1, leases.LeaseCount(other), "the lease stays");
            leases.SetFocused(other, true);
            leases.Reopen("dev-b");
            T.Check(T.NotNull(leases.Tile(other)).Focused, "reopened on the focused tier");
            FakeTile ended = T.NotNull(leases.Tile(a));
            leases.TileEnded(ended);
            T.Check(leases.Tile(a) is null);
            T.Check(leases.Camera(ended) is null);
            FakeTile again = T.NotNull(leases.Reopen(a));
            T.Check(!ReferenceEquals(again, ended));
            int changes = 0;
            leases.TileChanged += (_, _) => changes++;
            leases.CloseAll();
            T.Equal(2, changes);
            T.Equal(0, leases.Cameras.Count);
        });
    }
}

internal static class RevealTests
{
    public static void Run()
    {
        T.Run("reveal: a pass over the edge does not count; resting does", () =>
        {
            var clock = new ManualViewerClock(1000);
            var r = new SidebarReveal();
            r.Pointer(inEdge: true, inOverlay: false, clock.Now);
            T.Equal(clock.Now + 0.15, r.Deadline);
            clock.Advance(0.1);
            r.Pointer(inEdge: false, inOverlay: false, clock.Now);
            T.Check(r.Deadline is null, "left before the dwell");
            T.Check(!r.Tick(clock.Now + 1) && !r.IsShown);
            r.Pointer(true, false, clock.Now);
            clock.Advance(0.1);
            T.Check(!r.Tick(clock.Now), "not yet");
            r.Pointer(true, false, clock.Now); // moving within the strip keeps the first deadline
            clock.Advance(0.05);
            T.Check(r.Tick(clock.Now) && r.IsShown, "shown after 150 ms");
            T.Check(r.Deadline is null);
        });

        T.Run("reveal: hides half a second after the pointer leaves", () =>
        {
            var clock = new ManualViewerClock(1000);
            var r = Shown(clock);
            r.Pointer(false, true, clock.Now);
            T.Check(r.Deadline is null, "over the overlay");
            r.Pointer(false, false, clock.Now);
            T.Equal(clock.Now + 0.5, r.Deadline);
            clock.Advance(0.3);
            r.Pointer(false, true, clock.Now); // back in time
            T.Check(r.Deadline is null);
            r.Pointer(false, false, clock.Now);
            clock.Advance(0.49);
            T.Check(!r.Tick(clock.Now) && r.IsShown);
            clock.Advance(0.01);
            T.Check(r.Tick(clock.Now) && !r.IsShown);
        });

        T.Run("reveal: drags, menus, and editing hold it", () =>
        {
            var clock = new ManualViewerClock(1000);
            var r = Shown(clock);
            r.Pointer(false, false, clock.Now);
            clock.Advance(0.5);
            foreach (RevealHolds hold in new[] { RevealHolds.Drag, RevealHolds.Menu, RevealHolds.Editing })
            {
                T.Check(!r.Tick(clock.Now, hold) && r.IsShown, hold.ToString());
                T.Equal(clock.Now + 0.25, r.Deadline, "looks again soon");
                clock.Advance(0.25);
            }
            T.Check(r.Tick(clock.Now) && !r.IsShown, "released: hides");
        });

        T.Run("reveal: docking hides at once and disables it", () =>
        {
            var clock = new ManualViewerClock(1000);
            var r = Shown(clock);
            r.SetEnabled(false);
            T.Check(!r.IsShown && r.Deadline is null);
            r.Pointer(true, false, clock.Now);
            clock.Advance(1);
            T.Check(!r.Tick(clock.Now) && !r.IsShown);
            r.SetEnabled(true);
            r.Pointer(true, false, clock.Now);
            clock.Advance(0.15);
            T.Check(r.Tick(clock.Now) && r.IsShown);
        });

        T.Run("header: always visible in a normal window; nothing is revealed", () =>
        {
            var clock = new ManualViewerClock(1000);
            var h = new HeaderChrome();
            T.Check(h.IsDocked && h.IsVisible && !h.IsOverlayShown);
            h.Pointer(true, false, clock.Now);
            clock.Advance(1);
            T.Check(!h.Tick(clock.Now) && h.IsVisible && h.Deadline is null);
        });

        T.Run("header: full screen hides it; resting at the top reveals it; it slides away after the pointer leaves", () =>
        {
            var clock = new ManualViewerClock(1000);
            var h = new HeaderChrome();
            h.SetFullScreen(true);
            T.Check(!h.IsVisible && !h.IsDocked, "hidden at once");
            h.Pointer(true, false, clock.Now);
            clock.Advance(0.14);
            T.Check(!h.Tick(clock.Now) && !h.IsVisible, "a pass over the edge");
            clock.Advance(0.02);
            T.Check(h.Tick(clock.Now) && h.IsVisible && h.IsOverlayShown, "shown after 150 ms");
            h.Pointer(false, true, clock.Now);
            clock.Advance(5);
            T.Check(!h.Tick(clock.Now) && h.IsVisible, "stays while the pointer is on it");
            h.Pointer(false, false, clock.Now);
            clock.Advance(0.49);
            T.Check(!h.Tick(clock.Now) && h.IsVisible);
            clock.Advance(0.01);
            T.Check(h.Tick(clock.Now) && !h.IsVisible, "hidden half a second after");
        });

        T.Run("header: an open menu holds it; leaving full screen docks it", () =>
        {
            var clock = new ManualViewerClock(1000);
            var h = new HeaderChrome();
            h.SetFullScreen(true);
            h.Pointer(true, false, clock.Now);
            clock.Advance(0.15);
            T.Check(h.Tick(clock.Now) && h.IsVisible);
            h.Pointer(false, false, clock.Now);
            clock.Advance(0.5);
            T.Check(!h.Tick(clock.Now, RevealHolds.Menu) && h.IsVisible, "held by the menu");
            clock.Advance(0.25);
            T.Check(h.Tick(clock.Now) && !h.IsVisible, "released");
            h.Pointer(true, false, clock.Now);
            h.SetFullScreen(false);
            T.Check(h.IsDocked && h.IsVisible && !h.IsOverlayShown && h.Deadline is null, "docked again, nothing pending");
        });
    }

    private static SidebarReveal Shown(ManualViewerClock clock)
    {
        var r = new SidebarReveal();
        r.Pointer(true, false, clock.Now);
        clock.Advance(0.15);
        r.Tick(clock.Now);
        T.Check(r.IsShown);
        return r;
    }
}

internal static class StoreTests
{
    public static void Run()
    {
        string dir = Path.Combine(Path.GetTempPath(), "nvrview-shell-tests-" + Guid.NewGuid().ToString("N")[..8]);
        try
        {
            Tests(dir);
        }
        finally
        {
            if (Directory.Exists(dir))
            {
                Directory.Delete(dir, recursive: true);
            }
        }
    }

    private static void Tests(string dir)
    {
        T.Run("windows: restore round trip through the file", () =>
        {
            var store = new WindowStore(Path.Combine(dir, "windows.json"));
            T.Equal(0, store.Load().Count);
            List<SavedWindow> windows =
            [
                new(new WindowContent.Layout("layout-1"), new Rect(10, 20, 1180, 700), SidebarCollapsed: true, SidebarWidth: 280),
                new(new WindowContent.Group("group-1"), new Rect(0, 0, 800, 500), FullScreen: true, InsetX: 1, InsetY: 0.4, InsetGrid: new InsetGridSetting(2, 3)),
                new(new WindowContent.Scratch("Unsaved", LayoutNode.Split(SplitAxis.Row, 0.5,
                    LayoutNode.Leaf(Fixtures.Cam(2)), LayoutNode.Leaf(Fixtures.Cam(4, lens: 1)))), new Rect(-1200, 300, 900, 600)),
            ];
            store.Save(windows);
            List<SavedWindow> back = store.Load();
            T.Equal(3, back.Count);
            for (int i = 0; i < 3; i++)
            {
                T.Equal(windows[i] with { Content = null! }, back[i] with { Content = null! }, $"window {i}");
            }
            T.Equal<WindowContent>(windows[0].Content, back[0].Content);
            T.Equal<WindowContent>(windows[1].Content, back[1].Content);
            var scratch = (WindowContent.Scratch)back[2].Content;
            T.Check(scratch.Name == "Unsaved" && scratch.Root == ((WindowContent.Scratch)windows[2].Content).Root);
            string text = File.ReadAllText(Path.Combine(dir, "windows.json"));
            T.Check(!text.Contains("password", StringComparison.Ordinal) && !text.Contains("host", StringComparison.Ordinal), "ids, names, trees, frames only");
            T.Check(!File.Exists(Path.Combine(dir, "windows.json.tmp")));
            T.Check(back[0].InsetX is null && back[0].InsetY is null && back[1].InsetX == 1 && back[1].InsetY == 0.4, "the inset position is kept, and absent while default");
            T.Check(!text.Contains("insetAnchor", StringComparison.Ordinal) || text.IndexOf("insetAnchor", StringComparison.Ordinal) == text.LastIndexOf("insetAnchor", StringComparison.Ordinal), "only the moved window has one");
            T.Check(back[0].InsetGrid is null && back[1].InsetGrid == new InsetGridSetting(2, 3), "the window's own inset grid is kept, and absent while it follows the Settings");
            List<SavedWindow> grids = WindowStore.FromJson(JsonNode.Parse("""
                [{"content":{"layout":{"id":"x"}},"frame":{"x":1,"y":2,"width":900,"height":600},"insetGrid":{"rows":9,"columns":0}},
                 {"content":{"layout":{"id":"y"}},"frame":{"x":1,"y":2,"width":900,"height":600},"insetGrid":{"rows":"a"}}]
                """));
            T.Check(grids[0].InsetGrid == new InsetGridSetting(4, 1) && grids[1].InsetGrid is null, "a saved inset grid is clamped; a damaged one is ignored");
            List<SavedWindow> bad = WindowStore.FromJson(JsonNode.Parse("""
                [{"content":{"layout":{"id":"x"}},"frame":{"x":1,"y":2,"width":900,"height":600},"insetAnchor":{"x":3,"y":-1}},
                 {"content":{"layout":{"id":"y"}},"frame":{"x":1,"y":2,"width":900,"height":600},"insetAnchor":{"x":"a"}}]
                """));
            T.Check(bad[0].InsetX == 1 && bad[0].InsetY == 0 && bad[1].InsetX is null, "a saved inset position is clamped; a damaged one is ignored");
        });

        T.Run("windows: old or damaged entries", () =>
        {
            // Without the sidebar member: open. Unknown content: skipped.
            JsonNode doc = JsonNode.Parse("""
                [{"content":{"layout":{"id":"x"}},"frame":{"x":1,"y":2,"width":900,"height":600},"fullScreen":false},
                 {"content":{"hexagon":{}},"frame":{"x":1,"y":2,"width":900,"height":600}},
                 {"content":{"scratch":{"name":"N","root":{"split":"row","ratio":7,"a":{"camera":null},"b":{"camera":null}}}},"frame":{"x":1,"y":2,"width":9,"height":9}}]
                """)!;
            List<SavedWindow> list = WindowStore.FromJson(doc);
            T.Equal(1, list.Count);
            T.Check(!list[0].SidebarCollapsed);
            File.WriteAllText(Path.Combine(dir, "bad.json"), "{nope");
            T.Equal(0, new WindowStore(Path.Combine(dir, "bad.json")).Load().Count);
            T.Check(File.Exists(Path.Combine(dir, "bad.json.unreadable")));
        });

        // The shared vectors (fixtures/synthetic/tile-views.json "saved" and "migrate"; the Mac app's TileViewsTests
        // reads them too).
        static JsonObject Window(JsonNode c)
        {
            var o = new JsonObject
            {
                ["content"] = new JsonObject { ["layout"] = new JsonObject { ["id"] = "layout-1" } },
                ["frame"] = new JsonObject { ["x"] = 0.0, ["y"] = 0.0, ["width"] = 800.0, ["height"] = 500.0 },
            };
            foreach ((string key, JsonNode? value) in c["window"]!.AsObject())
            {
                o[key] = value?.DeepClone();
            }
            return o;
        }
        static string Want(JsonNode c) => string.Join("; ", c["entries"]!.AsArray().Select(e =>
        {
            JsonArray a = e!.AsArray();
            string fill = a[8] is null ? "null" : a[8]!.GetValue<bool>().ToString();
            return $"{a[0]!.GetValue<string>()} {a[1]!.GetValue<string>()} {a[2]!.GetValue<string>()} {a[3]!.GetValue<int>()} {a[4]!.GetValue<int>()} {a[5]!.GetValue<double>()} {a[6]!.GetValue<double>()} {a[7]!.GetValue<double>()} {fill}";
        }));
        static string Got(IEnumerable<(TileViewKey Key, TileView View)> entries) => string.Join("; ", entries.Select(e =>
            $"{e.Key.Layout} {e.Key.Tile} {e.Key.Camera.Device} {e.Key.Camera.Channel} {e.Key.Camera.Lens} {e.View.Zoom.Scale} {e.View.Zoom.CenterX} {e.View.Zoom.CenterY} {(e.View.Fill is { } f ? f.ToString() : "null")}"));

        T.Run("windows: Fill Tiles and the tiles' zoom, crop and Fill choices (shared vectors)", () =>
        {
            JsonNode vectors = JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "tile-views.json")))!;
            foreach (JsonNode? c in vectors["saved"]!.AsArray())
            {
                string name = c!["name"]!.GetValue<string>();
                List<SavedWindow> list = WindowStore.FromJson(new JsonArray(Window(c)));
                T.Equal(1, list.Count, name);
                SavedWindow w = list[0];
                T.Equal(c["fill"]!.GetValue<bool>(), w.Fill, name);
                T.Equal(Want(c), Got(w.Views?.Entries ?? []), name);
                T.Check(w.Legacy is null, $"{name}: nothing to migrate");
                // What is read back is written back the same.
                List<SavedWindow> again = WindowStore.FromJson(JsonNode.Parse(WindowStore.ToJson(list)));
                T.Equal(w with { Content = null! }, again[0] with { Content = null! }, $"{name}: round trip");
            }
            // A plain window writes none of the members.
            string plain = WindowStore.ToJson([new SavedWindow(new WindowContent.Layout("layout-1"), new Rect(0, 0, 800, 500))]);
            T.Check(!plain.Contains("\"fill\"", StringComparison.Ordinal) && !plain.Contains("\"tiles\"", StringComparison.Ordinal)
                && !plain.Contains("tileViews", StringComparison.Ordinal), "nothing written while default");
            // At most the limit, the newest kept.
            var many = new JsonArray();
            for (int i = 0; i < TileViews<TileViewKey>.Limit + 44; i++)
            {
                many.Add((JsonNode)new JsonObject { ["layout"] = "layout:layout-1", ["tile"] = $"t{i}", ["camera"] = new JsonObject { ["device"] = "nvr-a", ["channel"] = i }, ["scale"] = 2.0 });
            }
            var big = new JsonObject
            {
                ["content"] = new JsonObject { ["layout"] = new JsonObject { ["id"] = "layout-1" } },
                ["frame"] = new JsonObject { ["x"] = 0.0, ["y"] = 0.0, ["width"] = 800.0, ["height"] = 500.0 },
                ["tiles"] = many,
            };
            IReadOnlyList<(TileViewKey Key, TileView View)> kept = WindowStore.FromJson(new JsonArray(big))[0].Views!.Entries;
            T.Check(kept.Count == TileViews<TileViewKey>.Limit && kept[0].Key.Tile == "t44" && kept[^1].Key.Camera.Channel == TileViews<TileViewKey>.Limit + 43, "bounded, newest kept");
        });

        T.Run("windows: views kept per camera migrate to the tiles showing it (shared vectors)", () =>
        {
            JsonNode vectors = JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "tile-views.json")))!;
            foreach (JsonNode? c in vectors["migrate"]!.AsArray())
            {
                string name = c!["name"]!.GetValue<string>();
                SavedWindow w = WindowStore.FromJson(new JsonArray(Window(c)))[0];
                string layout = c["layout"]!.GetValue<string>();
                List<LayoutNode> leaves = [.. c["tiles"]!.AsArray().Select(t =>
                {
                    JsonArray a = t!.AsArray();
                    CameraRef? camera = a[1] is null ? null : new CameraRef(a[1]!.GetValue<string>(), a[2]!.GetValue<int>(), a[3]!.GetValue<int>());
                    return LayoutNode.Leaf(camera).SettingMember(NodePath.Root, TileIds.Member, JsonValue.Create(a[0]!.GetValue<string>())!.ToJsonString())!;
                })];
                LayoutNode tree = LayoutNode.Chain(SplitAxis.Row, leaves);
                TileViews<TileViewKey> views = w.PictureViews;
                if (w.Legacy is { } legacy)
                {
                    views.Migrate(legacy.Entries, layout, tree);
                }
                T.Equal(c["fill"]!.GetValue<bool>(), views.Fill, name);
                T.Equal(Want(c), Got(views.Saved), name);
                // Saved again, it is in the per-tile form only.
                string json = WindowStore.ToJson([w with { Views = SavedTileViews.Of(views), Legacy = null }]);
                T.Check(!json.Contains("tileViews", StringComparison.Ordinal), $"{name}: the old form is not written");
            }
        });

        T.Run("windows: tile identities and views per tile through swaps, camera changes, splits and removals", () =>
        {
            var a = new CameraRef("nvr-a", 0);
            var b = new CameraRef("nvr-a", 1);
            var two = new TileZoom(2, 0.3, 0.5);
            var three = new TileZoom(3, 0.6, 0.4);
            // Two tiles of one camera are independent.
            LayoutNode same = LayoutNode.Balanced(new[] { a, a, b }).WithTileIds();
            List<TileViewKey> keys = same.TileKeys("L");
            T.Equal(3, keys.Select(k => k.Tile).Distinct().Count(), "every tile has its own identity");
            var views = new TileViews<TileViewKey>();
            views.SetZoom(keys[0], two);
            views.SetZoom(keys[1], three);
            views.SetFill(keys[1], true);
            T.Check(views.Zoom(keys[0]) == two && !views.Fills(keys[0]) && views.Zoom(keys[1]) == three && views.Fills(keys[1]), "independent");
            T.Check(views.MixedIn("L") && !views.MixedIn("M"), "mixed per layout");

            // A swap moves each view with its camera; the tiles keep their identities.
            LayoutNode tree = LayoutNode.Balanced(new[] { a, b }).WithTileIds();
            NodePath p = tree.Leaves[0].Path, q = tree.Leaves[1].Path;
            views = new TileViews<TileViewKey>();
            views.SetZoom(tree.TileKeyAt(p, "L")!.Value, two);
            views.SetZoom(tree.TileKeyAt(q, "L")!.Value, three);
            views.SetZoom(new TileViewKey("M", "t0", a), three);
            views.SwapViews("L", tree, p, q);
            LayoutNode swapped = tree.Swapping(p, q)!;
            views.Prune("L", swapped);
            T.Check(swapped[p]!.TileId() == tree[p]!.TileId() && swapped[p]!.Camera == b, "the tiles keep their identities");
            T.Equal(three, views.Zoom(swapped.TileKeyAt(p, "L")!.Value), "b's view moved with it");
            T.Equal(two, views.Zoom(swapped.TileKeyAt(q, "L")!.Value), "a's view moved with it");

            // Another camera in a tile starts at the default; showing the first again does not bring it back.
            LayoutNode shown = swapped.Assigning(a, p)!.WithTileIds();
            views.Prune("L", shown);
            T.Equal(TileZoom.None, views.Zoom(shown.TileKeyAt(p, "L")!.Value), "a new camera starts at 1x");
            LayoutNode again = shown.Assigning(b, p)!;
            T.Equal(TileZoom.None, views.Zoom(again.TileKeyAt(p, "L")!.Value), "the old view is gone");
            T.Equal(three, views.Zoom(new TileViewKey("M", "t0", a)), "another layout's views are kept");

            // Identities survive splits and removals, and a stored layout reads them back.
            LayoutNode grid = LayoutNode.Balanced(new CameraRef?[] { a, b, a, null }).WithTileIds();
            var before = grid.Leaves.Select(l => grid[l.Path]!.TileId()).ToList();
            var split = grid.Splitting(grid.Leaves[0].Path, SplitAxis.Row)!.Value;
            LayoutNode named = split.Tree.WithTileIds();
            var after = named.Leaves.Select(l => named[l.Path]!.TileId()).ToList();
            T.Check(before.All(after.Contains) && named[split.NewLeaf]!.TileId() is { } fresh && !before.Contains(fresh), "a split keeps the identities and names the new tile");
            LayoutNode removed = named.Removing(named.Leaves[2].Path)!.Value.Tree;
            T.Check(removed.WithTileIds() == removed, "a removal keeps the others' identities");
            LayoutNode read = LayoutJson.ReadNode(JsonNode.Parse(LayoutJson.NodeToJson(removed))!);
            T.Check(read == removed, "stored and read back");

            // Renaming the content keeps the views.
            views = new TileViews<TileViewKey>();
            views.SetZoom(new TileViewKey(TileViewKey.ScratchKey, "t0", a), two);
            views.SetZoom(new TileViewKey("layout:x", "t0", a), three);
            views.Rename(TileViewKey.ScratchKey, "layout:x");
            T.Check(views.Count == 1 && views.Zoom(new TileViewKey("layout:x", "t0", a)) == two, "renamed");
        });

        T.Run("windows: placement keeps a reachable frame, else centers it", () =>
        {
            var screen = new Rect(0, 0, 1920, 1040);
            IReadOnlyList<Rect> one = [screen];
            var f = new Rect(100, 100, 900, 600);
            T.Equal(f, SavedWindow.Placement(f, one, 640, 400));
            var left = new Rect(-1920, 0, 1920, 1080);
            var there = new Rect(-1500, 200, 900, 600);
            T.Equal(there, SavedWindow.Placement(there, [screen, left], 640, 400));
            var edge = new Rect(1800, 100, 900, 600);
            T.Equal(edge, SavedWindow.Placement(edge, one, 640, 400), "enough of the title bar to grab");
            T.Equal(new Rect(510, 220, 900, 600), SavedWindow.Placement(there, one, 640, 400), "its display is gone");
            T.Equal(new Rect(510, 220, 900, 600), SavedWindow.Placement(new Rect(100, -590, 900, 600), one, 640, 400), "title bar above the screen");
            T.Equal(screen, SavedWindow.Placement(new Rect(-9000, 0, 3000, 2000), one, 640, 400), "shrunk to the screen");
            T.Equal(new Rect(100, 100, 640, 400), SavedWindow.Placement(new Rect(100, 100, 10, 10), one, 640, 400));
            T.Check(SavedWindow.Placement(f, [], 640, 400) is null);
            T.Check(SavedWindow.Placement(new Rect(double.NaN, 0, 900, 600), one, 640, 400) is { } p && double.IsFinite(p.X));
        });

        T.Run("groups: round trip, and the macOS app's file", () =>
        {
            var store = new GroupStore(Path.Combine(dir, "groups.json"));
            List<CameraGroup> groups = [new("group-1", "Group", [Fixtures.Cam(3), Fixtures.Cam(4, lens: 1)]), new("group-2", "Empty", [])];
            store.Save(groups);
            T.Check(store.Load().SequenceEqual(groups));
            // As the macOS app writes it (pretty, sorted keys).
            File.WriteAllText(Path.Combine(dir, "mac-groups.json"), """
                [
                  {
                    "cameras" : [
                      {
                        "channel" : 3,
                        "device" : "dev-a"
                      },
                      {
                        "channel" : 4,
                        "device" : "dev-a",
                        "lens" : 1
                      }
                    ],
                    "id" : "group-1",
                    "name" : "Group"
                  }
                ]
                """);
            T.Check(new GroupStore(Path.Combine(dir, "mac-groups.json")).Load().SequenceEqual(groups.Take(1)));
            T.Equal(0, new GroupStore(null).Load().Count);
        });

        T.Run("cameras: drag text", () =>
        {
            foreach (CameraRef c in new[] { Fixtures.Cam(0), Fixtures.Cam(12, "d-00ff", 1) })
            {
                T.Equal<CameraRef?>(c, CameraRefs.FromDragText(c.DragText()));
            }
            T.Equal("nvrview-camera:3.1@dev-a", Fixtures.Cam(3, lens: 1).DragText());
            foreach (string bad in new[] { "", "camera:1@x", "nvrview-camera:1@", "nvrview-camera:@x", "nvrview-camera:1.2@x", "nvrview-camera:a@x", "nvrview-camera:1.0.0@x" })
            {
                T.Check(CameraRefs.FromDragText(bad) is null, bad);
            }
            T.Equal("3 of 8", ChannelFilter.CountText(3, 8));
            T.Equal("8", ChannelFilter.CountText(8, 8));
            T.Equal(2, ChannelFilter.Visible([true, false, true], c => c, hideOffline: true).Count());
        });
    }
}
