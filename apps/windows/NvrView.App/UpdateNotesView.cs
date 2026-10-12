// The update prompt's body (docs/UX.md, "Updates"): a fixed line above (what
// you have) and the release notes in a scrollable, selectable, wrapping area
// of bounded height, so the title and the dialog's buttons stay in view
// however long the notes are. Self-test step updates-notes-scroll.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace NvrView.App;

internal static class UpdateNotesView
{
    public const double Width = 420;
    public const double MaxHeight = 220;

    /// <summary>The notes area alone: a ScrollViewer around selectable text.</summary>
    public static ScrollViewer Notes(string notes) => new()
    {
        MaxHeight = MaxHeight,
        Width = Width,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        // Focusable, so the arrow, Page, Home, and End keys scroll it.
        IsTabStop = true,
        Content = new TextBlock
        {
            Text = notes,
            TextWrapping = TextWrapping.Wrap,
            IsTextSelectionEnabled = true,
            Margin = new Thickness(0, 0, 12, 0),
        },
    };

    public static UIElement Build(string line, string notes)
    {
        var panel = new StackPanel { Spacing = 12, Width = Width };
        panel.Children.Add(new TextBlock { Text = line, TextWrapping = TextWrapping.Wrap });
        if (notes.Length > 0)
        {
            panel.Children.Add(Notes(notes));
        }
        return panel;
    }
}
