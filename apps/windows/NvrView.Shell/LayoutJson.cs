// Layouts as JSON (docs/FFI.md "Layouts"): what nv_layout_put takes and
// nv_layouts_get returns, read and written exactly as the macOS app does, so
// both apps share one store:
//
// - a leaf is {"camera": null} or {"camera": {"device", "channel", "lens"?}}
//   (lens written only when it is not 0, and only 0 or 1 accepted);
// - a split is {"split": "row"|"col", "ratio": 0 < r < 1, "a", "b"}; ratios
//   read are clamped to 0.1..0.9;
// - at most 17 levels and 64 tiles; id and name are required;
// - members this app does not know, anywhere, are kept and written back;
// - keys are written sorted, without spaces, like the macOS app's encoder.
//
// Reading a list skips entries that do not parse (a newer app may store
// shapes this one does not know).

using System.Collections.Immutable;
using System.Globalization;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>A layout that does not follow the rules.</summary>
public sealed class LayoutFormatException(string message) : Exception(message);

/// <summary>A named layout as the core stores it.</summary>
public sealed record SavedLayout
{
    public SavedLayout(string id, string name, LayoutNode root, int version = 0, ImmutableSortedDictionary<string, string>? extras = null)
    {
        Id = id;
        Name = name;
        Root = root;
        Version = version;
        Extras = extras ?? ImmutableSortedDictionary<string, string>.Empty.WithComparers(StringComparer.Ordinal);
    }

    public string Id { get; init; }

    public string Name { get; init; }

    /// <summary>Set by the core on every store (1, 2, ...); sent back unchanged (the core ignores it).</summary>
    public int Version { get; init; }

    public LayoutNode Root { get; init; }

    /// <summary>Members of the layout this app does not know (JSON text), written back on save.</summary>
    public ImmutableSortedDictionary<string, string> Extras { get; init; }

    /// <summary>A new id, "layout-" and 8 hex digits, like the macOS app's.</summary>
    public static string NewId() => "layout-" + Guid.NewGuid().ToString("N")[..8];

    /// <summary>The JSON nv_layout_put takes.</summary>
    public string ToJson() => LayoutJson.LayoutToJson(this);

    /// <summary>Reads what nv_layouts_get returns: {"layouts": [...]} or a bare array; entries that do not parse are skipped.</summary>
    public static List<SavedLayout> List(string json, out long revision) => LayoutJson.List(json, out revision);

    public static List<SavedLayout> List(string json) => LayoutJson.List(json, out _);

    /// <summary>One layout, or <see cref="LayoutFormatException"/>.</summary>
    public static SavedLayout Parse(string json) =>
        LayoutJson.ReadLayout(JsonNode.Parse(json) ?? throw new LayoutFormatException("null"));

    public bool Equals(SavedLayout? other) =>
        other is not null && Id == other.Id && Name == other.Name && Version == other.Version && Root == other.Root &&
        Extras.Count == other.Extras.Count && Extras.All(kv => other.Extras.TryGetValue(kv.Key, out string? v) && v == kv.Value);

    public override int GetHashCode() => HashCode.Combine(Id, Name, Version);
}

public static class LayoutJson
{
    private static readonly string[] SplitKeys = ["split", "ratio", "a", "b"];
    private static readonly string[] LeafKeys = ["camera"];
    private static readonly string[] CameraKeys = ["device", "channel", "lens"];
    private static readonly string[] LayoutKeys = ["id", "name", "version", "root"];

    private static readonly JsonWriterOptions WriterOptions = new()
    {
        // Text goes to the core, not into HTML; keep it as the other app writes it.
        Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
    };

    internal static bool IsKnownNodeMember(string name, bool leaf) =>
        leaf ? LeafKeys.Contains(name) : SplitKeys.Contains(name);

    // ---- reading ----

    public static List<SavedLayout> List(string json, out long revision)
    {
        revision = 0;
        JsonNode? doc;
        try
        {
            doc = JsonNode.Parse(json);
        }
        catch (JsonException)
        {
            return [];
        }
        JsonArray? items = doc as JsonArray;
        if (doc is JsonObject o)
        {
            items = o["layouts"] as JsonArray;
            if (o["revision"] is JsonValue r && r.TryGetValue(out long rev))
            {
                revision = rev;
            }
        }
        var list = new List<SavedLayout>();
        foreach (JsonNode? item in items ?? [])
        {
            try
            {
                list.Add(ReadLayout(item ?? throw new LayoutFormatException("null layout")));
            }
            catch (Exception e) when (e is LayoutFormatException or InvalidOperationException or FormatException or JsonException)
            {
                // Skipped: not a shape this app knows.
            }
        }
        return list;
    }

