// Split-pane layouts (docs/UX.md "Layouts", docs/FFI.md "Layouts"): a layout
// is a binary tree whose leaves are tiles, each showing one camera or
// nothing, and whose inner nodes split their rectangle in two, side by side
// (row) or stacked (col), at a ratio. Immutable: every operation returns a
// new tree, so the designer can keep the old one and tests check invariants
// without a window. The rules match the macOS app's, so a layout either app
// stores reads the same in the other.

using System.Collections.Immutable;
using System.Text;

namespace NvrView.Shell;

/// <summary>How a split divides its rectangle.</summary>
public enum SplitAxis
{
    /// <summary>Side by side: a on the left, b on the right ("row").</summary>
    Row,

    /// <summary>Stacked: a on top, b below ("col").</summary>
    Col,
}

/// <summary>One step from a split to a child.</summary>
public enum Side
{
    A,
    B,
}

/// <summary>Where a node sits: the sides taken from the root, as a string of 'a' and 'b' (empty for the root).</summary>
public readonly record struct NodePath(string Steps)
{
    public static NodePath Root => new("");

    public int Length => (Steps ?? "").Length;

    public bool IsRoot => Length == 0;

    public NodePath Append(Side side) => new((Steps ?? "") + (side == Side.A ? 'a' : 'b'));

    public NodePath Parent => IsRoot ? this : new(Steps[..^1]);

    public Side Last => Steps[^1] == 'a' ? Side.A : Side.B;

    public Side this[int i] => Steps[i] == 'a' ? Side.A : Side.B;

    public override string ToString() => IsRoot ? "root" : Steps;

    public static NodePath Of(params Side[] sides) => new(string.Concat(sides.Select(s => s == Side.A ? 'a' : 'b')));
}

/// <summary>
/// Members of a stored node this app does not know, kept so that saving a
/// layout writes them back (a newer app's additions survive this app's
/// edits). <see cref="Node"/> belongs to the tile or split itself and stays
/// with it; <see cref="Camera"/> belongs to a leaf's camera object and goes
/// away when the tile is given another camera. Values are compact JSON text
/// with object keys sorted.
/// </summary>
public sealed class NodeExtras : IEquatable<NodeExtras>
{
    public static readonly NodeExtras None = new(ImmutableSortedDictionary<string, string>.Empty, ImmutableSortedDictionary<string, string>.Empty);

    public NodeExtras(ImmutableSortedDictionary<string, string> node, ImmutableSortedDictionary<string, string> camera)
    {
        Node = node.WithComparers(StringComparer.Ordinal);
        Camera = camera.WithComparers(StringComparer.Ordinal);
    }

    public ImmutableSortedDictionary<string, string> Node { get; }

    public ImmutableSortedDictionary<string, string> Camera { get; }

    public bool IsEmpty => Node.IsEmpty && Camera.IsEmpty;

    /// <summary>The same, without what belonged to the camera.</summary>
    public NodeExtras WithoutCamera => Camera.IsEmpty ? this : new(Node, ImmutableSortedDictionary<string, string>.Empty);

    public NodeExtras WithNode(ImmutableSortedDictionary<string, string> node) => new(node, Camera);

    public bool Equals(NodeExtras? other) =>
        other is not null && Same(Node, other.Node) && Same(Camera, other.Camera);

    private static bool Same(ImmutableSortedDictionary<string, string> x, ImmutableSortedDictionary<string, string> y) =>
        x.Count == y.Count && x.All(kv => y.TryGetValue(kv.Key, out string? v) && v == kv.Value);

    public override bool Equals(object? obj) => Equals(obj as NodeExtras);

    public override int GetHashCode() => HashCode.Combine(Node.Count, Camera.Count);
}

/// <summary>One leaf in reading order.</summary>
public readonly record struct LeafInfo(NodePath Path, CameraRef? Camera);

