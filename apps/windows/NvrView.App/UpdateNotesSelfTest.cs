// Self-test step updates-notes-scroll: the update prompt's notes area, given
// the longest allowed notes (2000 characters), is a scrollable, selectable,
// wrapping text area whose height stays bounded while the text needs more;
// short notes do not get the full height. The dialog itself is not shown.

using Microsoft.UI.Xaml.Controls;

namespace NvrView.App;

internal static class UpdateNotesSelfTest
{
    public static List<string> Run()
    {
        var problems = new List<string>();
        string longNotes = string.Concat(Enumerable.Repeat("notes. ", 300))[..2000];
        ScrollViewer view = UpdateNotesView.Notes(longNotes);
        view.Measure(new Windows.Foundation.Size(UpdateNotesView.Width, double.PositiveInfinity));
        var text = (TextBlock)view.Content;
        if (text.Text.Length != 2000)
        {
            problems.Add("updates: the notes area does not hold all 2000 characters");
        }
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
        // The text alone, measured without the bound, must need more than the area shows.
        text.Measure(new Windows.Foundation.Size(UpdateNotesView.Width, double.PositiveInfinity));
        if (text.DesiredSize.Height <= UpdateNotesView.MaxHeight)
        {
            problems.Add("updates: 2000 characters do not overflow the notes area");
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
