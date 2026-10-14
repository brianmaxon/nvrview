// The update prompt's body (docs/UX.md, "Updates"): a fixed line above (what
// you have) and the release notes in a scrollable, selectable, wrapping area
// with set-apart headings and indented bullets, tall enough to show about 22
// lines (less in a small window), so the title and the dialog's buttons stay
// in view however long the notes are. A ContentDialog cannot be resized by the
// user (docs/PARITY.md), so the area takes the main window's height when the
// dialog opens. Self-test step updates-notes-scroll.

using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;

namespace NvrView.App;

internal static class UpdateNotesView
{
    public const double Width = 500;

    /// <summary>The tallest the notes area gets: about 22 lines of text.</summary>
    public const double MaxHeight = 440;

    /// <summary>The smallest height the area shrinks to in a small window.</summary>
    public const double MinHeight = 160;

    /// <summary>Space the dialog needs besides the notes area: title, line, buttons, and padding.</summary>
    private const double Chrome = 300;

    /// <summary>The Release notes list uses the same area.</summary>
    public const double ListMaxHeight = MaxHeight;

    /// <summary>The area's height for a window of this height.</summary>
    public static double HeightFor(double windowHeight) => Math.Clamp(windowHeight - Chrome, MinHeight, MaxHeight);

    /// <summary>The notes as styled paragraphs: headings bold and set apart, bullets with a hanging indent.</summary>
    public static RichTextBlock Text(string notes)
    {
        var text = new RichTextBlock
        {
            TextWrapping = TextWrapping.Wrap,
            IsTextSelectionEnabled = true,
            Margin = new Thickness(0, 0, 12, 0),
        };
        foreach (NoteLine line in ReleaseNotesLines.Classify(notes))
        {
            if (line.Kind == NoteLineKind.Blank)
            {
                continue;
            }
            var paragraph = new Paragraph();
            switch (line.Kind)
            {
                case NoteLineKind.Heading:
                    paragraph.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = line.Text, FontWeight = FontWeights.SemiBold, FontSize = 16 });
                    paragraph.Margin = new Thickness(0, text.Blocks.Count == 0 ? 0 : 12, 0, 2);
                    break;
                case NoteLineKind.Bullet:
                    paragraph.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = line.Text });
                    paragraph.Margin = new Thickness(8 + (line.Level * 16) + 14, 0, 0, 2);
                    paragraph.TextIndent = -14;
                    break;
                default:
                    paragraph.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = line.Text });
                    paragraph.Margin = new Thickness(0, 0, 0, 6);
                    break;
            }
            text.Blocks.Add(paragraph);
        }
        return text;
    }

    /// <summary>The notes area alone: a ScrollViewer around selectable text. <paramref name="notes"/> is already formatted (<see cref="ReleaseNotesText.Format"/>).</summary>
    public static ScrollViewer Notes(string notes, double maxHeight = MaxHeight) => new()
    {
        MaxHeight = Math.Min(maxHeight, HeightFor(App.Instance.MainWindow?.Content?.XamlRoot?.Size.Height ?? MaxHeight + Chrome)),
        Width = Width,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        // Focusable, so the arrow, Page, Home, and End keys scroll it.
        IsTabStop = true,
        Content = Text(notes),
    };

    public static UIElement Build(string line, string notes)
    {
        var panel = new StackPanel { Spacing = 12, Width = Width };
        panel.Children.Add(new TextBlock { Text = line, TextWrapping = TextWrapping.Wrap });
        if (notes.Length > 0)
        {
            panel.Children.Add(Notes(ReleaseNotesText.Format(notes)));
        }
        return panel;
    }
}
