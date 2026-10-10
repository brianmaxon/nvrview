// The symbols of the corner indicators (docs/UX.md "Corner indicators") as a
// reusable control: one small monochrome symbol per active kind with a soft
// shadow, an amber accent for the sound alarms, a tooltip and an automation
// name for each. Used by the tiles (TileChrome) and by the sidebar's camera
// and device rows (docs/UX.md: "the same symbols appear on each camera's row").
//
// Static elements only: it changes when the set of kinds changes (events and
// the slow tick), never per frame. Segoe Fluent Icons glyphs (Segoe MDL2
// Assets on Windows 10); the animal symbol, which the font lacks, is a
// vector paw.
//
//   var strip = new IndicatorStrip { SymbolSize = 14 };
//   strip.SetItems(viewer.Board.Items(device, channel));   // sidebar row
//   viewer.IndicatorsChanged += () => strip.SetItems(...);

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using NvrView.Tiles;
using Windows.UI;

namespace NvrView.App;

internal sealed class IndicatorStrip : StackPanel
{
    private static readonly SolidColorBrush White = new(Color.FromArgb(0xE6, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush Amber = new(Color.FromArgb(0xF2, 0xFF, 0xC7, 0x66));
    private static readonly SolidColorBrush InkShadow = new(Color.FromArgb(0xC0, 0, 0, 0));

    private double _size;
    private IReadOnlyList<IndicatorItem> _items = [];

    public IndicatorStrip(double symbolSize = 16)
    {
        _size = symbolSize;
        Orientation = Orientation.Horizontal;
        Spacing = Math.Max(3, symbolSize * 0.3);
        IsHitTestVisible = true;
        AutomationProperties.SetName(this, "Detections");
    }

    /// <summary>The symbols' size in DIPs.</summary>
    public double SymbolSize
    {
        get => _size;
        set
        {
            if (Math.Abs(_size - value) < 0.5)
            {
                return;
            }
            _size = value;
            Spacing = Math.Max(3, value * 0.3);
            IReadOnlyList<IndicatorItem> items = _items;
            _items = [];
            SetItems(items);
        }
    }

    /// <summary>What is shown now (nothing is never an "all clear").</summary>
    public IReadOnlyList<IndicatorItem> Items => _items;

    /// <summary>The width the symbols take (0 when none), so a title can keep clear.</summary>
    public double OccupiedWidth => _items.Count == 0 ? 0 : _items.Count * (_size + 2) + (_items.Count - 1) * Spacing;

    /// <summary>Shows exactly these symbols; does nothing when they are the same as now.</summary>
    public void SetItems(IReadOnlyList<IndicatorItem> items)
    {
        if (IndicatorItem.Same(_items, items))
        {
            return;
        }
        _items = items;
        Children.Clear();
        foreach (IndicatorItem item in items)
        {
            Children.Add(MakeSymbol(item, _size));
        }
        Visibility = items.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>The Segoe Fluent Icons code point for a kind (null: drawn as a vector).</summary>
    public static string? Glyph(IndicatorKind kind) => kind switch
    {
        IndicatorKind.Motion => "",
        IndicatorKind.Person => "",
        IndicatorKind.Vehicle => "",
        IndicatorKind.Animal => null,
        IndicatorKind.Face => "",
        IndicatorKind.Package => "",
        IndicatorKind.Visitor => "",
        IndicatorKind.SmokeAlarm => "",
        IndicatorKind.CoAlarm => "",
        _ => "",
    };

    private static FrameworkElement MakeSymbol(IndicatorItem item, double size)
    {
        SolidColorBrush ink = item.Kind.IsSound() ? Amber : White;
        // The shadow is a second copy in black, one pixel down and right: no
        // effect pass on a surface that sits above live video.
        var cell = new Grid { Width = size + 2, Height = size + 2 };
        cell.Children.Add(Draw(item.Kind, size, InkShadow, 1.2));
        cell.Children.Add(Draw(item.Kind, size, ink, 0));
        string text = item.Description;
        ToolTipService.SetToolTip(cell, text);
        AutomationProperties.SetName(cell, text);
        return cell;
    }

    private static FrameworkElement Draw(IndicatorKind kind, double size, Brush brush, double offset)
    {
        FrameworkElement e;
        if (Glyph(kind) is { } glyph)
        {
            e = new FontIcon { Glyph = glyph, FontSize = size, Foreground = brush };
        }
        else
        {
            e = Paw(size, brush);
        }
        e.HorizontalAlignment = HorizontalAlignment.Left;
        e.VerticalAlignment = VerticalAlignment.Top;
        e.Margin = new Thickness(offset, offset, 0, 0);
        e.IsHitTestVisible = false;
        return e;
    }

    // A paw print in a 24 x 24 box: one pad and four toes.
    private static Viewbox Paw(double size, Brush brush)
    {
        var canvas = new Canvas { Width = 24, Height = 24 };
        void Oval(double x, double y, double w, double h)
        {
            var oval = new Ellipse { Width = w, Height = h, Fill = brush };
            Canvas.SetLeft(oval, x);
            Canvas.SetTop(oval, y);
            canvas.Children.Add(oval);
        }
        Oval(6, 12, 12, 10);
        Oval(1.5, 9, 5, 6.5);
        Oval(6.5, 2.5, 5, 7);
        Oval(12.5, 2.5, 5, 7);
        Oval(17.5, 9, 5, 6.5);
        return new Viewbox { Width = size, Height = size, Child = canvas };
    }
}
