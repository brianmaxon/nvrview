// The camera motion controls (docs/UX.md "Pan, tilt, and zoom"): a pad of four directions with a speed
// slider and, when the camera reports lens ranges, zoom and focus sliders below it; a flyout of the lens
// controls alone; and the presets menu (recall only). Every control is a thin shell over PtzModel; the
// self-test drives the same entry points the pointer does. The Mac app's PTZControls.swift is the reference.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>
/// A control that is held: down on pointer down, up on pointer up, when the pointer capture is lost
/// (the pointer left the window, the flyout closed), and when the control goes away with its flyout.
/// </summary>
internal sealed class HoldButton
{
    public HoldButton(string glyph, string label)
    {
        Button = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 14 } };
        AutomationProperties.SetName(Button, label);
        ToolTipService.SetToolTip(Button, label);
        // The button handles pointer events itself: listen to the handled ones too.
        Button.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, _) => PressDown()), true);
        Button.AddHandler(UIElement.PointerReleasedEvent, new PointerEventHandler((_, _) => PressUp()), true);
        Button.AddHandler(UIElement.PointerCanceledEvent, new PointerEventHandler((_, _) => PressUp()), true);
        Button.AddHandler(UIElement.PointerCaptureLostEvent, new PointerEventHandler((_, _) => PressUp()), true);
        Button.Unloaded += (_, _) => PressUp();
    }

    public Button Button { get; }

    public Action OnDown { get; set; } = () => { };

    public Action OnUp { get; set; } = () => { };

    public bool IsDown { get; private set; }

    /// <summary>Presses the control, as a pointer down does.</summary>
    public void PressDown()
    {
        if (IsDown)
        {
            return;
        }
        IsDown = true;
        OnDown();
    }

    /// <summary>Lets go of it, as a pointer up does.</summary>
    public void PressUp()
    {
        if (!IsDown)
        {
            return;
        }
        IsDown = false;
        OnUp();
    }
}

/// <summary>A lens axis: label, a step down, a slider across the camera's range, and a step up.</summary>
internal sealed class PtzLensRow
{
    public Grid Root { get; } = new();

    private readonly Slider _slider = new() { StepFrequency = 1, SmallChange = 1, LargeChange = 1, MinWidth = 90, HorizontalAlignment = HorizontalAlignment.Stretch };
    private bool _updating;
    private bool _pointerDown;

    public PtzLensRow(PtzLensAxis axis)
    {
        Axis = axis;
        string name = axis == PtzLensAxis.Zoom ? "Zoom" : "Focus";
        Root.ColumnSpacing = 6;
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(48) });
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var label = new TextBlock { Text = name, VerticalAlignment = VerticalAlignment.Center, FontSize = 12 };
        Minus = StepButton("", axis == PtzLensAxis.Zoom ? "Zoom out" : "Focus nearer", -1);
        Plus = StepButton("", axis == PtzLensAxis.Zoom ? "Zoom in" : "Focus farther", 1);
        AutomationProperties.SetName(_slider, name);
        _slider.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(Minus, 1);
        Grid.SetColumn(_slider, 2);
        Grid.SetColumn(Plus, 3);
        Root.Children.Add(label);
        Root.Children.Add(Minus);
        Root.Children.Add(_slider);
        Root.Children.Add(Plus);
        _slider.ValueChanged += (_, e) =>
        {
            if (_updating)
            {
                return;
            }
            OnDrag((int)Math.Round(e.NewValue));
            // A drag ends with the pointer; a key or a simulated move is a single step.
            if (!_pointerDown)
            {
                OnEnd();
            }
        };
        _slider.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, _) => _pointerDown = true), true);
        _slider.AddHandler(UIElement.PointerReleasedEvent, new PointerEventHandler((_, _) => EndPointer()), true);
        _slider.AddHandler(UIElement.PointerCanceledEvent, new PointerEventHandler((_, _) => EndPointer()), true);
        _slider.AddHandler(UIElement.PointerCaptureLostEvent, new PointerEventHandler((_, _) => EndPointer()), true);
    }

    public PtzLensAxis Axis { get; }

    public Button Minus { get; }

    public Button Plus { get; }

    public Slider Slider => _slider;

    public Action<int> OnStep { get; set; } = _ => { };

    public Action<int> OnDrag { get; set; } = _ => { };

    public Action OnEnd { get; set; } = () => { };

    private Button StepButton(string glyph, string label, int step)
    {
        var b = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 12 } };
        AutomationProperties.SetName(b, label);
        ToolTipService.SetToolTip(b, label);
        b.Click += (_, _) => OnStep(step);
        return b;
    }

    private void EndPointer()
    {
        if (_pointerDown)
        {
            _pointerDown = false;
            OnEnd();
        }
    }

    /// <summary>Shows a position (not while it is being dragged: the drag owns the slider).</summary>
    public void Update(PtzRange range, int position)
    {
        if (_pointerDown)
        {
            return;
        }
        _updating = true;
        _slider.Minimum = range.Min;
        _slider.Maximum = Math.Max(range.Max, range.Min + 1);
        _slider.Value = range.Clamped(position);
        _updating = false;
    }

    /// <summary>The slider dragged to <paramref name="position"/> (and let go, when <paramref name="ended"/>), as the pointer does.</summary>
    public void Drag(int position, bool ended)
    {
        _pointerDown = true;
        _slider.Value = position;
        _pointerDown = !ended;
        if (ended)
        {
            OnEnd();
        }
    }
}