/// <summary>A node of a split tree: a <see cref="LayoutLeaf"/> or a <see cref="LayoutSplit"/>.</summary>
public abstract class LayoutNode : IEquatable<LayoutNode>
{
    /// <summary>The narrowest a pane may be dragged to, as a share of its split.</summary>
    public const double MinRatio = 0.1;

    public const double MaxRatio = 0.9;

    /// <summary>At most 16 splits deep (17 levels), as the core stores them.</summary>
    public const int MaxDepth = 17;

    public const int MaxLeaves = 64;

    private protected LayoutNode(NodeExtras extras) => Extras = extras;

    /// <summary>One empty tile.</summary>
    public static LayoutNode Empty { get; } = new LayoutLeaf(null, NodeExtras.None);

    /// <summary>The members of this node the app does not know.</summary>
    public NodeExtras Extras { get; }

    public bool IsLeaf => this is LayoutLeaf;

    /// <summary>The leaf's camera; null for an empty tile or a split.</summary>
    public CameraRef? Camera => (this as LayoutLeaf)?.Cam;

    public static LayoutNode Leaf(CameraRef? camera, NodeExtras? extras = null) => new LayoutLeaf(camera, extras ?? NodeExtras.None);

    public static LayoutNode Split(SplitAxis axis, double ratio, LayoutNode a, LayoutNode b, NodeExtras? extras = null) =>
        new LayoutSplit(axis, ratio, a, b, extras ?? NodeExtras.None);

    // ---- reading ----

    /// <summary>The node at <paramref name="path"/>, or null.</summary>
    public LayoutNode? this[NodePath path]
    {
        get
        {
            LayoutNode node = this;
            for (int i = 0; i < path.Length; i++)
            {
                if (node is not LayoutSplit s)
                {
                    return null;
                }
                node = path[i] == Side.A ? s.A : s.B;
            }
            return node;
        }
    }

    /// <summary>Every leaf in reading order (a before b).</summary>
    public IReadOnlyList<LeafInfo> Leaves
    {
        get
        {
            var list = new List<LeafInfo>();
            Walk(this, NodePath.Root, list);
            return list;

            static void Walk(LayoutNode n, NodePath path, List<LeafInfo> into)
            {
                switch (n)
                {
                    case LayoutLeaf l:
                        into.Add(new LeafInfo(path, l.Cam));
                        break;
                    case LayoutSplit s:
                        Walk(s.A, path.Append(Side.A), into);
                        Walk(s.B, path.Append(Side.B), into);
                        break;
                }
            }
        }
    }

    public int LeafCount => this is LayoutSplit s ? s.A.LeafCount + s.B.LeafCount : 1;

    public int Depth => this is LayoutSplit s ? 1 + Math.Max(s.A.Depth, s.B.Depth) : 1;

    /// <summary>The cameras shown, each once however many tiles show it.</summary>
    public IReadOnlySet<CameraRef> Cameras => Leaves.Where(l => l.Camera is not null).Select(l => l.Camera!.Value).ToHashSet();

    /// <summary>Every tile's camera in reading order, with repeats and empties.</summary>
    public IReadOnlyList<CameraRef?> CameraList => [.. Leaves.Select(l => l.Camera)];

    /// <summary>How many tiles show each camera (for leases).</summary>
    public Dictionary<CameraRef, int> CameraCounts()
    {
        var counts = new Dictionary<CameraRef, int>();
        foreach (LeafInfo l in Leaves)
        {
            if (l.Camera is { } c)
            {
                counts[c] = counts.GetValueOrDefault(c) + 1;
            }
        }
        return counts;
    }

    // ---- editing ----

    /// <summary>The tree with the node at <paramref name="path"/> replaced; null if there is none.</summary>
    public LayoutNode? Replacing(NodePath path, LayoutNode node) => Replace(this, path, 0, node);

