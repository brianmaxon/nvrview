// Tile identities in a layout and the window's views keyed by tile (docs/UX.md "Digital zoom", "Fill Tiles"):
// each leaf keeps its identity in its "tile" member (stored with the layout; a member of the tile, so it stays in
// place through swaps and camera changes, and with the old tile through a split). The macOS app's rules
// (NVRCore TileViews.swift), with the shared vectors in fixtures/synthetic/tile-views.json.

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell;

public static class TileIdentity
{
    /// <summary>The leaf's tile identity (null for a split or a leaf without a usable one).</summary>
    public static string? TileId(this LayoutNode node)
    {
        if (node is not LayoutLeaf || !node.Extras.Node.TryGetValue(TileIds.Member, out string? json))
        {
            return null;
        }
        try
        {
            return JsonNode.Parse(json) is JsonValue v && v.TryGetValue(out string? s) && TileIds.Valid(s) ? s : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>The tree with every leaf holding a tile identity of its own (<see cref="TileIds.Assign"/>); the same tree when they all do.</summary>
    public static LayoutNode WithTileIds(this LayoutNode tree)
    {
        IReadOnlyList<LeafInfo> leaves = tree.Leaves;
        var have = leaves.Select(l => tree[l.Path]?.TileId()).ToList();
        List<string> ids = TileIds.Assign(have);
        LayoutNode result = tree;
        for (int i = 0; i < leaves.Count; i++)
        {
            if (have[i] == ids[i] || result[leaves[i].Path] is not LayoutLeaf leaf)
            {
                continue;
            }
            var extras = leaf.Extras.WithNode(leaf.Extras.Node.SetItem(TileIds.Member, JsonValue.Create(ids[i])!.ToJsonString()));
            result = result.Replacing(leaves[i].Path, new LayoutLeaf(leaf.Cam, extras)) ?? result;
        }
        return result;
    }

    /// <summary>The view key of the tile at <paramref name="path"/> in the content <paramref name="layout"/>; null without a camera or an identity.</summary>
    public static TileViewKey? TileKeyAt(this LayoutNode tree, NodePath path, string layout) =>
        tree[path] is LayoutLeaf { Cam: { } camera } leaf && leaf.TileId() is { } id ? new TileViewKey(layout, id, camera) : null;

    /// <summary>The view keys of every tile with a camera, in reading order.</summary>
    public static List<TileViewKey> TileKeys(this LayoutNode tree, string layout) =>
        [.. tree.Leaves.Select(l => tree.TileKeyAt(l.Path, layout)).OfType<TileViewKey>()];

    /// <summary>Forgets the views of <paramref name="layout"/> that no tile of <paramref name="tree"/> has (a tile removed, or given another camera); other layouts' are kept.</summary>
    public static void Prune(this TileViews<TileViewKey> views, string layout, LayoutNode tree)
    {
        var keep = tree.TileKeys(layout).ToHashSet();
        views.RemoveAll(k => k.Layout == layout && !keep.Contains(k));
    }

    /// <summary>Before the tiles at <paramref name="p"/> and <paramref name="q"/> of <paramref name="tree"/> swap cameras: each view moves with its camera to the other tile.</summary>
    public static void SwapViews(this TileViews<TileViewKey> views, string layout, LayoutNode tree, NodePath p, NodePath q)
    {
        if (p == q || tree[p]?.TileId() is not { } ip || tree[q]?.TileId() is not { } iq)
        {
            return;
        }
        var moves = new List<(TileViewKey, TileViewKey)>();
        if (tree[p]?.Camera is { } cp)
        {
            moves.Add((new TileViewKey(layout, ip, cp), new TileViewKey(layout, iq, cp)));
        }
        if (tree[q]?.Camera is { } cq)
        {
            moves.Add((new TileViewKey(layout, iq, cq), new TileViewKey(layout, ip, cq)));
        }
        views.Move(moves);
    }

    /// <summary>The views of a file written when they were kept per camera (windows.json "tileViews"): each camera's view goes to every tile of <paramref name="tree"/> showing it, in reading order.</summary>
    public static void Migrate(this TileViews<TileViewKey> views, IEnumerable<(CameraRef Camera, TileView View)> legacy, string layout, LayoutNode tree)
    {
        var byCamera = new Dictionary<CameraRef, TileView>();
        foreach ((CameraRef camera, TileView view) in legacy)
        {
            byCamera[camera] = view;
        }
        foreach (TileViewKey key in tree.TileKeys(layout))
        {
            if (byCamera.TryGetValue(key.Camera, out TileView v))
            {
                views.Restore(key, v);
            }
        }
    }
}
