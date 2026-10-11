// Tests of where call cards go (NvrView.Tiles/Insets.cs). The geometry vectors are the macOS
// InsetGeometry tests' (AlertInsetTests.swift), so both apps place insets alike.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class InsetTests
{
    private static readonly CameraRef Door = new("dev-example", 3);
    private static readonly CameraRef Yard = new("dev-example", 1);

    public static IEnumerable<(string, Action)> All() =>
    [
        ("insets: stack from the corner opposite the indicators", StackFromTheOppositeCorner),
        ("insets: the Settings corner wins over the indicators' opposite", CornerChoice),
        ("insets: the picture keeps its aspect within limits", PictureHeights),
        ("insets: the controls over the stage keep clear of the insets", StagePanelAvoidsTheColumn),
        ("insets: a call shows over the stage when its camera fills it", CallOverTheStage),
    ];

    private static void StackFromTheOppositeCorner()
    {
        Check.Equal(ChromeCorner.BottomLeft, ChromeCorner.TopRight.Opposite(), "top right's opposite");
        Check.Equal(ChromeCorner.TopRight, ChromeCorner.BottomLeft.Opposite(), "bottom left's opposite");
        Check.Equal(ChromeCorner.BottomRight, ChromeCorner.TopLeft.Opposite(), "top left's opposite");
        Check.Equal(ChromeCorner.TopLeft, ChromeCorner.BottomRight.Opposite(), "bottom right's opposite");
        double width = InsetGeometry.Width(1000);
        Check.Equal(300.0, width, "30% of the stage");
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Frames(3, width, [200, 200, 200], ChromeCorner.BottomLeft, 1000, 600);
        // Two fit in 600; the third is left out (counted as more).
        Check.Equal(2, frames.Count, "frames that fit");
        Check.Equal(new InsetFrame(12, 600 - 12 - 200, 300, 200), frames[0], "newest nearest the corner");
        Check.Equal(600.0 - 12 - 200 - 8 - 200, frames[1].Y, "the next above it");
        IReadOnlyList<InsetFrame> top = InsetGeometry.Frames(1, width, [200], ChromeCorner.TopRight, 1000, 600);
        Check.Equal(new InsetFrame(1000 - 12 - 300, 12, 300, 200), top[0], "top right");
        Check.Equal(220.0, InsetGeometry.Width(400), "the minimum width");
        // The first inset always gets a frame, even on a stage too short for it.
        Check.Equal(1, InsetGeometry.Frames(2, 220, [500, 500], ChromeCorner.TopLeft, 400, 300).Count, "the first on a short stage");
    }

    private static void CornerChoice()
    {
        Check.Equal(ChromeCorner.BottomLeft, InsetGeometry.Corner(null, ChromeCorner.TopRight), "default: opposite the indicators");
        Check.Equal(ChromeCorner.TopRight, InsetGeometry.Corner(null, ChromeCorner.BottomLeft), "follows the indicators");
        Check.Equal(ChromeCorner.TopLeft, InsetGeometry.Corner(ChromeCorner.TopLeft, ChromeCorner.TopRight), "the user's corner");
        Check.Equal(ChromeCorner.TopRight, InsetGeometry.Corner(ChromeCorner.TopRight, ChromeCorner.TopRight), "even the indicators' own corner");
    }

    private static void PictureHeights()
    {
        Check.Equal(300 * 0.5625, InsetGeometry.PictureHeight(300, 0, 0), "16:9 while unknown");
        Check.Equal(300.0 * 1080 / 1920, InsetGeometry.PictureHeight(300, 1920, 1080), "the picture's aspect");
        Check.Equal(300 * 1.2, InsetGeometry.PictureHeight(300, 1000, 3000), "a tall picture is limited");
        Check.Equal(300 * 0.3, InsetGeometry.PictureHeight(300, 4000, 400), "a wide picture is limited");
    }

    private static void StagePanelAvoidsTheColumn()
    {
        InsetFrame free = InsetGeometry.StagePanel(1000, 600, 200, null);
        Check.Equal(new InsetFrame(260, 600 - 12 - 200, 480, 200), free, "centered along the bottom");
        Check.Equal(376.0, InsetGeometry.StagePanel(400, 600, 200, null).Width, "a narrow stage");
        // A column at the bottom left: the panel moves right of it.
        var left = new InsetFrame(12, 300, 300, 288);
        Check.Equal(12 + 300 + 12.0, InsetGeometry.StagePanel(1000, 600, 200, left).X, "right of a left column");
        // At the bottom right: left of it.
        var right = new InsetFrame(688, 300, 300, 288);
        Check.Equal(688 - 12 - 480.0, InsetGeometry.StagePanel(1000, 600, 200, right).X, "left of a right column");
        // At the top: no overlap, stays centered.
        Check.Equal(260.0, InsetGeometry.StagePanel(1000, 600, 200, new InsetFrame(12, 12, 300, 200)).X, "a top column");
        Check.Equal(new InsetFrame(12, 12, 300, 300), InsetGeometry.Union([new InsetFrame(12, 12, 300, 100), new InsetFrame(12, 120, 300, 192)]), "the column");
        Check.True(InsetGeometry.Union([]) is null, "no column");
    }

    private static void CallOverTheStage()
    {
        CameraRef doorLens1 = Door with { Lens = 1 };
        Check.Equal<CameraRef?>(Door, CallInsets.Filling(Door, [Door, Yard]), "the focused tile fills it");
        Check.Equal<CameraRef?>(Yard, CallInsets.Filling(null, [Yard]), "the only tile fills it");
        Check.Equal<CameraRef?>(null, CallInsets.Filling(null, [Door, Yard]), "a grid: none");
        Check.Equal<CameraRef?>(null, CallInsets.Filling(null, [null]), "an empty tile: none");
        Check.Equal<CameraRef?>(Door, CallInsets.OverStage([Door], doorLens1), "any lens of the doorbell's channel");
        Check.Equal<CameraRef?>(null, CallInsets.OverStage([Door], Yard), "another camera fills it: a card");
        Check.Equal<CameraRef?>(null, CallInsets.OverStage([Door], null), "nothing fills it: a card");
    }
}
