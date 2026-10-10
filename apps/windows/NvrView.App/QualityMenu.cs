// The Quality submenu of a tile's context menu (docs/UX.md "Stream quality
// per tile"): Auto, Clear, Fluent, and Balanced only where the camera serves a
// middle stream, a check mark on the tile's choice, and the note on what the
// main stream costs. Self-contained, so any window's tile menu can host it:
//
//   menu.Items.Add(QualityMenu.Create(tile.Quality, viewer.OffersBalanced(tile),
//       q => viewer.SetQuality(tile, q)));

using Microsoft.UI.Xaml.Controls;
using NvrView.Tiles;

namespace NvrView.App;

internal static class QualityMenu
{
    /// <summary>The "Quality" submenu.</summary>
    /// <param name="current">The tile's choice.</param>
    /// <param name="offersBalanced">Whether the camera serves a middle stream (<see cref="Viewer.OffersBalanced"/>).</param>
    /// <param name="chosen">Called with the new choice.</param>
    public static MenuFlyoutSubItem Create(TileQuality current, bool offersBalanced, Action<TileQuality> chosen)
    {
        var sub = new MenuFlyoutSubItem { Text = "Quality" };
        foreach (TileQuality q in Enum.GetValues<TileQuality>())
        {
            if (q == TileQuality.Balanced && !offersBalanced)
            {
                continue;
            }
            var item = new RadioMenuFlyoutItem { Text = q.Title(), GroupName = "tile-quality", IsChecked = q == current };
            TileQuality choice = q;
            item.Click += (_, _) => chosen(choice);
            sub.Items.Add(item);
        }
        sub.Items.Add(new MenuFlyoutSeparator());
        sub.Items.Add(new MenuFlyoutItem { Text = TileQualities.CostNote, IsEnabled = false });
        return sub;
    }

    /// <summary>The submenu's item texts for a camera (for tests and the self-test).</summary>
    public static IReadOnlyList<string> Choices(bool offersBalanced) =>
        [.. Enum.GetValues<TileQuality>().Where(q => q != TileQuality.Balanced || offersBalanced).Select(q => q.Title())];
}
