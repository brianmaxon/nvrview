// The overlay drawn above one tile's video (docs/UX.md, "Chrome" and
// "Listening from any tile"): a header with the speaker button and the
// camera's name that fades in on hover and out a few seconds later, a thin
// level meter along the top (green while the tile is audible and always
// shown then; white on gray while hovering a muted tile, fading with the
// header), the selection outline, a status line in words (connecting,
// reconnecting with its countdown, waiting for the camera, login failed,
// needs the HEVC Video Extension, ...), detection badges, and an optional
// diagnostics line, the corner indicators (IndicatorStrip: always shown, also
// in full screen), the sound badges ("Smoke alarm · sound"), and a raised
// alert (docs/UX.md "Alerts"): a red border that blinks about once a second
// (steady without Windows' "Show animations"), and a header strip, pinned
// visible, with the reason, first and last time, count, and a Dismiss button.
// Plain XAML elements above the swap chain; positions and meters are updated
// once per frame without allocating; everything else on events and the slow
// tick.

using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Xaml.Shapes;
using NvrView.Tiles;
using Windows.UI;

namespace NvrView.App;

internal sealed class TileChrome
{
    public const double HeaderHeight = 32;
    private const double MeterHeight = 3;

    private static readonly SolidColorBrush HeaderBrush = new(Color.FromArgb(0x99, 0, 0, 0));
    private static readonly SolidColorBrush ListeningBrush = new(Color.FromArgb(0xD0, 0x34, 0xC7, 0x59));
    private static readonly SolidColorBrush MutedMeterBrush = new(Color.FromArgb(0xE0, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush TrackBrush = new(Color.FromArgb(0x80, 0x80, 0x80, 0x80));
    private static readonly SolidColorBrush StatusBrush = new(Color.FromArgb(0xB0, 0, 0, 0));
    private static readonly SolidColorBrush BadgeBrush = new(Color.FromArgb(0xB0, 0x20, 0x20, 0x20));
    // A calm slate: a sound badge says what was heard, the alert says it is urgent.
    private static readonly SolidColorBrush SoundBadgeBrush = new(Color.FromArgb(0xE0, 0x3C, 0x4A, 0x5C));
    private static readonly SolidColorBrush AlertRed = new(Color.FromArgb(0xFF, 0xE0, 0x24, 0x24));
    private static readonly SolidColorBrush AlertStripBrush = new(Color.FromArgb(0xE0, 0x8C, 0x0D, 0x0D));
    private static readonly SolidColorBrush White = new(Colors.White);
    private static readonly SolidColorBrush OutlineBrush = new(Color.FromArgb(0xE0, 0x3A, 0x96, 0xDD));

    private readonly FontIcon _speakerIcon;
    private readonly TextBlock _title;
    private readonly Rectangle _track;
    private readonly Rectangle _bar;
    private readonly Border _statusBox;
    private readonly Button _retry;
    private readonly TextBlock _status;
    private readonly StackPanel _badges;
    private readonly TextBlock _diagnostics;
    private readonly Border _outline;
    private readonly Grid _header;
    private readonly Button _reset;
    private readonly Border _map;
    private readonly Border _mapRegion;
    private readonly IndicatorStrip _indicators = new();
    private readonly Border _alertBorder;
    private readonly Grid _alertStrip;
    private readonly TextBlock _alertText;
    private readonly Storyboard _blink = new();
    private float _x = float.NaN, _y, _w, _h;
    private bool _audible;
    private string _badgeKey = "";
    private ChromeCorner _corner = ChromeCorners.Default;
    private string? _alertKey;
    private bool _blinking;
    private bool _zoomed, _mapShown;
    private double _reserve;
    private double _mapX = double.NaN, _mapY, _mapW, _mapH;
    private UnitRect _mapVisible;

    /// <summary>Height of the pinned alert strip (four lines and padding).</summary>
    public const double AlertStripHeight = 4 * 15 + 14;

    public TileChrome(StageTile tile, Action<StageTile> speakerClicked)
    {
        Tile = tile;
        Root = new Grid { IsHitTestVisible = true, Background = null };

        _outline = new Border
        {
            BorderBrush = OutlineBrush,
            BorderThickness = new Thickness(2),
            Opacity = 0,
            IsHitTestVisible = false,
            OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(200) },
        };
        Root.Children.Add(_outline);

        _alertBorder = new Border
        {
            BorderBrush = AlertRed,
            BorderThickness = new Thickness(4),
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        Root.Children.Add(_alertBorder);
        var blink = new DoubleAnimationUsingKeyFrames { Duration = new Duration(TimeSpan.FromSeconds(AlertBorder.BlinkPeriod)), RepeatBehavior = RepeatBehavior.Forever };
        blink.KeyFrames.Add(new DiscreteDoubleKeyFrame { KeyTime = KeyTime.FromTimeSpan(TimeSpan.Zero), Value = 1 });
        blink.KeyFrames.Add(new DiscreteDoubleKeyFrame { KeyTime = KeyTime.FromTimeSpan(TimeSpan.FromSeconds(AlertBorder.BlinkPeriod / 2)), Value = 0 });
        Storyboard.SetTarget(blink, _alertBorder);
        Storyboard.SetTargetProperty(blink, "Opacity");
        _blink.Children.Add(blink);

        _header = new Grid
        {
            Height = HeaderHeight,
            VerticalAlignment = VerticalAlignment.Top,
            Background = HeaderBrush,
            Opacity = 0,
            OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(200) },
        };
        _header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _speakerIcon = new FontIcon { Glyph = "", FontSize = 14, Foreground = White };
        Speaker = new Button
        {
            Content = _speakerIcon,
            Background = new SolidColorBrush(Colors.Transparent),
            BorderThickness = new Thickness(0),
            Padding = new Thickness(8, 4, 8, 4),
            VerticalAlignment = VerticalAlignment.Center,
        };
        ToolTipService.SetToolTip(Speaker, "Listen (Ctrl+click to add)");
        Speaker.Click += (_, _) => speakerClicked(Tile);
        _header.Children.Add(Speaker);
        _title = new TextBlock
        {
            Foreground = White,
            FontSize = 13,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Margin = new Thickness(2, 0, 8, 0),
        };
        Grid.SetColumn(_title, 1);
        _header.Children.Add(_title);
        // The reset-zoom control (docs/UX.md "Digital zoom"): shown while the tile is zoomed, or its virtual camera moved.
        _reset = new Button
        {
            Content = new FontIcon { Glyph = "\uE71E", FontSize = 14, Foreground = White },
            Background = new SolidColorBrush(Colors.Transparent),
            BorderThickness = new Thickness(0),
            Padding = new Thickness(8, 4, 8, 4),
            VerticalAlignment = VerticalAlignment.Center,
            Visibility = Visibility.Collapsed,
        };
        ToolTipService.SetToolTip(_reset, "Reset zoom (0)");
        AutomationProperties.SetName(_reset, "Reset zoom");
        _reset.Click += (_, _) => ResetClicked?.Invoke(Tile);
        Grid.SetColumn(_reset, 2);
        _header.Children.Add(_reset);
        Root.Children.Add(_header);

        // The zoom map: the whole picture's outline with the visible region marked, in the bottom left of
        // the zoomed picture (it can lie outside the 1x picture's rectangle, so it is moved, not clipped).
        _mapRegion = new Border
        {
            Background = new SolidColorBrush(Color.FromArgb(0x2E, 0xFF, 0xFF, 0xFF)),
            BorderBrush = White,
            BorderThickness = new Thickness(1.5),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
        };
        _map = new Border
        {
            Background = new SolidColorBrush(Color.FromArgb(0x73, 0, 0, 0)),
            BorderBrush = new SolidColorBrush(Color.FromArgb(0x8C, 0xFF, 0xFF, 0xFF)),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(2),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
            IsHitTestVisible = false,
            Opacity = 0,
            Visibility = Visibility.Collapsed,
            Child = new Canvas { IsHitTestVisible = false, Children = { _mapRegion } },
            OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(200) },
        };
        AutomationProperties.SetName(_map, "Zoom map");
        Root.Children.Add(_map);

        _alertText = new TextBlock
        {
            Foreground = White,
            FontSize = 12,
            LineHeight = 15,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
            IsTextSelectionEnabled = false,
        };
        var dismiss = new Button { Content = "Dismiss", VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 8, 0), Padding = new Thickness(10, 3, 10, 3) };
        ToolTipService.SetToolTip(dismiss, "Dismiss this alert (Enter)");
        dismiss.Click += (_, _) => DismissClicked?.Invoke(Tile);
        _alertStrip = new Grid
        {
            Height = AlertStripHeight - 8,
            Margin = new Thickness(4),
            VerticalAlignment = VerticalAlignment.Top,
            Background = AlertStripBrush,
            CornerRadius = new CornerRadius(6),
            Visibility = Visibility.Collapsed,
        };
        _alertStrip.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _alertStrip.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _alertText.Margin = new Thickness(10, 0, 0, 0);
        _alertStrip.Children.Add(_alertText);
        Grid.SetColumn(dismiss, 1);
        _alertStrip.Children.Add(dismiss);
        Root.Children.Add(_alertStrip);

