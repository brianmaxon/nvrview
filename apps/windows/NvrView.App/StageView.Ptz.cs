// Camera motion on the stage (docs/UX.md "Pan, tilt, and zoom", "Capture and camera keys"): the pad and lens
// flyouts under the header buttons and the "…" menu's "Pan and Tilt…", the presets menu, and the Alt keys.
// These are the user's own controls and need no enable; a move is held by a control (or a key), renewed by
// the model while it is held, and stopped on release, window close, tile close and focus loss. The Mac
// app's StageView+PTZ.swift is the reference.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;

namespace NvrView.App;

internal sealed partial class StageView
{
    private Flyout? _ptzFlyout;

    /// <summary>The pad or lens flyout that is open, if one is (self-test).</summary>
    internal PtzFlyoutContent? PtzContent { get; private set; }

    /// <summary>Whether an Alt key has acted since Alt went down (the window then keeps Alt from opening the system menu).</summary>
    internal bool PtzAltUsed { get; set; }

    /// <summary>Shows the problems the model reports as a line over the tile (the flyout has its own line).</summary>
    private void InstallPtz() => _viewer.Ptz.Changed += OnPtzChanged;

    private void RemovePtz() => _viewer.Ptz.Changed -= OnPtzChanged;

    private void OnPtzChanged(PtzChange change)
    {
        if (change.ProblemFor is not { } camera || change.Problem is not { } text || PtzContent?.Camera == camera
            || !_tiles.Any(t => t.CameraOrNone == camera))
        {
            return;
        }
        ShowToast($"{_viewer.CameraName(camera)}: {text}");
    }

    /// <summary>Opens the pad or the lens flyout for the tile's camera, under <paramref name="from"/> (the header button) or at the tile's header.</summary>
    internal void ShowPtz(PtzMode mode, StageTile t, FrameworkElement? from)
    {
        if (t.CameraOrNone is not { } camera)
        {
            return;
        }
        ClosePtzFlyout();
        var content = new PtzFlyoutContent(camera, mode, _viewer.CameraName(camera), _viewer.Ptz);
        var flyout = new Flyout { Content = content, Placement = FlyoutPlacementMode.Bottom };
        flyout.Closed += (_, _) =>
        {
            if (ReferenceEquals(_ptzFlyout, flyout))
            {
                EndPtzFlyout();
            }
        };
        // Alt keys while the flyout has the keyboard focus (its popup is outside the window's key route).
        content.KeyDown += (_, e) =>
        {
            if (HandlePtzKey(e.Key, e.KeyStatus.ScanCode, e.KeyStatus.IsMenuKeyDown, repeat: e.KeyStatus.WasKeyDown))
            {
                e.Handled = true;
            }
        };
        content.KeyUp += (_, e) => HandlePtzKeyUp(e.Key);
        _ptzFlyout = flyout;
        PtzContent = content;
        if (from is not null)
        {
            flyout.ShowAt(from);
        }
        else
        {
            flyout.ShowAt(this, new FlyoutShowOptions
            {
                Position = new Windows.Foundation.Point(Math.Max(t.FrameX + t.FrameWidth / 2, t.FrameX), t.FrameY + TileChrome.HeaderHeight),
                Placement = FlyoutPlacementMode.Bottom,
            });
        }
    }

    /// <summary>Closes the flyout, and with it anything it holds.</summary>
    internal void ClosePtzFlyout()
    {
        if (_ptzFlyout is not { } flyout)
        {
            return;
        }
        flyout.Hide();
        EndPtzFlyout();
    }

    private void EndPtzFlyout()
    {
        PtzContent?.Closed();
        PtzContent = null;
        _ptzFlyout = null;
    }

    /// <summary>The presets menu of the tile's camera, once its list is read.</summary>
    internal void ShowPresets(StageTile t, FrameworkElement? from)
    {
        if (t.CameraOrNone is not { } camera)
        {
            return;
        }
        _viewer.Ptz.WithInfo(camera, info =>
        {
            if (info is null)
            {
                ShowToast($"{_viewer.CameraName(camera)}: the presets could not be read");
                return;
            }
            MenuFlyout menu = PresetsMenu.Menu(info, camera, _viewer.Ptz);
            PopUp(menu, from ?? this, t);
        });
    }

