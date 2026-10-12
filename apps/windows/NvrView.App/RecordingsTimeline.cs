// The Recordings window's two time controls (docs/UX.md "Recordings"): the
// scrubber under the transport (the selected incident with its margins, or
// the whole day, with the events marked by kind; dragging shows keyframes,
// the release the exact picture) and the day's timeline lanes (one lane per
// camera: recording spans in gray, events as bars colored by kind (sounds in
// their own color), a loudness strip under each lane with a legend, an hour
// ruler, zoom from the whole day to 48x with the wheel around the pointer, and
// the playhead). Both only draw what the browser hands them and report where the
// user pointed; the browser and the player decide what that means.

using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;
using Windows.UI.Core;

namespace NvrView.App;

/// <summary>An event mark on the scrubber or a lane.</summary>
internal readonly record struct TimeMark(long StartMs, long EndMs, string Kind);

internal static class TimelineColors
{
    public static Windows.UI.Color Of(string kind) => KindFilters.Indicator(kind) switch
    {
        IndicatorKind.Person => Windows.UI.Color.FromArgb(0xFF, 0x3B, 0x82, 0xF6),
        IndicatorKind.Vehicle => Windows.UI.Color.FromArgb(0xFF, 0xA8, 0x55, 0xF7),
        IndicatorKind.Animal => Windows.UI.Color.FromArgb(0xFF, 0x22, 0xC5, 0x5E),
        IndicatorKind.Motion => Windows.UI.Color.FromArgb(0xFF, 0x9C, 0xA3, 0xAF),
        IndicatorKind.SmokeAlarm or IndicatorKind.CoAlarm => Windows.UI.Color.FromArgb(0xFF, 0xF5, 0x9E, 0x0B),
        IndicatorKind.Visitor => Windows.UI.Color.FromArgb(0xFF, 0x14, 0xB8, 0xA6),
        _ => Windows.UI.Color.FromArgb(0xFF, 0xA1, 0x62, 0x07),
    };

    /// <summary>An event from sound other than an alarm (alarms keep their kind's color).</summary>
    public static Windows.UI.Color Of(RecordedEvent e) =>
        SoundKinds.IsSound(e) && KindFilters.Indicator(e.Kind) == IndicatorKind.Other ? Sound : Of(e.Kind);

    public static readonly Windows.UI.Color Sound = Windows.UI.Color.FromArgb(0xFF, 0xDB, 0x5C, 0x9E);
    public static readonly Windows.UI.Color Loudness = Windows.UI.Color.FromArgb(0xFF, 0x2D, 0xB8, 0xC0);
    public static readonly Windows.UI.Color Playhead = Windows.UI.Color.FromArgb(0xFF, 0xFF, 0x5A, 0x4F);
    public static readonly Windows.UI.Color Track = Windows.UI.Color.FromArgb(0xFF, 0x3A, 0x3C, 0x42);
    public static readonly Windows.UI.Color Recorded = Windows.UI.Color.FromArgb(0xFF, 0x70, 0x73, 0x7A);
}

/// <summary>The scrubber: a track over a range of the playback with event marks and the playhead.</summary>
internal sealed class ScrubBar : Canvas
{
    private readonly Rectangle _head = new() { Width = 2, Fill = new SolidColorBrush(TimelineColors.Playhead), IsHitTestVisible = false };
    private long _start, _end, _position;
    private List<TimeMark> _marks = [];
    private List<long> _cuts = [];
    private bool _drawnMarks;
    private bool _dragging;

    public ScrubBar()
    {
        Height = 30;
        MinWidth = 60;
        Background = new SolidColorBrush(Colors.Transparent);
        SizeChanged += (_, _) => Redraw(true);
        PointerPressed += OnPressed;
        PointerMoved += OnMoved;
        PointerReleased += OnReleased;
        PointerCanceled += (_, _) => _dragging = false;
    }

    /// <summary>The user pointed at a time: the milliseconds, and whether this is the release (the exact picture).</summary>
    public event Action<long, bool>? Seeked;

    /// <summary>The follow cut's cuts shown as white ticks (self-test).</summary>
    public IReadOnlyList<long> CutTicks => _cuts;

    /// <summary>Whether a drag is on (the position shown is then the pointer's).</summary>
    public bool IsDragging => _dragging;