        _track = new Rectangle
        {
            Height = MeterHeight,
            VerticalAlignment = VerticalAlignment.Top,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Fill = TrackBrush,
            Opacity = 0,
            IsHitTestVisible = false,
        };
        _bar = new Rectangle
        {
            Height = MeterHeight,
            Width = 0,
            VerticalAlignment = VerticalAlignment.Top,
            HorizontalAlignment = HorizontalAlignment.Left,
            Fill = MutedMeterBrush,
            Opacity = 0,
            IsHitTestVisible = false,
        };
        Root.Children.Add(_track);
        Root.Children.Add(_bar);

        _status = new TextBlock
        {
            Foreground = White,
            FontSize = 14,
            TextWrapping = TextWrapping.Wrap,
            TextAlignment = TextAlignment.Center,
        };
        _statusBox = new Border
        {
            Background = StatusBrush,
            CornerRadius = new CornerRadius(6),
            Padding = new Thickness(12, 6, 12, 6),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(12, 12, 12, 6),
            Child = _status,
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        // Under the message, after a long time without a picture: a small bordered text button.
        _retry = new Button
        {
            Content = "Retry",
            FontSize = 12,
            Padding = new Thickness(12, 3, 12, 3),
            HorizontalAlignment = HorizontalAlignment.Center,
            Margin = new Thickness(0, 0, 0, 12),
            Background = StatusBrush,
            Foreground = White,
            BorderBrush = White,
            BorderThickness = new Thickness(1),
            Visibility = Visibility.Collapsed,
        };
        ToolTipService.SetToolTip(_retry, "Try this camera again now");
        AutomationProperties.SetName(_retry, "Retry");
        AutomationProperties.SetHelpText(_retry, "Tries this camera's stream again now");
        _retry.Click += (_, _) => RetryClicked?.Invoke(Tile);
        // The box has no background of its own: only the message and the button take clicks.
        var statusStack = new StackPanel
        {
            Orientation = Orientation.Vertical,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        statusStack.Children.Add(_statusBox);
        statusStack.Children.Add(_retry);
        Root.Children.Add(statusStack);

        _badges = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 4,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Bottom,
            Margin = new Thickness(6),
            IsHitTestVisible = false,
        };
        Root.Children.Add(_badges);

        _diagnostics = new TextBlock
        {
            Foreground = White,
            FontSize = 11,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Bottom,
            Margin = new Thickness(6),
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        Root.Children.Add(_diagnostics);

        Root.Children.Add(_indicators);
        PlaceIndicators();
    }

    /// <summary>Raised when the alert strip's Dismiss button is clicked.</summary>
    public Action<StageTile>? DismissClicked { get; set; }

    /// <summary>Whether the alert strip and border are shown.</summary>
    public bool AlertShown => _alertKey is not null;

    /// <summary>Whether the alert border is blinking (false: steady, or no alert).</summary>
    public bool AlertBlinking => _blinking;

    /// <summary>The alert strip's text, for the self-test.</summary>
    public string AlertText => _alertText.Text;

    /// <summary>The corner the indicators sit in.</summary>
    public ChromeCorner Corner => _corner;

    /// <summary>The symbols now shown in the corner.</summary>
    public IReadOnlyList<IndicatorItem> Indicators => _indicators.Items;

    /// <summary>Raised when the header's reset-zoom control is clicked.</summary>
    public Action<StageTile>? ResetClicked { get; set; }

    /// <summary>Whether the header's reset-zoom control shows.</summary>
    public bool ResetShown => _reset.Visibility == Visibility.Visible;

    /// <summary>Presses the reset-zoom control as a click would (the self-test).</summary>
    public void PressReset()
    {
        if (ResetShown)
        {
            ResetClicked?.Invoke(Tile);
        }
    }

    /// <summary>Whether the zoom map is on screen (zoomed, and the chrome up).</summary>
    public bool MapShown => _zoomed && _mapShown && _map.Visibility == Visibility.Visible;

    /// <summary>The zoom map's frame in the stage's units (empty when it is not up) and the region it marks, for the self-test.</summary>
    public (UnitRect Frame, UnitRect Visible) MapGeometry => (new UnitRect(_mapX, _mapY, _mapW, _mapH), _mapVisible);

    /// <summary>The map follows the chrome: shown while the pointer is active, faded out with it.</summary>
    public void SetMapShown(bool shown)
    {
        _mapShown = shown;
        _map.Opacity = _zoomed && shown ? 1 : 0;
    }

    public StageTile Tile { get; }

    public Grid Root { get; }

    public Button Speaker { get; }

    /// <summary>The header's opacity target (hover), 0 or 1.</summary>
    public bool HeaderShown { get; private set; }

    /// <summary>Follows the tile's rectangle (published by the render thread). Per frame; allocation-free.</summary>
    public void Place()
    {
        StageTile t = Tile;
        bool visible = t.Visible && t.Width > 1 && t.Height > 1;
        Visibility v = visible ? Visibility.Visible : Visibility.Collapsed;
        if (Root.Visibility != v)
        {
            Root.Visibility = v;
        }
        if (!visible)
        {
            return;
        }
        float x = t.X, y = t.Y, w = t.Width, h = t.Height;
        if (x != _x || y != _y)
        {
            Root.Translation = new Vector3(x, y, 0);
            _x = x;
            _y = y;
        }
        if (w != _w || h != _h)
        {
            Root.Width = w;
            Root.Height = h;
            _w = w;
            _h = h;
        }
        PlaceZoom(t);
    }

    // The reset control and the zoom map follow the tile's zoom. Per frame; allocation-free.
    private void PlaceZoom(StageTile t)
    {
        bool dewarped = t.Fisheye is { } fe && fe.Mode.Dewarps();
        bool moved = dewarped && t.FisheyeSetting.Aim is not null;
        bool zoomed = !dewarped && t.Zoom > 1.0001f && t.PictureWidth > 0 && t.PictureHeight > 0 && t.ViewWidth > 0 && t.ViewHeight > 0;
        Visibility reset = zoomed || moved ? Visibility.Visible : Visibility.Collapsed;
        if (_reset.Visibility != reset)
        {
            _reset.Visibility = reset;
            UpdateHeaderMargins();
        }
        if (!zoomed)
        {
            if (_zoomed)
            {
                _zoomed = false;
                _map.Opacity = 0;
                _map.Visibility = Visibility.Collapsed;
                _mapX = double.NaN;
            }
            return;
        }
        TileZoom zoom = t.DigitalZoom;
        ZoomBase b = t.ZoomBase;
        UnitRect d = zoom.Destination(b);
        // In the picture as drawn zoomed (it grows over the bars), in the stage's units.
        var picture = new UnitRect(t.ViewX + d.X * t.ViewWidth, t.ViewY + d.Y * t.ViewHeight, d.Width * t.ViewWidth, d.Height * t.ViewHeight);
        UnitRect frame = ZoomMap.Frame(picture, t.PictureWidth / (double)t.PictureHeight);
        UnitRect visible = zoom.Visible(b);
        if (!_zoomed)
        {
            _zoomed = true;
            _map.Visibility = Visibility.Visible;
            _map.Opacity = _mapShown ? 1 : 0;
        }
        if (frame.X != _mapX || frame.Y != _mapY || frame.Width != _mapW || frame.Height != _mapH || visible != _mapVisible)
        {
            _mapX = frame.X;
            _mapY = frame.Y;
            _mapW = frame.Width;
            _mapH = frame.Height;
            _mapVisible = visible;
            _map.Width = Math.Max(frame.Width, 0);
            _map.Height = Math.Max(frame.Height, 0);
            _map.Translation = new Vector3((float)(frame.X - t.X), (float)(frame.Y - t.Y), 0);
            UnitRect r = ZoomMap.Region(visible, Math.Max(frame.Width - 2, 0), Math.Max(frame.Height - 2, 0));
            _mapRegion.Width = r.Width;
            _mapRegion.Height = r.Height;
            Canvas.SetLeft(_mapRegion, r.X);
            Canvas.SetTop(_mapRegion, r.Y);
        }
    }

    /// <summary>Shows or hides the hover chrome and the selection outline.</summary>
    public void SetHover(bool header, bool selected)
    {
        HeaderShown = header;
        _header.Opacity = header ? 1 : 0;
        _outline.Opacity = selected && header ? 1 : selected ? 0.5 : 0;
        UpdateMeterLook();
    }

    /// <summary>The level meter, once per frame while it can be seen.</summary>
    public void SetLevel(float peak)
    {
        if (!(_audible || HeaderShown))
        {
            return;
        }
        double width = Math.Clamp(peak, 0, 1) * Math.Max(_w, 0);
        if (Math.Abs(_bar.Width - width) > 0.5)
        {
            _bar.Width = width;
        }
    }

    /// <summary>Called when the Retry button is pressed.</summary>
    public Action<StageTile>? RetryClicked { get; set; }

    /// <summary>Whether the Retry button shows under the status message.</summary>
    public bool RetryShown => _retry.Visibility == Visibility.Visible;

    /// <summary>Shows or hides the Retry button (it follows the status message's visibility).</summary>
    public void SetRetry(bool shown)
    {
        Visibility v = shown && _statusBox.Visibility == Visibility.Visible ? Visibility.Visible : Visibility.Collapsed;
        if (_retry.Visibility != v)
        {
            _retry.Visibility = v;
        }
    }

    /// <summary>Presses the Retry button as a click or the keyboard would (the self-test).</summary>
    public void PressRetry()
    {
        if (RetryShown)
        {
            RetryClicked?.Invoke(Tile);
        }
    }

    /// <summary>Text and badges after a state change (not per frame).</summary>
    public void Refresh(string title, string? status, bool audible, string? diagnostics)
    {
        if (_title.Text != title)
        {
            _title.Text = title;
        }
        if (_audible != audible)
        {
            _audible = audible;
            _speakerIcon.Glyph = audible ? "" : "";
            UpdateMeterLook();
        }
        if (status is null)
        {
            _statusBox.Visibility = Visibility.Collapsed;
        }
        else
        {
            if (_status.Text != status)
            {
                _status.Text = status;
            }
            _statusBox.Visibility = Visibility.Visible;
        }
        if (diagnostics is null)
        {
            _diagnostics.Visibility = Visibility.Collapsed;
        }
        else
        {
            _diagnostics.Text = diagnostics;
            _diagnostics.Visibility = Visibility.Visible;
        }
    }

    /// <summary>
    /// Sound badges ("Smoke alarm · sound") on a slate pill at the bottom left; empty hides them. A
    /// microphone is not an alarm sensor: no badge never means no alarm.
    /// </summary>
    public void SetSoundBadges(IReadOnlyList<string> texts)
    {
        string key = string.Join("|", texts);
        if (key == _badgeKey)
        {
            return;
        }
        _badgeKey = key;
        _badges.Children.Clear();
        foreach (string text in texts)
        {
            _badges.Children.Add(new Border
            {
                Background = SoundBadgeBrush,
                CornerRadius = new CornerRadius(4),
                Padding = new Thickness(6, 2, 6, 2),
                Child = new TextBlock { Text = text, Foreground = White, FontSize = 11, FontWeight = FontWeights.SemiBold },
            });
        }
    }

    /// <summary>The corner indicators: the symbols to show now and the corner (a setting). Cheap; call it on events and the tick.</summary>
    public void SetIndicators(IReadOnlyList<IndicatorItem> items, ChromeCorner corner)
    {
        double side = Math.Min(_w, _h);
        if (side > 0)
        {
            _indicators.SymbolSize = Math.Clamp(side * 0.07, 12, 20);
        }
        _indicators.SetItems(items);
        if (corner != _corner)
        {
            _corner = corner;
            PlaceIndicators();
        }
        double reserve = _corner == ChromeCorner.TopRight ? _indicators.OccupiedWidth + 8 : 0;
        _reserve = reserve;
        UpdateHeaderMargins();
    }

    // The top right indicators sit over the header: the reset control (or the title, without it) ends before them.
    private void UpdateHeaderMargins()
    {
        Thickness m = _reset.Margin;
        if (Math.Abs(m.Right - _reserve) > 0.5)
        {
            _reset.Margin = new Thickness(0, 0, _reserve, 0);
        }
        Thickness tm = _title.Margin;
        double titleRight = 8 + (_reset.Visibility == Visibility.Visible ? 0 : _reserve);
        if (Math.Abs(tm.Right - titleRight) > 0.5)
        {
            _title.Margin = new Thickness(2, 0, titleRight, 0);
        }
    }

    private void PlaceIndicators()
    {
        bool left = _corner.IsLeft(), top = _corner.IsTop();
        _indicators.HorizontalAlignment = left ? HorizontalAlignment.Left : HorizontalAlignment.Right;
        _indicators.VerticalAlignment = top ? VerticalAlignment.Top : VerticalAlignment.Bottom;
        // Clear of the speaker button (top left), the status badges and diagnostics (bottom), and the alert strip (top).
        double alert = AlertShown && top ? AlertStripHeight : 0;
        _indicators.Margin = new Thickness(left ? (top ? 44 : 8) : 0, top ? 8 + alert : 0, left ? 0 : 8, top ? 0 : 30);
    }

    /// <summary>
    /// The raised alert for this tile's camera: its lines (<see cref="Alert.Lines"/>) in a strip pinned at the top
    /// (not part of the hover chrome, so it stays through the full screen auto-hide) and a red border, blinking
    /// about once a second or steady. Null clears it.
    /// </summary>
    public void SetAlert(string[]? lines, bool blinking)
    {
        if (lines is null)
        {
            if (_alertKey is null)
            {
                return;
            }
            _alertKey = null;
            _alertStrip.Visibility = Visibility.Collapsed;
            _alertBorder.Visibility = Visibility.Collapsed;
            _blink.Stop();
            _blinking = false;
            _header.Margin = new Thickness(0);
            PlaceIndicators();
            return;
        }
        string text = string.Join("\n", lines);
        if (text != _alertKey)
        {
            bool first = _alertKey is null;
            _alertKey = text;
            _alertText.Text = text;
            AutomationProperties.SetName(_alertStrip, "Alert: " + string.Join(", ", lines));
            _alertStrip.Visibility = Visibility.Visible;
            _alertBorder.Visibility = Visibility.Visible;
            _header.Margin = new Thickness(0, AlertStripHeight, 0, 0);
            if (first)
            {
                PlaceIndicators();
            }
        }
        if (blinking != _blinking || (blinking && _blink.GetCurrentState() == ClockState.Stopped))
        {
            _blinking = blinking;
            if (blinking)
            {
                _blink.Begin();
            }
            else
            {
                _blink.Stop();
                _alertBorder.Opacity = 1;
            }
        }
    }

    private void UpdateMeterLook()
    {
        if (_audible)
        {
            _bar.Fill = ListeningBrush;
            _bar.Opacity = 1;
            _track.Opacity = 0;
        }
        else
        {
            _bar.Fill = MutedMeterBrush;
            _bar.Opacity = HeaderShown ? 1 : 0;
            _track.Opacity = HeaderShown ? 1 : 0;
        }
    }
}
