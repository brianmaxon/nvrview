// The loudness strip of the Recordings window's lanes (docs/UX.md "Recordings",
// Sound): the strip itself with its gap labels and hover text, the handle that
// drags its height, and where that handle sits. Split from RecordingsTimeline.cs.

using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using NvrView.Shell;

namespace NvrView.App;

/// <summary>
/// The thin bar on the first loudness strip's lower edge: dragging it up or down asks for another height of all the strips
/// (the browser clamps and keeps it). It is not drawn into the lanes' canvas, so redrawing the lanes does not end a drag.
/// </summary>
internal sealed class LoudnessResizeHandle : Grid
{
    /// <summary>How tall the grab area is.</summary>
    public const double Thickness = 6;

    private double? _fromY;
    private double _fromHeight;

    public LoudnessResizeHandle()
    {
        Background = new SolidColorBrush(Colors.Transparent);
        VerticalAlignment = VerticalAlignment.Top;
        HorizontalAlignment = HorizontalAlignment.Stretch;
        Visibility = Visibility.Collapsed;
        Children.Add(new Rectangle
        {
            Height = 1,
            VerticalAlignment = VerticalAlignment.Center,
            Fill = new SolidColorBrush(Windows.UI.Color.FromArgb(0x38, 0xFF, 0xFF, 0xFF)),
            IsHitTestVisible = false,
        });
        ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeNorthSouth);
        ToolTipService.SetToolTip(this, "Drag to change the height of the loudness lanes");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(this, "Loudness lane height");
        PointerPressed += (_, e) =>
        {
            if (CapturePointer(e.Pointer))
            {
                _fromY = e.GetCurrentPoint(null).Position.Y;
                _fromHeight = CurrentHeight;
                e.Handled = true;
            }
        };
        PointerMoved += (_, e) =>
        {
            if (_fromY is { } from)
            {
                HeightChanged?.Invoke(_fromHeight + (e.GetCurrentPoint(null).Position.Y - from));
                e.Handled = true;
            }
        };
        PointerReleased += (_, e) => End(e.Pointer);
        PointerCanceled += (_, e) => End(e.Pointer);
        PointerCaptureLost += (_, _) => End(null);
    }

    /// <summary>The strips' height now (set by the lanes).</summary>
    public double CurrentHeight { get; set; }

    /// <summary>The height asked for while dragging.</summary>
    public event Action<double>? HeightChanged;

    /// <summary>The drag ended.</summary>
    public event Action? Kept;

    private void End(Pointer? pointer)
    {
        if (_fromY is null)
        {
            return;
        }
        _fromY = null;
        if (pointer is not null)
        {
            ReleasePointerCapture(pointer);
        }
        Kept?.Invoke();
    }
}

internal sealed partial class LanesView
{
    // The handle on the first strip's lower edge, kept in place while the lanes scroll (it is not part of the canvas, so
    // a drag survives the redraws it causes).
    private void PlaceHandle()
    {
        if (_lanes.Count == 0 || !_drawn)
        {
            _handle.Visibility = Visibility.Collapsed;
            return;
        }
        double y = RecordingsPanes.RulerHeight + RecordingsPanes.LaneHeight + _loudnessHeight - _scroll.VerticalOffset - (LoudnessResizeHandle.Thickness / 2);
        _handle.Visibility = y > -LoudnessResizeHandle.Thickness && y < ActualHeight ? Visibility.Visible : Visibility.Collapsed;
        _handle.Margin = new Thickness(0, Math.Max(y, 0), 0, 0);
        _handle.Height = LoudnessResizeHandle.Thickness;
        _handle.CurrentHeight = _loudnessHeight;
    }

    // A lane's loudness strip: the loudest second of each bucket light, its average dark, from -80 dBFS (nothing)
    // to 0 (full height), as two polygons (not a shape per bucket); blank where no audio was analyzed.
    private void AddLoudness(RecordingCamera camera, LoudnessLane? lane, double top, double width)
    {
        double h = _loudnessHeight, bottom = top + h;
        var track = new Rectangle { Width = width, Height = h, Fill = new SolidColorBrush(Windows.UI.Color.FromArgb(0x0D, 0xFF, 0xFF, 0xFF)), IsHitTestVisible = false };
        Canvas.SetTop(track, top);
        _canvas.Children.Add(track);
        if (lane is null || lane.Bucket <= 0)
        {
            return;
        }
        if (lane.Heard)
        {
            _canvas.Children.Add(Envelope(lane, lane.Peak, 0x60, bottom, width));
            _canvas.Children.Add(Envelope(lane, lane.Rms, 0xD8, bottom, width));
        }
        // Empty stretches say why: a label where it fits, a tint where "Analyze this gap" applies, and the reason as
        // hover text.
        foreach (LoudnessGap g in lane.GapList)
        {
            double x0 = width * (g.Start - _day) / 86400.0, x1 = width * (g.End + 1 - _day) / 86400.0;
            if (x1 <= x0)
            {
                continue;
            }
            var hit = new Rectangle
            {
                Width = x1 - x0,
                Height = h,
                Fill = new SolidColorBrush(g.CanAnalyze ? Windows.UI.Color.FromArgb(0x22, 0x3B, 0x82, 0xF6) : Colors.Transparent),
            };
            ToolTipService.SetToolTip(hit, g.Help);
            Canvas.SetLeft(hit, x0);
            Canvas.SetTop(hit, top);
            _canvas.Children.Add(hit);
            // About 5.5 points a character at 9 points; the label is skipped where it would not fit or the strip is thin.
            if (h >= 14 && x1 - x0 >= (g.Label.Length * 5.5) + 8)
            {
                var text = new TextBlock
                {
                    Text = g.Label,
                    FontSize = 9,
                    Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xA3, 0xA8)),
                    IsHitTestVisible = false,
                };
                Canvas.SetLeft(text, x0 + 4);
                Canvas.SetTop(text, top + ((h - 12) / 2));
                _canvas.Children.Add(text);
            }
        }
    }

    private Polygon Envelope(LoudnessLane lane, IReadOnlyList<int?> values, byte alpha, double bottom, double width)
    {
        double h = _loudnessHeight;
        var points = new Microsoft.UI.Xaml.Media.PointCollection();
        double x = 0;
        points.Add(new Windows.Foundation.Point(0, bottom));
        for (int i = 0; i < values.Count; i++)
        {
            double x0 = width * (lane.Start + i * lane.Bucket - _day) / 86400.0, x1 = width * (lane.Start + (i + 1) * lane.Bucket - _day) / 86400.0;
            double y = values[i] is { } db ? bottom - Math.Max(h * LoudnessScale.Level(db), 1) : bottom;
            points.Add(new Windows.Foundation.Point(x0, y));
            points.Add(new Windows.Foundation.Point(x1, y));
            x = x1;
        }
        points.Add(new Windows.Foundation.Point(x, bottom));
        Windows.UI.Color c = TimelineColors.Loudness;
        return new Polygon { Points = points, Fill = new SolidColorBrush(Windows.UI.Color.FromArgb(alpha, c.R, c.G, c.B)), IsHitTestVisible = false };
    }
}