    internal static SavedLayout ReadLayout(JsonNode node)
    {
        JsonObject o = node as JsonObject ?? throw new LayoutFormatException("a layout is an object");
        string id = String(o, "id");
        string name = String(o, "name");
        int version = 0;
        if (o.TryGetPropertyValue("version", out JsonNode? v) && v is not null)
        {
            version = Int(v, "version");
        }
        LayoutNode root = ReadNode(o["root"] ?? throw new LayoutFormatException("no root")).Normalized;
        if (root.Depth > LayoutNode.MaxDepth || root.LeafCount > LayoutNode.MaxLeaves)
        {
            throw new LayoutFormatException("layout too large");
        }
        return new SavedLayout(id, name, root, version, Unknown(o, LayoutKeys));
    }

    /// <summary>A node, or <see cref="LayoutFormatException"/>.</summary>
    public static LayoutNode ReadNode(JsonNode node)
    {
        JsonObject o = node as JsonObject ?? throw new LayoutFormatException("a node is an object");
        if (o.TryGetPropertyValue("split", out JsonNode? split) && split is not null)
        {
            SplitAxis axis = split.GetValueKind() == JsonValueKind.String
                ? split.GetValue<string>() switch
                {
                    "row" => SplitAxis.Row,
                    "col" => SplitAxis.Col,
                    _ => throw new LayoutFormatException("split is row or col"),
                }
                : throw new LayoutFormatException("split is row or col");
            JsonNode ratioNode = o["ratio"] ?? throw new LayoutFormatException("no ratio");
            if (ratioNode.GetValueKind() != JsonValueKind.Number)
            {
                throw new LayoutFormatException("ratio is a number");
            }
            double ratio = ratioNode.GetValue<double>();
            if (!(ratio > 0 && ratio < 1))
            {
                throw new LayoutFormatException("ratio out of range");
            }
            return LayoutNode.Split(axis, ratio,
                ReadNode(o["a"] ?? throw new LayoutFormatException("no a")),
                ReadNode(o["b"] ?? throw new LayoutFormatException("no b")),
                new NodeExtras(Unknown(o, SplitKeys), Empty));
        }
        if (!o.TryGetPropertyValue("camera", out JsonNode? camera))
        {
            throw new LayoutFormatException("no camera or split");
        }
        ImmutableSortedDictionary<string, string> nodeExtras = Unknown(o, LeafKeys);
        if (camera is null)
        {
            return LayoutNode.Leaf(null, new NodeExtras(nodeExtras, Empty));
        }
        JsonObject c = camera as JsonObject ?? throw new LayoutFormatException("camera is an object or null");
        string device = String(c, "device");
        int channel = Int(c["channel"] ?? throw new LayoutFormatException("no channel"), "channel");
        int lens = 0;
        if (c.TryGetPropertyValue("lens", out JsonNode? l) && l is not null)
        {
            lens = Int(l, "lens");
        }
        if (lens is < 0 or > CameraRef.MaxLens)
        {
            throw new LayoutFormatException("lens out of range");
        }
        return LayoutNode.Leaf(new CameraRef(device, channel, lens), new NodeExtras(nodeExtras, Unknown(c, CameraKeys)));
    }

    private static ImmutableSortedDictionary<string, string> Empty => ImmutableSortedDictionary<string, string>.Empty.WithComparers(StringComparer.Ordinal);

    private static string String(JsonObject o, string key) =>
        o[key] is JsonValue v && v.GetValueKind() == JsonValueKind.String ? v.GetValue<string>() : throw new LayoutFormatException($"{key} is a string");

    private static int Int(JsonNode n, string key)
    {
        if (n is JsonValue v && v.GetValueKind() == JsonValueKind.Number && v.TryGetValue(out JsonElement e) && e.TryGetInt32(out int i))
        {
            return i;
        }
        if (n is JsonValue w && w.TryGetValue(out int j) && w.GetValueKind() == JsonValueKind.Number)
        {
            return j;
        }
        throw new LayoutFormatException($"{key} is an integer");
    }

    private static ImmutableSortedDictionary<string, string> Unknown(JsonObject o, string[] known)
    {
        ImmutableSortedDictionary<string, string>.Builder b = Empty.ToBuilder();
        foreach (KeyValuePair<string, JsonNode?> kv in o)
        {
            if (!known.Contains(kv.Key))
            {
                b[kv.Key] = Canonical(kv.Value);
            }
        }
        return b.ToImmutable();
    }

