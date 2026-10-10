// The overlay drawn above one tile's video (docs/UX.md, "Chrome" and
// "Listening from any tile"): a header with the speaker button and the
// camera's name that fades in on hover and out a few seconds later, a thin
// level meter along the top (green while the tile is audible and always
// shown then; white on gray while hovering a muted tile, fading with the
// header), the selection outline, a status line in words (connecting,
// reconnecting with its countdown, waiting for the camera, login failed,
// needs the HEVC Video Extension, ...), detection badges, and an optional
// diagnostics line. Plain XAML elements above the swap chain; positions and
// meters are updated once per frame without allocating.

using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Xaml.Shapes;
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
    private static readonly SolidColorBrush AlarmBrush = new(Color.FromArgb(0xD0, 0xB0, 0x30, 0x20));
    private static readonly SolidColorBrush White = new(Colors.White);
    private static readonly SolidColorBrush OutlineBrush = new(Color.FromArgb(0xE0, 0x3A, 0x96, 0xDD));

    private readonly FontIcon _speakerIcon;
    private readonly TextBlock _title;
    private readonly Rectangle _track;
    private readonly Rectangle _bar;
    private readonly Border _statusBox;
    private readonly TextBlock _status;
    private readonly StackPanel _badges;
    private readonly TextBlock _diagnostics;
    private readonly Border _outline;
    private readonly Grid _header;
    private float _x = float.NaN, _y, _w, _h;
    private bool _audible;
    private string _badgeKey = "";

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
        Root.Children.Add(_header);

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
            Margin = new Thickness(12),
            Child = _status,
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        Root.Children.Add(_statusBox);

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
        string key = string.Join("|", Tile.Detections.Select(d => $"{d.Kind}/{d.Source}/{d.Lens}"));
        if (key != _badgeKey)
        {
            _badgeKey = key;
            _badges.Children.Clear();
            foreach (Detection d in Tile.Detections)
            {
                bool alarm = d.Kind is "smoke_alarm" or "co_alarm";
                _badges.Children.Add(new Border
                {
                    Background = alarm ? AlarmBrush : BadgeBrush,
                    CornerRadius = new CornerRadius(4),
                    Padding = new Thickness(6, 2, 6, 2),
                    Child = new TextBlock
                    {
                        Text = BadgeText(d),
                        Foreground = White,
                        FontSize = 11,
                        FontWeight = alarm ? FontWeights.SemiBold : FontWeights.Normal,
                    },
                });
            }
        }
    }

    /// <summary>The words on a detection badge: what, and who detected it.</summary>
    public static string BadgeText(Detection d)
    {
        string what = d.Kind switch
        {
            "people" => "Person",
            "vehicle" => "Vehicle",
            "dog_cat" => "Animal",
            "face" => "Face",
            "package" => "Package",
            "motion" => "Motion",
            "visitor" => "Visitor",
            "cry" => "Crying",
            "smoke_alarm" => "Smoke alarm",
            "co_alarm" => "CO alarm",
            _ => d.Kind.Replace('_', ' '),
        };
        string source = d.Source switch
        {
            "device" => "camera",
            "local_audio" or "local_video" => "this PC",
            _ => d.Source,
        };
        return d.Lens > 0 ? $"{what} · {source} · lens {d.Lens + 1}" : $"{what} · {source}";
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