    private static LayoutNode? Replace(LayoutNode at, NodePath path, int i, LayoutNode node)
    {
        if (i == path.Length)
        {
            return node;
        }
        if (at is not LayoutSplit s)
        {
            return null;
        }
        if (path[i] == Side.A)
        {
            return Replace(s.A, path, i + 1, node) is { } na ? new LayoutSplit(s.Axis, s.Ratio, na, s.B, s.Extras) : null;
        }
        return Replace(s.B, path, i + 1, node) is { } nb ? new LayoutSplit(s.Axis, s.Ratio, s.A, nb, s.Extras) : null;
    }

    /// <summary>
    /// Splits the leaf at <paramref name="path"/> in two: the old tile (with
    /// its members) moves into a, b is a new tile showing <paramref name="camera"/>
    /// (empty by default). Null if <paramref name="path"/> is not a leaf or
    /// the tree would grow too large.
    /// </summary>
    public (LayoutNode Tree, NodePath NewLeaf)? Splitting(NodePath path, SplitAxis axis, CameraRef? camera = null, double ratio = 0.5)
    {
        if (this[path] is not LayoutLeaf existing || LeafCount >= MaxLeaves || path.Length + 2 > MaxDepth)
        {
            return null;
        }
        var node = new LayoutSplit(axis, Clamp(ratio), existing, new LayoutLeaf(camera, NodeExtras.None), NodeExtras.None);
        return Replacing(path, node) is { } tree ? (tree, path.Append(Side.B)) : null;
    }

    /// <summary>
    /// Removes the leaf at <paramref name="path"/>; its sibling takes the
    /// parent's whole rectangle. Removing the only tile leaves one empty
    /// tile. Returns the new tree and the path of the node that took the space.
    /// </summary>
    public (LayoutNode Tree, NodePath Survivor)? Removing(NodePath path)
    {
        if (this[path] is not LayoutLeaf)
        {
            return null;
        }
        if (path.IsRoot)
        {
            return (Empty, NodePath.Root);
        }
        NodePath parent = path.Parent;
        Side other = path.Last == Side.A ? Side.B : Side.A;
        if (this[parent.Append(other)] is not { } sibling || Replacing(parent, sibling) is not { } tree)
        {
            return null;
        }
        return (tree, parent);
    }

    /// <summary>
    /// Swaps the cameras of two leaves; the tree's shape stays. Each camera
    /// takes its own members along; the tiles keep theirs.
    /// </summary>
    public LayoutNode? Swapping(NodePath p, NodePath q)
    {
        if (this[p] is not LayoutLeaf lp || this[q] is not LayoutLeaf lq)
        {
            return null;
        }
        if (p == q)
        {
            return this;
        }
        return Replacing(p, new LayoutLeaf(lq.Cam, new NodeExtras(lp.Extras.Node, lq.Extras.Camera)))?
            .Replacing(q, new LayoutLeaf(lp.Cam, new NodeExtras(lq.Extras.Node, lp.Extras.Camera)));
    }

    /// <summary>Shows <paramref name="camera"/> (or nothing) in the leaf at <paramref name="path"/>. The tile keeps its own members; the old camera's go with it.</summary>
    public LayoutNode? Assigning(CameraRef? camera, NodePath path)
    {
        if (this[path] is not LayoutLeaf leaf)
        {
            return null;
        }
        if (leaf.Cam == camera)
        {
            return this;
        }
        return Replacing(path, new LayoutLeaf(camera, leaf.Extras.WithoutCamera));
    }

    /// <summary>Sets the ratio of the split at <paramref name="path"/>, kept within MinRatio..MaxRatio.</summary>
    public LayoutNode? SettingRatio(double ratio, NodePath path) =>
        this[path] is LayoutSplit s ? Replacing(path, new LayoutSplit(s.Axis, Clamp(ratio), s.A, s.B, s.Extras)) : null;