    /// <summary>The "…" and right-click menus' Presets item, from the list read so far (reading it when it has not been, and filling in when it arrives).</summary>
    internal MenuFlyoutSubItem PresetsSubItem(CameraRef camera, string label)
    {
        var sub = new MenuFlyoutSubItem { Text = label };
        void Fill(PtzInfo info)
        {
            sub.Items.Clear();
            foreach (MenuFlyoutItemBase i in PresetsMenu.Items(info, camera, _viewer.Ptz))
            {
                sub.Items.Add(i);
            }
        }
        if (_viewer.Ptz.InfoFor(camera) is { } known)
        {
            Fill(known);
            return sub;
        }
        sub.Items.Add(new MenuFlyoutItem { Text = "Reading the presets…", IsEnabled = false });
        void Arrived(PtzChange change)
        {
            if (_viewer.Ptz.InfoFor(camera) is { } info)
            {
                _viewer.Ptz.Changed -= Arrived;
                Fill(info);
            }
        }
        _viewer.Ptz.Changed += Arrived;
        _viewer.Ptz.WithInfo(camera, info =>
        {
            if (info is null)
            {
                _viewer.Ptz.Changed -= Arrived;
                sub.Items.Clear();
                sub.Items.Add(new MenuFlyoutItem { Text = "The presets could not be read", IsEnabled = false });
            }
        });
        return sub;
    }

    // ---- keys (docs/UX.md "Capture and camera keys") ----

    /// <summary>
    /// An Alt key for the selected tile's camera (<paramref name="alt"/>: Alt is down). False when the key is not one or the tile has
    /// no such control (the key goes on).
    /// </summary>
    public bool HandlePtzKey(VirtualKey key, uint scanCode, bool alt, bool repeat)
    {
        static bool Down(VirtualKey k) => Microsoft.UI.Input.InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);
        if (PtzKeys.Map((int)key, (int)scanCode, alt, Down(VirtualKey.Control), Down(VirtualKey.Shift), repeat) is not { } mapped)
        {
            return false;
        }
        StageTile? s = IsFocused && Renderer.Focus is { } focus ? focus.Tile : Selected;
        if (s is not { IsEmpty: false } || HeaderTileOf(s) is not { } tile)
        {
            return false;
        }
        CameraRef camera = s.Camera;
        Interact();
        switch (mapped.Kind)
        {
            case PtzKeyKind.Move:
                if (!tile.PanTilt)
                {
                    return false;
                }
                _viewer.Ptz.KeyDown(mapped.Direction, camera);
                break;
            case PtzKeyKind.Zoom:
                if (!tile.Lens)
                {
                    return false;
                }
                _viewer.Ptz.WithInfo(camera, info =>
                {
                    if (info is null)
                    {
                        ShowToast($"{_viewer.CameraName(camera)}: the lens could not be read");
                    }
                    _viewer.Ptz.ZoomStep(mapped.Value, camera);
                });
                break;
            default:
                if (!tile.Presets)
                {
                    return false;
                }
                _viewer.Ptz.WithInfo(camera, info =>
                {
                    if (info is not null && !_viewer.Ptz.RecallNth(mapped.Value, camera))
                    {
                        ShowToast($"{_viewer.CameraName(camera)}: there is no preset {mapped.Value}");
                    }
                });
                break;
        }
        PtzAltUsed = true;
        return true;
    }

    /// <summary>An arrow key came up: the move it held ends.</summary>
    public void HandlePtzKeyUp(VirtualKey key)
    {
        if (PtzKeys.ArrowOf((int)key) is { } d)
        {
            _viewer.Ptz.KeyUp(d);
        }
    }

    /// <summary>Alt came up: a move an arrow key still holds ends. True if an Alt key was used since Alt went down (the window then keeps Alt from opening the system menu).</summary>
    public bool HandlePtzAltUp()
    {
        _viewer.Ptz.ModifiersChanged(altDown: false);
        bool used = PtzAltUsed;
        PtzAltUsed = false;
        return used;
    }
}