/// <summary>Which controls the flyout shows.</summary>
internal enum PtzMode
{
    Pad,
    Lens,
}

/// <summary>
/// The flyout's content: the pad (<see cref="PtzMode.Pad"/>) or the lens sliders (<see cref="PtzMode.Lens"/>) for one
/// camera, with the camera's name and the latest refusal or failure.
/// </summary>
internal sealed class PtzFlyoutContent
{
    /// <summary>What the flyout shows.</summary>
    public StackPanel Root { get; } = new() { Spacing = 8, Width = ContentWidth };

    public const double ContentWidth = 240;

    private readonly PtzModel _ptz;
    private readonly string _name;
    private readonly TextBlock _status = new() { TextWrapping = TextWrapping.Wrap, FontSize = 12, Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xE5, 0x48, 0x4D)) };
    private readonly TextBlock _speedLabel = new() { FontSize = 12, VerticalAlignment = VerticalAlignment.Center, Width = 64 };

    public PtzFlyoutContent(CameraRef camera, PtzMode mode, string name, PtzModel ptz)
    {
        Camera = camera;
        Mode = mode;
        _name = name;
        _ptz = ptz;
        ptz.Changed += OnChanged;
        ptz.ClearProblem(camera);
        Build();
        ptz.WithInfo(camera, _ => Refresh());
    }

    public CameraRef Camera { get; }

    public PtzMode Mode { get; }

    public Dictionary<PtzDirection, HoldButton> Pad { get; } = [];

    public Dictionary<PtzLensAxis, PtzLensRow> LensRows { get; } = [];

    public Slider? SpeedSlider { get; private set; }

    /// <summary>The line the flyout shows for a refusal or failure (self-test).</summary>
    public string StatusText => _status.Text;

    /// <summary>The flyout goes away: nothing stays held.</summary>
    public void Closed()
    {
        _ptz.Changed -= OnChanged;
        foreach (HoldButton b in Pad.Values)
        {
            b.PressUp();
        }
        _ptz.Release();
    }

    private void OnChanged(PtzChange change) => Refresh();

    private void Build()
    {
        Root.Children.Clear();
        Pad.Clear();
        LensRows.Clear();
        SpeedSlider = null;
        Root.Children.Add(new TextBlock { Text = _name, FontSize = 13, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis });
        if (_ptz.InfoFor(Camera) is not { } info)
        {
            Root.Children.Add(new TextBlock { Text = "Reading the camera…", FontSize = 12 });
            Finish();
            return;
        }
        if (Mode == PtzMode.Pad)
        {
            if (info.PanTilt)
            {
                Root.Children.Add(BuildPad());
                Root.Children.Add(BuildSpeed(info));
            }
            else
            {
                Root.Children.Add(new TextBlock { Text = "This camera does not pan or tilt", FontSize = 12 });
            }
        }
        foreach (PtzLensAxis axis in Enum.GetValues<PtzLensAxis>())
        {
            if (info.Range(axis) is not { } range)
            {
                continue;
            }
            PtzLensAxis a = axis;
            var row = new PtzLensRow(a)
            {
                OnStep = direction => _ptz.StepLens(a, direction, Camera),
                OnDrag = p => _ptz.DragLens(a, p, Camera),
                OnEnd = () => _ptz.EndLensDrag(a, Camera),
            };
            row.Update(range, _ptz.LensPosition(Camera, a) ?? range.Cur);
            LensRows[a] = row;
            Root.Children.Add(row.Root);
        }
        if (Mode == PtzMode.Lens && LensRows.Count == 0)
        {
            Root.Children.Add(new TextBlock { Text = "This camera has no zoom or focus", FontSize = 12 });
        }
        Finish();
    }

    private Grid BuildPad()
    {
        var grid = new Grid { HorizontalAlignment = HorizontalAlignment.Center, RowSpacing = 4, ColumnSpacing = 4 };
        for (int i = 0; i < 3; i++)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(36) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(44) });
        }
        void Place(PtzDirection d, string glyph, string label, int col, int row)
        {
            var b = new HoldButton(glyph, label);
            b.Button.HorizontalAlignment = HorizontalAlignment.Stretch;
            b.Button.VerticalAlignment = VerticalAlignment.Stretch;
            b.OnDown = () => _ptz.Press(d, Camera);
            b.OnUp = () => _ptz.Release();
            Grid.SetColumn(b.Button, col);
            Grid.SetRow(b.Button, row);
            grid.Children.Add(b.Button);
            Pad[d] = b;
        }
        Place(PtzDirection.Up, "", "Tilt up", 1, 0);
        Place(PtzDirection.Left, "", "Pan left", 0, 1);
        Place(PtzDirection.Right, "", "Pan right", 2, 1);
        Place(PtzDirection.Down, "", "Tilt down", 1, 2);
        var stop = new Button { Content = new FontIcon { Glyph = "", FontSize = 14 }, HorizontalAlignment = HorizontalAlignment.Stretch, VerticalAlignment = VerticalAlignment.Stretch };
        AutomationProperties.SetName(stop, "Stop");
        ToolTipService.SetToolTip(stop, "Stop");
        stop.Click += (_, _) => _ptz.Release();
        Grid.SetColumn(stop, 1);
        Grid.SetRow(stop, 1);
        grid.Children.Add(stop);
        return grid;
    }

    private Grid BuildSpeed(PtzInfo info)
    {
        var grid = new Grid { ColumnSpacing = 6 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _speedLabel.Text = $"Speed {_ptz.Speed}";
        var slider = new Slider
        {
            Minimum = info.MinSpeed,
            Maximum = Math.Max(info.MaxSpeed, info.MinSpeed + 1),
            StepFrequency = 1,
            Value = Math.Clamp(_ptz.Speed, info.MinSpeed, Math.Max(info.MaxSpeed, info.MinSpeed + 1)),
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(slider, "Speed");
        slider.ValueChanged += (_, e) =>
        {
            _ptz.Speed = (int)Math.Round(e.NewValue);
            _speedLabel.Text = $"Speed {_ptz.Speed}";
        };
        Grid.SetColumn(slider, 1);
        grid.Children.Add(_speedLabel);
        grid.Children.Add(slider);
        SpeedSlider = slider;
        return grid;
    }

    private void Finish()
    {
        string text = _ptz.ProblemFor(Camera) ?? "";
        _status.Text = text;
        _status.Visibility = text.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        Root.Children.Add(_status);
    }

    /// <summary>The camera's state changed (capabilities read, a position, a refusal).</summary>
    private void Refresh()
    {
        bool haveRows = LensRows.Count > 0 || Pad.Count > 0;
        PtzInfo? info = _ptz.InfoFor(Camera);
        if (info is not null && (!haveRows || ShapeChanged(info)))
        {
            Build();
            return;
        }
        if (info is null && Root.Children.Count < 2)
        {
            Build();
            return;
        }
        foreach ((PtzLensAxis axis, PtzLensRow row) in LensRows)
        {
            if (info?.Range(axis) is { } range && _ptz.LensPosition(Camera, axis) is { } p)
            {
                row.Update(range, p);
            }
        }
        string text = _ptz.ProblemFor(Camera) ?? "";
        if (_status.Text != text)
        {
            _status.Text = text;
            _status.Visibility = text.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
        }
    }

    private bool ShapeChanged(PtzInfo info)
    {
        bool wantsPad = Mode == PtzMode.Pad && info.PanTilt;
        if (wantsPad != (Pad.Count > 0))
        {
            return true;
        }
        return Enum.GetValues<PtzLensAxis>().Any(a => (info.Range(a) is not null) != LensRows.ContainsKey(a));
    }
}

/// <summary>The presets menu of a camera: its list in the camera's order, the first nine with their Alt+digit hint; a choice recalls it. Recall only: saving or deleting presets is a device setting and is not offered.</summary>
internal static class PresetsMenu
{
    /// <summary>One item per preset, or a disabled "No presets".</summary>
    public static IEnumerable<MenuFlyoutItemBase> Items(PtzInfo info, CameraRef camera, PtzModel ptz)
    {
        if (info.Presets.Count == 0)
        {
            yield return new MenuFlyoutItem { Text = "No presets", IsEnabled = false };
            yield break;
        }
        for (int i = 0; i < info.Presets.Count; i++)
        {
            PtzPreset preset = info.Presets[i];
            var item = new MenuFlyoutItem { Text = preset.Name.Length == 0 ? $"Preset {i + 1}" : preset.Name };
            if (i < 9)
            {
                item.KeyboardAcceleratorTextOverride = $"Alt+{i + 1}";
            }
            item.Tag = preset;
            item.Click += (_, _) => Choose(item, camera, ptz);
            yield return item;
        }
    }

    /// <summary>What choosing a menu item does (the item's click, and the self-test): recalls its preset.</summary>
    public static void Choose(MenuFlyoutItem item, CameraRef camera, PtzModel ptz)
    {
        if (item.Tag is PtzPreset preset)
        {
            ptz.Recall(preset.Id, camera);
        }
    }

    public static MenuFlyout Menu(PtzInfo info, CameraRef camera, PtzModel ptz)
    {
        var menu = new MenuFlyout();
        foreach (MenuFlyoutItemBase i in Items(info, camera, ptz))
        {
            menu.Items.Add(i);
        }
        return menu;
    }
}