    /// <summary>
    /// Sets (or with null removes) a member of the tile or split at
    /// <paramref name="path"/> that this app keeps for itself, such as a
    /// tile's stream quality; <paramref name="json"/> is the value as JSON.
    /// </summary>
    public LayoutNode? SettingMember(NodePath path, string name, string? json)
    {
        if (this[path] is not { } node || LayoutJson.IsKnownNodeMember(name, node.IsLeaf))
        {
            return null;
        }
        ImmutableSortedDictionary<string, string> members = json is null
            ? node.Extras.Node.Remove(name)
            : node.Extras.Node.SetItem(name, LayoutJson.Canonical(json));
        NodeExtras extras = node.Extras.WithNode(members);
        return Replacing(path, node switch
        {
            LayoutLeaf l => new LayoutLeaf(l.Cam, extras),
            LayoutSplit s => new LayoutSplit(s.Axis, s.Ratio, s.A, s.B, extras),
            _ => node,
        });
    }

    /// <summary>A member this app keeps on the node at <paramref name="path"/> (JSON text), or null.</summary>
    public string? Member(NodePath path, string name) => this[path]?.Extras.Node.GetValueOrDefault(name);

    /// <summary>Replaces every camera of <paramref name="device"/> with nothing (a removed device).</summary>
    public LayoutNode Clearing(string device) => this switch
    {
        LayoutLeaf l => l.Cam?.Device == device ? new LayoutLeaf(null, l.Extras.WithoutCamera) : l,
        LayoutSplit s => new LayoutSplit(s.Axis, s.Ratio, s.A.Clearing(device), s.B.Clearing(device), s.Extras),
        _ => this,
    };

    /// <summary>Renames a device key in every leaf.</summary>
    public LayoutNode Rekeying(string oldKey, string newKey) => this switch
    {
        LayoutLeaf l => l.Cam is { } c && c.Device == oldKey ? new LayoutLeaf(c.Rekeyed(newKey), l.Extras) : l,
        LayoutSplit s => new LayoutSplit(s.Axis, s.Ratio, s.A.Rekeying(oldKey, newKey), s.B.Rekeying(oldKey, newKey), s.Extras),
        _ => this,
    };

    /// <summary>The same tree with every ratio clamped into range (for trees read from elsewhere).</summary>
    public LayoutNode Normalized => this switch
    {
        LayoutSplit s => new LayoutSplit(s.Axis, Clamp(double.IsFinite(s.Ratio) ? s.Ratio : 0.5), s.A.Normalized, s.B.Normalized, s.Extras),
        _ => this,
    };

    public static double Clamp(double ratio) => Math.Min(Math.Max(ratio, MinRatio), MaxRatio);

    // ---- building ----

    /// <summary><paramref name="nodes"/> in a line along <paramref name="axis"/>, each the same size.</summary>
    public static LayoutNode Chain(SplitAxis axis, IReadOnlyList<LayoutNode> nodes)
    {
        if (nodes.Count == 0)
        {
            return Empty;
        }
        if (nodes.Count == 1)
        {
            return nodes[0];
        }
        return new LayoutSplit(axis, 1.0 / nodes.Count, nodes[0], Chain(axis, [.. nodes.Skip(1)]), NodeExtras.None);
    }

    /// <summary>A grid of equal tiles (rows of about sqrt(n) tiles, the last row possibly shorter, each row as wide as the others).</summary>
    public static LayoutNode Balanced(IEnumerable<CameraRef?> cameras)
    {
        List<CameraRef?> list = [.. cameras.Take(MaxLeaves)];
        if (list.Count == 0)
        {
            return Empty;
        }
        int columns = (int)Math.Ceiling(Math.Sqrt(list.Count));
        var rows = new List<LayoutNode>();
        for (int i = 0; i < list.Count; i += columns)
        {
            rows.Add(Chain(SplitAxis.Row, [.. list.Skip(i).Take(columns).Select(c => Leaf(c))]));
        }
        return Chain(SplitAxis.Col, rows);
    }

    public static LayoutNode Balanced(IEnumerable<CameraRef> cameras) => Balanced(cameras.Select(c => (CameraRef?)c));

    // ---- equality ----