    /// <summary>The range the bar spans, device-local milliseconds.</summary>
    public (long Start, long End) Range => (_start, _end);

    /// <summary>Shows a range, its marks, the follow cut's cuts (white ticks), and the playhead.</summary>
    public void Set(long start, long end, IEnumerable<TimeMark> marks, long positionMs, IReadOnlyList<long> cuts)
    {
        List<TimeMark> list = [.. marks];
        bool changed = start != _start || end != _end || !list.SequenceEqual(_marks) || !cuts.SequenceEqual(_cuts);
        _start = start;
        _end = end;
        _marks = list;
        _cuts = [.. cuts];
        if (!_dragging)
        {
            _position = positionMs;
        }
        Redraw(changed);
    }

    /// <summary>The playhead's x for a time, in the bar's width.</summary>
    public double XOf(long ms) => ActualWidth * TransportState.Fraction(ms, _start, _end);

    private void Redraw(bool marks)
    {
        double w = ActualWidth, h = ActualHeight;
        if (w <= 0 || h <= 0)
        {
            return;
        }
        if (marks || !_drawnMarks)
        {
            Children.Clear();
            Children.Add(new Rectangle { Width = w, Height = 6, RadiusX = 3, RadiusY = 3, Fill = new SolidColorBrush(TimelineColors.Track) });
            SetTop(Children[0], h - 12);
            foreach (TimeMark m in _marks)
            {
                double x0 = w * TransportState.Fraction(m.StartMs, _start, _end), x1 = w * TransportState.Fraction(m.EndMs, _start, _end);
                var bar = new Rectangle { Width = Math.Max(x1 - x0, 3), Height = 10, Fill = new SolidColorBrush(TimelineColors.Of(m.Kind)), IsHitTestVisible = false };
                SetLeft(bar, x0);
                SetTop(bar, h - 14);
                Children.Add(bar);
            }
            foreach (long cut in _cuts)
            {
                double x = w * TransportState.Fraction(cut, _start, _end);
                var tick = new Rectangle
                {
                    Width = 2,
                    Height = 18,
                    Fill = new SolidColorBrush(Colors.White),
                    Stroke = new SolidColorBrush(Windows.UI.Color.FromArgb(0x80, 0, 0, 0)),
                    StrokeThickness = 0.5,
                    IsHitTestVisible = false,
                };
                SetLeft(tick, x - 1);
                SetTop(tick, h - 18);
                Children.Add(tick);
            }
            Children.Add(_head);
            _drawnMarks = true;
        }
        _head.Height = h;
        SetLeft(_head, Math.Clamp(XOf(_position) - 1, 0, Math.Max(w - 2, 0)));
        SetTop(_head, 0);
    }

    private long TimeAt(PointerRoutedEventArgs e) =>
        TransportState.TimeAtFraction(e.GetCurrentPoint(this).Position.X / Math.Max(ActualWidth, 1), _start, _end);

    private void OnPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_end <= _start)
        {
            return;
        }
        _dragging = true;
        _ = CapturePointer(e.Pointer);
        _position = TimeAt(e);
        Redraw(false);
        Seeked?.Invoke(_position, false);
        e.Handled = true;
    }

    private void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
        {
            return;
        }
        _position = TimeAt(e);
        Redraw(false);
        Seeked?.Invoke(_position, false);
    }

    private void OnReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!_dragging)
        {
            return;
        }
        _dragging = false;
        ReleasePointerCapture(e.Pointer);
        _position = TimeAt(e);
        Redraw(false);
        Seeked?.Invoke(_position, true);
        e.Handled = true;
    }

    /// <summary>Points at a time as a user would (self-test): a drag to it and the release.</summary>
    public void PointAt(long ms)
    {
        _position = ms;
        Redraw(false);
        Seeked?.Invoke(ms, false);
        Seeked?.Invoke(ms, true);
    }
}

