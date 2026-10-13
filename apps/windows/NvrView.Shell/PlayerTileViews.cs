// The recording player's picture settings (docs/UX.md "Recordings", "Digital zoom"): per camera in the
// player, its digital zoom (or a filled picture's crop), its own Fill choice, and its fisheye view and the
// virtual camera's aim. The rules are the live tiles' own (TileViews, TileZoom, FisheyeAim in NvrView.Tiles);
// this adds only what a player needs on top:
//
// - The state belongs to the player's tile, not to the playback: play, pause, seek, speed changes, another
//   incident of the same cameras and the switch between the grid and the large or single view all keep it.
// - A camera that leaves the player (another set of cameras, Remove from Incident) forgets its view
//   (Retain), so a tile whose camera changed starts at 1x, centered, in the Original fisheye view.
// - It never reaches the device, a download, or an export: it is state of the picture on screen, and
//   nothing else reads it.
//
// The same type as the macOS app's PlayerTileViews (NVRCore), with the same tests.

using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>The recording player's picture settings per camera (see the file comment).</summary>
public sealed class PlayerTileViews
{
    private readonly TileViews<RecordingCamera> _views = new();
    private readonly Dictionary<RecordingCamera, FisheyeTileSetting> _fisheye = [];

    /// <summary>Nothing is kept.</summary>
    public bool IsEmpty => _views.Count == 0 && _fisheye.Count == 0;

    // ---- digital zoom and Fill ----

    /// <summary>The camera's zoom (1x and centered when nothing is kept).</summary>
    public TileZoom Zoom(RecordingCamera camera) => _views.Zoom(camera);

    /// <summary>Whether the camera's picture fills its tile (its own choice; the player has no window-wide switch).</summary>
    public bool Fills(RecordingCamera camera) => _views.Fills(camera);

    /// <summary>Sets the camera's zoom or crop.</summary>
    public void SetZoom(RecordingCamera camera, TileZoom zoom) => _views.SetZoom(camera, zoom);

    /// <summary>
    /// Zooms by <paramref name="factor"/> around the unit point (<paramref name="ux"/>, <paramref name="uy"/>) of
    /// the tile, which stays put (the picture sits in the tile as <paramref name="b"/> says).
    /// </summary>
    public void ZoomAround(RecordingCamera camera, double ux, double uy, double factor, ZoomBase b) =>
        _views.SetZoom(camera, _views.Zoom(camera).ZoomedBy(factor, ux, uy, b));

    /// <summary>Whether a drag moves the picture (zoomed in, or filled with part hidden).</summary>
    public bool CanPan(RecordingCamera camera, ZoomBase b) => _views.Zoom(camera).CanPan(b);

    /// <summary>The picture follows a drag of (<paramref name="dx"/>, <paramref name="dy"/>) in units of the tile.</summary>
    public void Pan(RecordingCamera camera, double dx, double dy, ZoomBase b)
    {
        if (CanPan(camera, b))
        {
            _views.SetZoom(camera, _views.Zoom(camera).Panned(dx, dy, b));
        }
    }

    /// <summary>0: back to 1x and a filled crop to the middle; a dewarped fisheye view back to its default direction and field of view.</summary>
    public void Reset(RecordingCamera camera)
    {
        if (FisheyeDewarps(camera))
        {
            ResetFisheye(camera);
        }
        else
        {
            _views.SetZoom(camera, TileZoom.None);
        }
    }

    /// <summary>The camera's tile fills, or fits.</summary>
    public void SetFill(RecordingCamera camera, bool fills) => _views.SetFill(camera, fills);

    // ---- fisheye ----

    /// <summary>The camera's fisheye view.</summary>
    public FisheyeTileSetting Fisheye(RecordingCamera camera) => _fisheye.GetValueOrDefault(camera, FisheyeTileSetting.Original);

    /// <summary>Whether the camera shows a dewarped view now.</summary>
    public bool FisheyeDewarps(RecordingCamera camera) => Fisheye(camera).Mode.Dewarps();

    /// <summary>Chooses the camera's view (the aim is kept).</summary>
    public void SetFisheyeMode(RecordingCamera camera, FisheyeViewMode mode) =>
        Put(camera, new FisheyeTileSetting(mode, _fisheye.TryGetValue(camera, out FisheyeTileSetting kept) ? kept.Aim : null));

    /// <summary>A drag of (<paramref name="dx"/>, <paramref name="dy"/>) points in a view <paramref name="w"/> x <paramref name="h"/> points big turns the view.</summary>
    public void TurnFisheye(RecordingCamera camera, double dx, double dy, double w, double h, FisheyeMount mount)
    {
        FisheyeTileSetting s = Fisheye(camera);
        if (s.Mode.Dewarps())
        {
            Put(camera, new FisheyeTileSetting(s.Mode, s.AimFor(mount).Dragged(dx, dy, w, h, s.Mode, mount)));
        }
    }

    /// <summary>The wheel or a pinch: Virtual PTZ zooms (more than 1 zooms in).</summary>
    public void ZoomFisheye(RecordingCamera camera, double factor, FisheyeMount mount)
    {
        FisheyeTileSetting s = Fisheye(camera);
        if (s.Mode == FisheyeViewMode.Ptz)
        {
            Put(camera, new FisheyeTileSetting(s.Mode, s.AimFor(mount).Zoomed(factor, mount)));
        }
    }

    /// <summary>Sets the virtual camera's aim as it is (a drag or an easing step).</summary>
    public void SetAim(RecordingCamera camera, FisheyeAim aim)
    {
        FisheyeTileSetting s = Fisheye(camera);
        if (s.Mode.Dewarps())
        {
            Put(camera, new FisheyeTileSetting(s.Mode, aim));
        }
    }

    /// <summary>Back to the mount's default direction and field of view.</summary>
    public void ResetFisheye(RecordingCamera camera) => Put(camera, new FisheyeTileSetting(Fisheye(camera).Mode, null));

    private void Put(RecordingCamera camera, FisheyeTileSetting setting)
    {
        if (setting == FisheyeTileSetting.Original)
        {
            _fisheye.Remove(camera);
        }
        else
        {
            _fisheye[camera] = setting;
        }
    }

    // ---- which cameras ----

    /// <summary>Keeps the views of <paramref name="cameras"/> only: a camera that is no longer in the player starts again at 1x when it comes back.</summary>
    public void Retain(IEnumerable<RecordingCamera> cameras)
    {
        var keep = cameras.ToHashSet();
        _views.RemoveAll(c => !keep.Contains(c));
        foreach (RecordingCamera gone in _fisheye.Keys.Where(c => !keep.Contains(c)).ToList())
        {
            _fisheye.Remove(gone);
        }
    }

    // ---- what the header offers ----

    /// <summary>Zoomed past 1x, or a moved virtual camera (Reset Zoom applies).</summary>
    public bool IsZoomed(RecordingCamera camera) => FisheyeDewarps(camera) ? Fisheye(camera).Aim is not null : _views.Zoom(camera).IsZoomed;

    /// <summary>A filled picture's crop moved at 1x (Reset Position applies).</summary>
    public bool IsPanned(RecordingCamera camera) => !FisheyeDewarps(camera) && _views.Fills(camera) && _views.Zoom(camera).IsPanned;
}
