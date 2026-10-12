// The Recordings window's two time controls (docs/UX.md "Recordings"): the
// scrubber under the transport (the selected incident with its margins, or
// the whole day, with the events marked by kind; dragging shows keyframes,
// the release the exact picture) and the day's timeline lanes (one lane per
// camera: recording spans in gray, events as bars colored by kind, an hour
// ruler, zoom from the whole day to 48x with Ctrl and the wheel, and the
// playhead). Both only draw what the browser hands them and report where the
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

    /// <summary>Whether a drag is on (the position shown is then the pointer's).</summary>
    public bool IsDragging => _dragging;

    /// <summary>The range the bar spans, device-local milliseconds.</summary>
    public (long Start, long End) Range => (_start, _end);

    /// <summary>Shows a range, its marks, and the playhead.</summary>
    public void Set(long start, long end, IEnumerable<TimeMark> marks, long positionMs)
    {
        List<TimeMark> list = [.. marks];
        bool changed = start != _start || end != _end || !list.SequenceEqual(_marks);
        _start = start;
        _end = end;
        _marks = list;
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
    private long _day;
    private double _zoom = 1;
    private long? _playheadSec;
    private bool _drawn;

    public LanesView()
    {
        Children.Add(_scroll);
        _scroll.Content = _canvas;
        _scroll.SizeChanged += (_, _) => Rebuild();
        _scroll.ViewChanged += (_, _) => PlaceLabels();
        _canvas.Tapped += OnTapped;
        _canvas.PointerWheelChanged += OnWheel;
    }

    /// <summary>A click on a lane: the camera, the second, and the event under the pointer if any.</summary>
    public event Action<RecordingCamera, long, RecordedEvent?>? LaneClicked;

    /// <summary>The user zoomed with Ctrl and the wheel.</summary>
    public event Action<double>? ZoomRequested;

    private double ContentWidth => Math.Max(_scroll.ActualWidth - 4, 100) * _zoom;

    /// <summary>Shows the lanes of a day.</summary>
    public void Set(IReadOnlyList<TimelineLane> lanes, long day, double zoom, long? playheadMs)
    {
        bool changed = !ReferenceEquals(lanes, _lanes) || day != _day || zoom != _zoom || !_drawn;
        _lanes = lanes;
        _day = day;
        _zoom = zoom;
        _playheadSec = playheadMs is { } ms ? DeviceTime.FloorDiv(ms, 1000) : null;
        if (changed)
        {
            Rebuild();
        }
        else
        {
            PlaceHead();
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
        double height = RecordingsPanes.RulerHeight + Math.Max(_lanes.Count, 1) * RecordingsPanes.LaneHeight;
        _canvas.Width = w;
        _canvas.Height = height;
        // Hour ruler: every 6 hours at the whole day, finer as it zooms.
        int step = _zoom >= 8 ? 1 : _zoom >= 4 ? 2 : _zoom >= 2 ? 3 : 6;
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
            double top = RecordingsPanes.RulerHeight + i * RecordingsPanes.LaneHeight;
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
                var bar = new Rectangle { Width = Math.Max(x1 - x0, 3), Height = RecordingsPanes.LaneHeight - 6, Fill = new SolidColorBrush(TimelineColors.Of(e.Kind)), RadiusX = 2, RadiusY = 2, IsHitTestVisible = false };
                Canvas.SetLeft(bar, x0);
                Canvas.SetTop(bar, top + 3);
                _canvas.Children.Add(bar);
            }
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
        _canvas.Children.Add(_head);
        PlaceLabels();
        PlaceHead();
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
        ((int)Math.Floor((p.Y - RecordingsPanes.RulerHeight) / RecordingsPanes.LaneHeight), _day + (long)(p.X / ContentWidth * 86400));

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

    private void OnWheel(object sender, PointerRoutedEventArgs e)
    {
        if (!InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down))
        {
            return;
        }
        int delta = e.GetCurrentPoint(_canvas).Properties.MouseWheelDelta;
        double zoom = Math.Clamp(_zoom * Math.Pow(1.25, delta / 120.0), 1, 48);
        if (zoom == _zoom)
        {
            e.Handled = true;
            return;
        }
        // Keep the time under the pointer where it is.
        double px = e.GetCurrentPoint(_scroll).Position.X;
        double fraction = (_scroll.HorizontalOffset + px) / ContentWidth;
        ZoomRequested?.Invoke(zoom);
        double newWidth = Math.Max(_scroll.ActualWidth - 4, 100) * zoom;
        _ = _scroll.ChangeView(Math.Max(fraction * newWidth - px, 0), null, null, true);
        e.Handled = true;
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