/// <summary>The day's timeline lanes with an hour ruler, zoom, and the playhead.</summary>
internal sealed class LanesView : Grid
{
    private readonly ScrollViewer _scroll = new()
    {
        HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalScrollMode = ScrollMode.Auto,
        VerticalScrollMode = ScrollMode.Auto,
        ZoomMode = ZoomMode.Disabled,
    };
    private readonly Canvas _canvas = new() { Background = new SolidColorBrush(Colors.Transparent) };
    private readonly Rectangle _head = new() { Width = 2, Fill = new SolidColorBrush(TimelineColors.Playhead), IsHitTestVisible = false };
    private readonly List<TextBlock> _labels = [];
    private IReadOnlyList<TimelineLane> _lanes = [];
    private IReadOnlyList<LoudnessLane?> _loudness = [];
    private long _day;
    private TimelineViewport _view = new();
    // Scrolls of ours still to land (their ViewChanged is not the user's), and retries left.
    private bool _scrolling;
    private int _retries;
    private long? _playheadSec;
    private bool _drawn;

    public LanesView()
    {
        Children.Add(_scroll);
        _scroll.Content = _canvas;
        _scroll.SizeChanged += (_, _) =>
        {
            Rebuild();
            WidthChanged?.Invoke(ViewportWidth);
        };
        _scroll.ViewChanged += OnViewChanged;
        _canvas.Tapped += OnTapped;
        _canvas.PointerWheelChanged += OnWheel;
    }

    /// <summary>A click on a lane: the camera, the second, and the event under the pointer if any.</summary>
    public event Action<RecordingCamera, long, RecordedEvent?>? LaneClicked;

    /// <summary>The user zoomed (the wheel) or panned (Shift and the wheel, a horizontal wheel, the scroll bar).</summary>
    public event Action<TimelineViewport>? ViewportRequested;

    /// <summary>The lanes' visible width.</summary>
    public double ViewportWidth => Math.Max(_scroll.ActualWidth - 4, 100);

    /// <summary>Raised when the lanes' width changed (the loudness bucket follows it).</summary>
    public event Action<double>? WidthChanged;

    private double ContentWidth => ViewportWidth * _view.Zoom;

    /// <summary>The part of the day shown.</summary>
    public TimelineViewport Viewport => _view;

    /// <summary>Shows the lanes of a day, <paramref name="view"/>'s part of it, with each lane's loudness (in lane order).</summary>
    public void Set(IReadOnlyList<TimelineLane> lanes, IReadOnlyList<LoudnessLane?> loudness, long day, TimelineViewport view, long? playheadMs)
    {
        bool changed = !ReferenceEquals(lanes, _lanes) || !loudness.SequenceEqual(_loudness) || day != _day || view.Zoom != _view.Zoom || !_drawn;
        _lanes = lanes;
        _loudness = loudness;
        _day = day;
        _view = view;
        _playheadSec = playheadMs is { } ms ? DeviceTime.FloorDiv(ms, 1000) : null;
        if (changed)
        {
            Rebuild();
        }
        else
        {
            PlaceHead();
        }
        ScrollToView();
    }

    /// <summary>Scrolls to the viewport's start (the content is the whole day at the zoom).</summary>
    private void ScrollToView()
    {
        if (_scroll.ActualWidth <= 0)
        {
            return;
        }
        double x = _view.Start * ContentWidth;
        if (Math.Abs(_scroll.HorizontalOffset - x) > 0.5)
        {
            _scrolling = true;
            _ = _scroll.ChangeView(x, null, null, true);
        }
    }

    private void OnViewChanged(object? sender, ScrollViewerViewChangedEventArgs e)
    {
        PlaceLabels();
        if (e.IsIntermediate || _scroll.ActualWidth <= 0)
        {
            return;
        }
        if (_scrolling)
        {
            // Ours landed (or was clamped while the canvas was still being laid out at the new width: once more).
            _scrolling = false;
            if (Math.Abs(_scroll.HorizontalOffset - _view.Start * ContentWidth) > 0.5 && _retries++ < 1)
            {
                ScrollToView();
            }
            else
            {
                _retries = 0;
            }
            return;
        }
        // The scroll bar or a touchpad moved the lanes: the viewport follows.
        var moved = new TimelineViewport(_view.Zoom, _scroll.HorizontalOffset / ContentWidth);
        if (Math.Abs(moved.Start - _view.Start) * ContentWidth > 0.5)
        {
            _view = moved;
            ViewportRequested?.Invoke(moved);
        }
    }