    /// <summary>Compact JSON text with object keys sorted (numbers kept as written).</summary>
    public static string Canonical(string json) => Canonical(JsonNode.Parse(json));

    private static string Canonical(JsonNode? node)
    {
        using var stream = new MemoryStream();
        using (var w = new Utf8JsonWriter(stream, WriterOptions))
        {
            WriteSorted(w, node);
        }
        return Encoding.UTF8.GetString(stream.ToArray());
    }

    private static void WriteSorted(Utf8JsonWriter w, JsonNode? node)
    {
        switch (node)
        {
            case null:
                w.WriteNullValue();
                break;
            case JsonObject o:
                w.WriteStartObject();
                foreach (KeyValuePair<string, JsonNode?> kv in o.OrderBy(kv => kv.Key, StringComparer.Ordinal))
                {
                    w.WritePropertyName(kv.Key);
                    WriteSorted(w, kv.Value);
                }
                w.WriteEndObject();
                break;
            case JsonArray a:
                w.WriteStartArray();
                foreach (JsonNode? item in a)
                {
                    WriteSorted(w, item);
                }
                w.WriteEndArray();
                break;
            default:
                node.WriteTo(w);
                break;
        }
    }

    // ---- writing ----

    public static string LayoutToJson(SavedLayout layout) => Write(w =>
    {
        // Sorted: extras merged with id, name, root, version.
        var members = new SortedDictionary<string, Action>(StringComparer.Ordinal);
        foreach (KeyValuePair<string, string> kv in layout.Extras)
        {
            if (!LayoutKeys.Contains(kv.Key))
            {
                members[kv.Key] = () => w.WriteRawValue(kv.Value);
            }
        }
        members["id"] = () => w.WriteStringValue(layout.Id);
        members["name"] = () => w.WriteStringValue(layout.Name);
        members["version"] = () => w.WriteNumberValue(layout.Version);
        members["root"] = () => WriteNode(w, layout.Root);
        WriteObject(w, members);
    });

    public static string NodeToJson(LayoutNode node) => Write(w => WriteNode(w, node));

    private static string Write(Action<Utf8JsonWriter> body)
    {
        using var stream = new MemoryStream();
        using (var w = new Utf8JsonWriter(stream, WriterOptions))
        {
            body(w);
        }
        return Encoding.UTF8.GetString(stream.ToArray());
    }

    private static void WriteObject(Utf8JsonWriter w, SortedDictionary<string, Action> members)
    {
        w.WriteStartObject();
        foreach (KeyValuePair<string, Action> kv in members)
        {
            w.WritePropertyName(kv.Key);
            kv.Value();
        }
        w.WriteEndObject();
    }

    private static void WriteNode(Utf8JsonWriter w, LayoutNode node)
    {
        var members = new SortedDictionary<string, Action>(StringComparer.Ordinal);
        switch (node)
        {
            case LayoutLeaf leaf:
                AddExtras(members, w, leaf.Extras.Node, LeafKeys);
                if (leaf.Cam is { } c)
                {
                    members["camera"] = () =>
                    {
                        var cam = new SortedDictionary<string, Action>(StringComparer.Ordinal);
                        AddExtras(cam, w, leaf.Extras.Camera, CameraKeys);
                        cam["device"] = () => w.WriteStringValue(c.Device);
                        cam["channel"] = () => w.WriteNumberValue(c.Channel);
                        if (c.Lens != 0)
                        {
                            cam["lens"] = () => w.WriteNumberValue(c.Lens);
                        }
                        WriteObject(w, cam);
                    };
                }
                else
                {
                    members["camera"] = w.WriteNullValue;
                }
                break;
            case LayoutSplit s:
                AddExtras(members, w, s.Extras.Node, SplitKeys);
                members["split"] = () => w.WriteStringValue(s.Axis == SplitAxis.Row ? "row" : "col");
                // Shortest text that reads back as the same double, as the macOS encoder writes it.
                members["ratio"] = () => w.WriteRawValue(s.Ratio.ToString("R", CultureInfo.InvariantCulture));
                members["a"] = () => WriteNode(w, s.A);
                members["b"] = () => WriteNode(w, s.B);
                break;
        }
        WriteObject(w, members);
    }

    private static void AddExtras(SortedDictionary<string, Action> members, Utf8JsonWriter w, ImmutableSortedDictionary<string, string> extras, string[] known)
    {
        foreach (KeyValuePair<string, string> kv in extras)
        {
            if (!known.Contains(kv.Key))
            {
                members[kv.Key] = () => w.WriteRawValue(kv.Value);
            }
        }
    }
}
