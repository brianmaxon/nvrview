// Self-test step updates-notes-scroll: the update prompt's notes area, given
// the longest allowed synthetic notes (2000 characters of headings and
// bullets), is a scrollable, selectable, wrapping area that shows about 22
// lines at once in a tall window and shrinks with a small one, keeps a
// bound, styles headings and bullets, and overflows for long notes; short
// notes do not get the full height. (A ContentDialog cannot be resized by the
// user: docs/PARITY.md.) The dialog itself is not shown.

using Microsoft.UI.Text;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;

namespace NvrView.App;

internal static class UpdateNotesSelfTest
{
    public static List<string> Run()
    {
        var problems = new List<string>();
        string block = "";
        for (int n = 1; block.Length < 2000; n++)
        {
            block += $"Section {n}\n- first made-up point of section {n}\n- second made-up point, long enough that it wraps onto another line in a narrow window\n\n";
        }
        string longNotes = ReleaseNotesText.Format(block[..2000]);
        ScrollViewer view = UpdateNotesView.Notes(longNotes);
        view.Measure(new Windows.Foundation.Size(UpdateNotesView.Width, double.PositiveInfinity));
        var text = (RichTextBlock)view.Content;
        if (!text.IsTextSelectionEnabled || text.TextWrapping != Microsoft.UI.Xaml.TextWrapping.Wrap)
        {
            problems.Add("updates: the notes are not selectable and wrapping");
        }
        if (view.VerticalScrollBarVisibility == ScrollBarVisibility.Disabled || !view.IsTabStop)
        {
            problems.Add("updates: the notes area cannot be scrolled by the mouse or the keyboard");
        }
        if (view.DesiredSize.Height > UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: the notes area is taller than its bound");
        }
        // A useful amount at once: the bound holds about 20 lines of text, and a tall window gets all of it.
        if (UpdateNotesView.MaxHeight < 20 * 20 || UpdateNotesView.HeightFor(1200) != UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: the notes area is too short in a tall window");
        }
        if (UpdateNotesView.HeightFor(0) != UpdateNotesView.MinHeight || UpdateNotesView.HeightFor(600) >= UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: the notes area does not shrink with a small window");
        }
        // The text alone, measured without the bound, must need more than the area shows.
        text.Measure(new Windows.Foundation.Size(UpdateNotesView.Width, double.PositiveInfinity));
        if (text.DesiredSize.Height <= UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: 2000 characters do not overflow the notes area");
        }
        // Headings are bold and bullets hang.
        var heading = text.Blocks.OfType<Microsoft.UI.Xaml.Documents.Paragraph>().First();
        if (heading.Inlines.OfType<Microsoft.UI.Xaml.Documents.Run>().FirstOrDefault()?.FontWeight.Weight < FontWeights.SemiBold.Weight)
        {
            problems.Add("updates: a heading is not bold");
        }
        if (!text.Blocks.OfType<Microsoft.UI.Xaml.Documents.Paragraph>().Any(p => p.TextIndent < 0))
        {
            problems.Add("updates: a bullet does not hang");
        }
        ScrollViewer shortView = UpdateNotesView.Notes("One line.");
        shortView.Measure(new Windows.Foundation.Size(UpdateNotesView.Width, double.PositiveInfinity));
        if (shortView.DesiredSize.Height >= UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: short notes take the full height");
        }
        return problems;
    }
}