    private void Rebuild()
    {
        double w = ContentWidth;
        if (_scroll.ActualWidth <= 0)
        {
            return;
        }
        _drawn = true;
        _canvas.Children.Clear();
        _labels.Clear();
        double height = RecordingsPanes.RulerHeight + Math.Max(_lanes.Count, 1) * RecordingsPanes.RowHeight + RecordingsPanes.LegendHeight;
        _canvas.Width = w;
        _canvas.Height = height;
        // Hour ruler: every 6 hours at the whole day, finer as it zooms.
        int step = _view.Zoom >= 8 ? 1 : _view.Zoom >= 4 ? 2 : _view.Zoom >= 2 ? 3 : 6;
        for (int hour = 0; hour <= 24; hour += step)
        {
            double x = w * hour / 24.0;
            var tick = new Rectangle { Width = 1, Height = height, Fill = new SolidColorBrush(Windows.UI.Color.FromArgb(0x30, 0xFF, 0xFF, 0xFF)), IsHitTestVisible = false };
            Canvas.SetLeft(tick, x);
            _canvas.Children.Add(tick);
            if (hour < 24)
            {
                var t = new TextBlock { Text = $"{hour:00}:00", FontSize = 10, Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xA3, 0xA8)), IsHitTestVisible = false };
                Canvas.SetLeft(t, x + 3);
                Canvas.SetTop(t, 1);
                _canvas.Children.Add(t);
            }
        }
        for (int i = 0; i < _lanes.Count; i++)
        {
            TimelineLane lane = _lanes[i];
            double top = RecordingsPanes.RulerHeight + i * RecordingsPanes.RowHeight;
            foreach (RecordingSpan s in lane.Spans)
            {
                double x0 = w * (s.Start - _day) / 86400.0, x1 = w * (s.End - _day) / 86400.0;
                var bar = new Rectangle { Width = Math.Max(x1 - x0, 1), Height = RecordingsPanes.LaneHeight - 10, Fill = new SolidColorBrush(TimelineColors.Recorded), Opacity = 0.55, IsHitTestVisible = false };
                Canvas.SetLeft(bar, x0);
                Canvas.SetTop(bar, top + 5);
                _canvas.Children.Add(bar);
            }
            foreach (RecordedEvent e in lane.Events)
            {
                double x0 = w * (e.Start - _day) / 86400.0, x1 = w * (e.End - _day) / 86400.0;
                var bar = new Rectangle { Width = Math.Max(x1 - x0, 3), Height = RecordingsPanes.LaneHeight - 6, Fill = new SolidColorBrush(TimelineColors.Of(e)), RadiusX = 2, RadiusY = 2, IsHitTestVisible = false };
                Canvas.SetLeft(bar, x0);
                Canvas.SetTop(bar, top + 3);
                _canvas.Children.Add(bar);
            }
            AddLoudness(i < _loudness.Count ? _loudness[i] : null, top + RecordingsPanes.LaneHeight, w);
            var label = new TextBlock
            {
                Text = lane.Failed ? $"{lane.Name} ⚠ could not search this day" : lane.Name,
                FontSize = 11,
                Foreground = new SolidColorBrush(lane.Failed ? Windows.UI.Color.FromArgb(0xFF, 0xF5, 0x9E, 0x0B) : Colors.White),
                IsHitTestVisible = false,
            };
            Canvas.SetTop(label, top + 5);
            _labels.Add(label);
            _canvas.Children.Add(label);
        }
        var legend = new TextBlock
        {
            Text = "Loudness: light = loudest second, dark = average; blank = not analyzed (not silence). Pink bars are sounds.",
            FontSize = 10,
            Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xA3, 0xA8)),
            IsHitTestVisible = false,
        };
        Canvas.SetTop(legend, height - RecordingsPanes.LegendHeight);
        _labels.Add(legend);
        _canvas.Children.Add(legend);
        _canvas.Children.Add(_head);
        PlaceLabels();
        PlaceHead();
    }

    // A lane's loudness strip: the loudest second of each bucket light, its average dark, from -80 dBFS (nothing)
    // to 0 (full height), as two polygons (not a shape per bucket); blank where no audio was analyzed.
    private void AddLoudness(LoudnessLane? lane, double top, double width)
    {
        double h = RecordingsPanes.LoudnessHeight, bottom = top + h;
        var track = new Rectangle { Width = width, Height = h, Fill = new SolidColorBrush(Windows.UI.Color.FromArgb(0x0D, 0xFF, 0xFF, 0xFF)), IsHitTestVisible = false };
        Canvas.SetTop(track, top);
        _canvas.Children.Add(track);
        if (lane is null || lane.Bucket <= 0 || !lane.Heard)
        {
            return;
        }
        _canvas.Children.Add(Envelope(lane, lane.Peak, 0x60, bottom, width));
        _canvas.Children.Add(Envelope(lane, lane.Rms, 0xD8, bottom, width));
    }

    private Polygon Envelope(LoudnessLane lane, IReadOnlyList<int?> values, byte alpha, double bottom, double width)
    {
        double h = RecordingsPanes.LoudnessHeight;
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

    private void PlaceLabels()
    {
        // Lane names stay at the left edge of what is shown.
        foreach (TextBlock label in _labels)
        {
            Canvas.SetLeft(label, _scroll.HorizontalOffset + 6);
        }
    }

    private void PlaceHead()
    {
        if (_playheadSec is not { } sec || sec < _day || sec > _day + DeviceTime.Day)
        {
            _head.Visibility = Visibility.Collapsed;
            return;
        }
        _head.Visibility = Visibility.Visible;
        _head.Height = _canvas.Height;
        Canvas.SetLeft(_head, ContentWidth * (sec - _day) / 86400.0 - 1);
        Canvas.SetTop(_head, 0);
    }

    private (int Lane, long Second) At(Windows.Foundation.Point p) =>
        ((int)Math.Floor((p.Y - RecordingsPanes.RulerHeight) / RecordingsPanes.RowHeight), _day + (long)(p.X / ContentWidth * 86400));

    private void OnTapped(object sender, TappedRoutedEventArgs e)
    {
        (int lane, long second) = At(e.GetPosition(_canvas));
        if (lane < 0 || lane >= _lanes.Count)
        {
            return;
        }
        // An event under the pointer (a few pixels of slack on each side).
        long slack = (long)(4 / ContentWidth * 86400);
        RecordedEvent? near = _lanes[lane].Events.FirstOrDefault(x => x.Start - slack <= second && second <= x.End + slack);
        LaneClicked?.Invoke(_lanes[lane].Camera, second, near);
        e.Handled = true;
    }

    // The wheel zooms around the pointer, each notch 1.25x (as on macOS);
    // Shift and the wheel, or a horizontal wheel, pans.
    private void OnWheel(object sender, PointerRoutedEventArgs e)
    {
        PointerPointProperties props = e.GetCurrentPoint(_canvas).Properties;
        bool shift = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Shift).HasFlag(CoreVirtualKeyStates.Down);
        Wheel(props.MouseWheelDelta, props.IsHorizontalMouseWheel, shift, e.GetCurrentPoint(_scroll).Position.X / ViewportWidth);
        e.Handled = true;
    }

    /// <summary>
    /// A wheel turn of <paramref name="delta"/> (120 a notch, positive away from the user or to the right) at the
    /// pointer <paramref name="pointer"/> (0 at the lanes' left edge, 1 at their right), as the window gets it (the
    /// self-test calls it too).
    /// </summary>
    public void Wheel(int delta, bool horizontal, bool shift, double pointer)
    {
        double notches = delta / 120.0;
        TimelineViewport next = horizontal ? _view.PannedBy(notches * TimelineViewport.LineStep)
            : shift ? _view.PannedBy(-notches * TimelineViewport.LineStep)
            : _view.ZoomedBy(TimelineViewport.WheelFactor(notches), Math.Clamp(pointer, 0, 1));
        if (next != _view)
        {
            ViewportRequested?.Invoke(next);
        }
    }

    /// <summary>Clicks a lane as a user would (self-test).</summary>
    public void ClickLane(int lane, long second)
    {
        if (lane >= 0 && lane < _lanes.Count)
        {
            long slack = 0;
            RecordedEvent? near = _lanes[lane].Events.FirstOrDefault(x => x.Start - slack <= second && second <= x.End + slack);
            LaneClicked?.Invoke(_lanes[lane].Camera, second, near);
        }
    }

    /// <summary>The playhead is shown (self-test).</summary>
    public bool PlayheadShown => _head.Visibility == Visibility.Visible && _drawn;
}
