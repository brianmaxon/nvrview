// Camera motion in the viewer (docs/UX.md "Pan, tilt, and zoom"): the model over the core, and the
// per-camera grants for automation (camera motion control: off until the user allows it in Settings;
// the user's own pad, presets and lens controls need none).

using Microsoft.UI.Dispatching;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>The core's camera motion calls for <see cref="PtzModel"/>.</summary>
internal sealed class NvCorePtz(Func<NvCore> core) : IPtzCore
{
    public string Info(ulong tile) => core().PtzInfo(tile);

    public void Move(ulong tile, PtzDirection direction, int speed) => core().PtzMove(tile, PtzNames.Of(direction), (uint)Math.Clamp(speed, 1, 64));

    public void Stop(ulong tile) => core().PtzStop(tile);

    public void Lens(ulong tile, PtzLensAxis axis, int position) => core().PtzLens(tile, PtzNames.Of(axis), position);

    public void Preset(ulong tile, int id) => core().PtzPreset(tile, (uint)id);
}

internal sealed partial class Viewer
{
    /// <summary>What each camera can do, the held move, preset recall and lens positions.</summary>
    public PtzModel Ptz { get; private set; } = null!;

    private DispatcherQueueTimer? _ptzTimer;

    private void InitPtz()
    {
        DispatcherQueue? ui = DispatcherQueue.GetForCurrentThread();
        Ptz = new PtzModel(
            new NvCorePtz(() => Core),
            Clock,
            camera => Leases.Tile(camera) is { } t ? (ulong?)t.Id : null,
            id => Tiles.FirstOrDefault(t => t.Id == id)?.Camera,
            speed: Settings.PtzSpeed,
            saveSpeed: s => Settings.PtzSpeed = s,
            // The capabilities read waits for the camera (up to 20 s): not on the UI thread.
            background: work => _ = Task.Run(work),
            post: work =>
            {
                if (ui is null)
                {
                    work();
                }
                else
                {
                    ui.TryEnqueue(() => work());
                }
            },
            // The stage's tick renews a held move too; this timer keeps it alive when the stage is not refreshing
            // (a window behind others): twice per renewal interval.
            backupTimer: on =>
            {
                if (ui is null)
                {
                    return;
                }
                if (_ptzTimer is null)
                {
                    _ptzTimer = ui.CreateTimer();
                    _ptzTimer.Interval = TimeSpan.FromSeconds(PtzHold.RenewInterval / 2);
                    _ptzTimer.IsRepeating = true;
                    _ptzTimer.Tick += (_, _) => Ptz.Tick();
                }
                if (on)
                {
                    _ptzTimer.Start();
                }
                else
                {
                    _ptzTimer.Stop();
                }
            });
        // A tile that closes takes its held move, readings and lens positions with it.
        Leases.TileChanged += (camera, tile) =>
        {
            if (tile is null)
            {
                Ptz.CameraClosed(camera);
            }
        };
    }

    /// <summary>Whether a camera can be moved at all (either lens), for Settings.</summary>
    public bool CanMove(CameraRef camera)
    {
        if (ChannelOf(camera.ChannelKey) is not { } info)
        {
            return false;
        }
        for (int lens = 0; lens < Math.Max(info.Lenses, 1); lens++)
        {
            PtzCaps caps = info.PtzOf(lens);
            if (caps.PanTilt || caps.Presets)
            {
                return true;
            }
        }
        return false;
    }

    /// <summary>
    /// Lets (or stops letting) an automation move a camera: <c>auto_correction</c> or a local API client's name.
    /// Camera motion control: off for every camera until the user allows it here; passed to the core at once and
    /// after every connection. The user's own controls never need it.
    /// </summary>
    public void SetMotionGrant(CameraRef camera, string client, bool granted)
    {
        camera = camera.ChannelKey;
        Settings.SetMotionGrant(camera, client, granted);
        AppLog.Info($"Motion control {(granted ? "allowed" : "turned off")} for {client}: channel {camera.Channel + 1}");
        if (FindDevice(camera.Device) is { Id: not 0 } device)
        {
            PassMotionGrant(device, camera.Channel, client, granted);
        }
    }

    private void PassMotionGrant(DeviceEntry device, int channel, string client, bool granted)
    {
        if (channel is < 0 or > 255)
        {
            return;
        }
        try
        {
            Core.MotionGrant(device.Id, (byte)channel, client, granted);
        }
        catch (NvException e)
        {
            AppLog.Warn($"motion grant for channel {channel + 1} failed: {e.Status}");
        }
    }

    /// <summary>Passes the user's automation grants for a device's cameras to the core (it keeps them for this run only).</summary>
    private void ApplyMotionGrants(DeviceEntry device)
    {
        if (device.Id == 0)
        {
            return;
        }
        foreach ((int channel, string client) in Settings.MotionGrantsFor(device.Key))
        {
            PassMotionGrant(device, channel, client, true);
        }
    }
}