    public bool Equals(LayoutNode? other) => (this, other) switch
    {
        (LayoutLeaf x, LayoutLeaf y) => x.Cam == y.Cam && x.Extras.Equals(y.Extras),
        (LayoutSplit x, LayoutSplit y) => x.Axis == y.Axis && x.Ratio.Equals(y.Ratio) && x.A.Equals(y.A) && x.B.Equals(y.B) && x.Extras.Equals(y.Extras),
        _ => false,
    };

    public override bool Equals(object? obj) => Equals(obj as LayoutNode);

    public override int GetHashCode() => HashCode.Combine(LeafCount, Depth);

    public static bool operator ==(LayoutNode? x, LayoutNode? y) => x is null ? y is null : x.Equals(y);

    public static bool operator !=(LayoutNode? x, LayoutNode? y) => !(x == y);

    /// <summary>The shape without cameras, for tests ("row0.5(L,L)").</summary>
    public string Shape()
    {
        var sb = new StringBuilder();
        Write(this, sb);
        return sb.ToString();

        static void Write(LayoutNode n, StringBuilder sb)
        {
            if (n is LayoutSplit s)
            {
                sb.Append(s.Axis == SplitAxis.Row ? "row" : "col").Append(s.Ratio.ToString("R", System.Globalization.CultureInfo.InvariantCulture)).Append('(');
                Write(s.A, sb);
                sb.Append(',');
                Write(s.B, sb);
                sb.Append(')');
            }
            else
            {
                sb.Append('L');
            }
        }
    }

    public override string ToString() => LayoutJson.NodeToJson(this);
}

/// <summary>A tile: one camera, or nothing.</summary>
public sealed class LayoutLeaf : LayoutNode
{
    internal LayoutLeaf(CameraRef? camera, NodeExtras extras) : base(extras) => Cam = camera;

    public CameraRef? Cam { get; }
}

/// <summary>A split of its rectangle into a and b.</summary>
public sealed class LayoutSplit : LayoutNode
{
    internal LayoutSplit(SplitAxis axis, double ratio, LayoutNode a, LayoutNode b, NodeExtras extras) : base(extras)
    {
        Axis = axis;
        Ratio = ratio;
        A = a;
        B = b;
    }

    public SplitAxis Axis { get; }

    public double Ratio { get; }

    public LayoutNode A { get; }

    public LayoutNode B { get; }
}

/// <summary>Starting points for the designer.</summary>
public enum LayoutPreset
{
    Single,
    TwoByTwo,
    OnePlusThree,
    ThreeByThree,
}

public static class LayoutPresets
{
    public static IReadOnlyList<LayoutPreset> All { get; } = [LayoutPreset.Single, LayoutPreset.TwoByTwo, LayoutPreset.OnePlusThree, LayoutPreset.ThreeByThree];

    public static string Title(this LayoutPreset p) => p switch
    {
        LayoutPreset.Single => "Single",
        LayoutPreset.TwoByTwo => "2 × 2",
        LayoutPreset.OnePlusThree => "1 + 3",
        _ => "3 × 3",
    };

    /// <summary>The preset's shape, filled with <paramref name="cameras"/> in order (empty tiles for the rest).</summary>
    public static LayoutNode Tree(this LayoutPreset p, IReadOnlyList<CameraRef>? cameras = null)
    {
        cameras ??= [];
        CameraRef? At(int i) => i < cameras.Count ? cameras[i] : null;
        LayoutNode C(int i) => LayoutNode.Leaf(At(i));
        return p switch
        {
            LayoutPreset.Single => C(0),
            LayoutPreset.TwoByTwo => LayoutNode.Balanced(Enumerable.Range(0, 4).Select(At)),
            LayoutPreset.OnePlusThree => LayoutNode.Split(SplitAxis.Row, 2.0 / 3, C(0), LayoutNode.Chain(SplitAxis.Col, [C(1), C(2), C(3)])),
            _ => LayoutNode.Balanced(Enumerable.Range(0, 9).Select(At)),
        };
    }
}
