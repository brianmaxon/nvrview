using NvrView.Tiles;

namespace NvrView.Shell.Tests;

/// <summary>The All Cameras selection and its refresh rule. Pure logic: no clocks.</summary>
internal static class AllCamerasTests
{
    private static AllCamerasChannel[] Channels(int[] online, int[]? offline = null, int lensChannel = -1, int lenses = 1) =>
        [.. online.Select(c => new AllCamerasChannel(c, true, c == lensChannel ? lenses : 1))
            .Concat((offline ?? []).Select(c => new AllCamerasChannel(c, false, c == lensChannel ? lenses : 1)))
            .OrderBy(c => c.Channel)];

    private static AllCamerasDevice Dev(string key, AllCamerasChannel[] channels, bool connected = true) => new(key, connected, channels);

    private static CameraRef Cam(string d, int c, int lens = 0) => new(d, c, lens);

    public static void Run()
    {
        T.Run("all cameras: devices in order, each with its online channels", () =>
        {
            List<CameraRef> list = AllCameras.Select([Dev("a", Channels([0, 1])), Dev("b", Channels([0, 2]))]);
            T.Equal(string.Join(",", new[] { Cam("a", 0), Cam("a", 1), Cam("b", 0), Cam("b", 2) }), string.Join(",", list));
        });

        T.Run("all cameras: a full first device does not crowd out the next", () =>
        {
            List<CameraRef> list = AllCameras.Select([Dev("a", Channels([.. Enumerable.Range(0, 16)])), Dev("b", Channels([0, 1]))]);
            T.Equal(18, list.Count);
            T.Equal(Cam("b", 1), list[^1]);
            T.Equal(Cam("b", 0), list[^2]);
        });

        T.Run("all cameras: offline channels are left out", () =>
        {
            List<CameraRef> list = AllCameras.Select([Dev("a", Channels([0, 2], offline: [1, 3]))]);
            T.Equal(string.Join(",", new[] { Cam("a", 0), Cam("a", 2) }), string.Join(",", list));
        });

        T.Run("all cameras: devices not connected are left out until they connect", () =>
        {
            List<CameraRef> waiting = AllCameras.Select([Dev("a", Channels([0])), Dev("b", Channels([0, 1]), connected: false), Dev("c", [])]);
            T.Equal(string.Join(",", new[] { Cam("a", 0) }), string.Join(",", waiting));
            List<CameraRef> later = AllCameras.Select([Dev("a", Channels([0])), Dev("b", Channels([0, 1])), Dev("c", Channels([0]))]);
            T.Equal(4, later.Count);
            T.Equal(Cam("c", 0), later[^1]);
        });

        T.Run("all cameras: every lens of an online channel", () =>
        {
            List<CameraRef> list = AllCameras.Select([Dev("a", Channels([0, 1], lensChannel: 1, lenses: 2))]);
            T.Equal(string.Join(",", new[] { Cam("a", 0), Cam("a", 1), Cam("a", 1, 1) }), string.Join(",", list));
            T.Equal(0, AllCameras.Select([Dev("a", Channels([], offline: [1], lensChannel: 1, lenses: 2))]).Count);
        });

        T.Run("all cameras: stops at the layout limit", () =>
        {
            T.Equal(64, LayoutNode.MaxLeaves);
            AllCamerasDevice[] devices = [.. Enumerable.Range(0, 5).Select(i => Dev($"d{i}", Channels([.. Enumerable.Range(0, 16)])))];
            List<CameraRef> list = AllCameras.Select(devices);
            T.Equal(64, list.Count);
            T.Equal(Cam("d3", 15), list[^1]);
            T.Equal(64, AllCameras.Select([Dev("a", Channels([.. Enumerable.Range(0, 64)], lensChannel: 63, lenses: 2))]).Count);
            T.Equal(3, AllCameras.Select(devices, 3).Count);
            T.Equal(64, LayoutNode.Balanced(list).LeafCount);
        });

        T.Run("all cameras: nothing online is an empty list", () =>
        {
            T.Equal(0, AllCameras.Select([]).Count);
            T.Equal(0, AllCameras.Select([Dev("a", Channels([0]), connected: false)]).Count);
        });

        T.Run("all cameras: an unedited window rebuilds only when the list changes", () =>
        {
            var watch = new AllCamerasWatch([]);
            T.Check(!watch.Refresh([]));
            CameraRef[] one = [Cam("a", 0)];
            T.Check(watch.Refresh(one));
            T.Check(!watch.Refresh(one));
            T.Check(!watch.Refresh([Cam("a", 0)]), "an equal list built again");
            CameraRef[] two = [Cam("a", 0), Cam("b", 0)];
            T.Check(watch.Refresh(two));
            T.Check(!watch.Refresh(two));
            T.Check(watch.Refresh([Cam("b", 0)]));
            T.Check(watch.Refresh([]));
        });

        T.Run("all cameras: an edited window is never refreshed", () =>
        {
            var watch = new AllCamerasWatch([Cam("a", 0)]);
            watch.MarkEdited();
            T.Check(!watch.Refresh([Cam("a", 0), Cam("b", 0)]));
            T.Check(!watch.Refresh([]));
            T.Equal(1, watch.Shown.Count);
        });
    }
}
