// What the app keeps between launches besides layouts (which the core
// stores): the camera groups and the open windows, as small JSON files in the
// app's data directory, each written whole and atomically. No secrets and no
// device addresses go in here: only ids, names, device keys, channel numbers,
// layout trees, and window frames.

using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>Writes a file whole and atomically (temporary file, flush, rename); reads it back.</summary>
public static class JsonFile
{
    public static void Write(string path, string text)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        string temp = path + ".tmp";
        using (FileStream s = File.Create(temp))
        {
            byte[] bytes = Encoding.UTF8.GetBytes(text);
            s.Write(bytes);
            s.Flush(flushToDisk: true);
        }
        File.Move(temp, path, overwrite: true);
    }

    /// <summary>The parsed file, or null when it is missing; an unreadable file is kept as *.unreadable.</summary>
    public static JsonNode? Read(string path)
    {
        if (!File.Exists(path))
        {
            return null;
        }
        try
        {
            return JsonNode.Parse(File.ReadAllText(path));
        }
        catch (Exception e) when (e is JsonException or IOException)
        {
            try
            {
                File.Move(path, path + ".unreadable", overwrite: true);
            }
            catch (IOException)
            {
            }
            return null;
        }
    }

    internal static JsonObject CameraJson(CameraRef c)
    {
        var o = new JsonObject { ["channel"] = c.Channel, ["device"] = c.Device };
        if (c.Lens != 0)
        {
            o["lens"] = c.Lens;
        }
        return o;
    }

    internal static CameraRef? CameraFrom(JsonNode? n)
    {
        if (n is not JsonObject o || o["device"] is not JsonValue d || d.GetValueKind() != JsonValueKind.String
            || o["channel"] is not JsonValue ch || !ch.TryGetValue(out int channel))
        {
            return null;
        }
        int lens = o["lens"] is JsonValue l && l.TryGetValue(out int v) ? v : 0;
        return lens is < 0 or > CameraRef.MaxLens ? null : new CameraRef(d.GetValue<string>(), channel, lens);
    }

    internal static readonly JsonSerializerOptions Pretty = new() { WriteIndented = true };
}

/// <summary>A named set of cameras; showing a group lays them out as an even grid.</summary>
public sealed record CameraGroup(string Id, string Name, IReadOnlyList<CameraRef> Cameras)
{
    /// <summary>A new id, "group-" and 8 hex digits, like the macOS app's.</summary>
    public static string NewId() => "group-" + Guid.NewGuid().ToString("N")[..8];

    public bool Equals(CameraGroup? other) =>
        other is not null && Id == other.Id && Name == other.Name && Cameras.SequenceEqual(other.Cameras);

    public override int GetHashCode() => HashCode.Combine(Id, Name, Cameras.Count);
}

/// <summary>groups.json (the macOS app's format: an array of {cameras, id, name}), or memory only when the path is null.</summary>
public sealed class GroupStore(string? path)
{
    public List<CameraGroup> Load()
    {
        if (path is null || JsonFile.Read(path) is not JsonArray items)
        {
            return [];
        }
        var list = new List<CameraGroup>();
        foreach (JsonNode? item in items)
        {
            if (item is JsonObject o && o["id"] is JsonValue id && id.GetValueKind() == JsonValueKind.String
                && o["name"] is JsonValue name && name.GetValueKind() == JsonValueKind.String)
            {
                List<CameraRef> cameras = [.. (o["cameras"] as JsonArray ?? []).Select(JsonFile.CameraFrom).OfType<CameraRef>()];
                list.Add(new CameraGroup(id.GetValue<string>(), name.GetValue<string>(), cameras));
            }
        }
        return list;
    }

    public static string ToJson(IEnumerable<CameraGroup> groups)
    {
        var array = new JsonArray();
        foreach (CameraGroup g in groups)
        {
            var cameras = new JsonArray();
            foreach (CameraRef c in g.Cameras)
            {
                cameras.Add((JsonNode)JsonFile.CameraJson(c));
            }
            array.Add((JsonNode)new JsonObject { ["cameras"] = cameras, ["id"] = g.Id, ["name"] = g.Name });
        }
        return array.ToJsonString(JsonFile.Pretty);
    }

    public void Save(IEnumerable<CameraGroup> groups)
    {
        if (path is not null)
        {
            JsonFile.Write(path, ToJson(groups));
        }
    }
}

/// <summary>What a window shows.</summary>
public abstract record WindowContent
{
    private WindowContent()
    {
    }

    /// <summary>A layout the core stores; edits are saved back to it.</summary>
    public sealed record Layout(string Id) : WindowContent;

    /// <summary>A group's cameras as an even grid; edits stay in the window until saved as a layout.</summary>
    public sealed record Group(string Id) : WindowContent;

    /// <summary>An unsaved arrangement.</summary>
    public sealed record Scratch(string Name, LayoutNode Root) : WindowContent;

    internal JsonObject ToJson() => this switch
    {
        Layout l => new JsonObject { ["layout"] = new JsonObject { ["id"] = l.Id } },
        Group g => new JsonObject { ["group"] = new JsonObject { ["id"] = g.Id } },
        Scratch s => new JsonObject { ["scratch"] = new JsonObject { ["name"] = s.Name, ["root"] = JsonNode.Parse(LayoutJson.NodeToJson(s.Root)) } },
        _ => [],
    };

    internal static WindowContent? From(JsonNode? n)
    {
        if (n is not JsonObject o)
        {
            return null;
        }
        if (o["layout"]?["id"] is JsonValue lid && lid.GetValueKind() == JsonValueKind.String)
        {
            return new Layout(lid.GetValue<string>());
        }
        if (o["group"]?["id"] is JsonValue gid && gid.GetValueKind() == JsonValueKind.String)
        {
            return new Group(gid.GetValue<string>());
        }
        if (o["scratch"] is JsonObject s && s["name"] is JsonValue name && name.GetValueKind() == JsonValueKind.String && s["root"] is { } root)
        {
            try
            {
                LayoutNode tree = LayoutJson.ReadNode(root).Normalized;
                return tree.Depth <= LayoutNode.MaxDepth && tree.LeafCount <= LayoutNode.MaxLeaves ? new Scratch(name.GetValue<string>(), tree) : null;
            }
            catch (LayoutFormatException)
            {
                return null;
            }
        }
        return null;
    }
}

/// <summary>One window as it is remembered between launches.</summary>
public sealed record SavedWindow(WindowContent Content, Rect Frame, bool FullScreen = false, bool SidebarCollapsed = false, double SidebarWidth = 0, double? InsetX = null, double? InsetY = null)
{
    /// <summary>
    /// Where to put a restored window (screen pixels, origin top left): its
    /// saved frame if enough of its title bar (the top 32 pixels, at least 80
    /// wide) is on one of the <paramref name="screens"/> (work areas) to grab
    /// it, else the same size (shrunk to fit) centered on the first screen;
    /// null without screens. Never smaller than the minimum.
    /// </summary>
    public static Rect? Placement(Rect frame, IReadOnlyList<Rect> screens, double minWidth, double minHeight)
    {
        if (screens.Count == 0)
        {
            return null;
        }
        Rect main = screens[0];
        var f = new Rect(frame.X, frame.Y, Math.Max(frame.Width, minWidth), Math.Max(frame.Height, minHeight));
        var title = new Rect(f.X, f.Y, f.Width, 32);
        bool grabbable = double.IsFinite(f.X) && double.IsFinite(f.Y) && double.IsFinite(f.Width) && double.IsFinite(f.Height) && screens.Any(s =>
        {
            double w = Math.Min(s.Right, title.Right) - Math.Max(s.X, title.X);
            double h = Math.Min(s.Bottom, title.Bottom) - Math.Max(s.Y, title.Y);
            return w >= 80 && h >= 10;
        });
        if (grabbable)
        {
            return f;
        }
        double fw = Math.Min(double.IsFinite(f.Width) ? f.Width : main.Width, main.Width);
        double fh = Math.Min(double.IsFinite(f.Height) ? f.Height : main.Height, main.Height);
        return new Rect(Math.Round(main.MidX - fw / 2), Math.Round(main.MidY - fh / 2), fw, fh);
    }
}

/// <summary>windows.json, or memory only when the path is null.</summary>
public sealed class WindowStore(string? path)
{
    public static string ToJson(IEnumerable<SavedWindow> windows)
    {
        var array = new JsonArray();
        foreach (SavedWindow w in windows)
        {
            var o = new JsonObject
            {
                ["content"] = w.Content.ToJson(),
                ["frame"] = new JsonObject { ["x"] = w.Frame.X, ["y"] = w.Frame.Y, ["width"] = w.Frame.Width, ["height"] = w.Frame.Height },
                ["fullScreen"] = w.FullScreen,
                ["sidebarCollapsed"] = w.SidebarCollapsed,
            };
            if (w.SidebarWidth > 0)
            {
                o["sidebarWidth"] = w.SidebarWidth;
            }
            if (w.InsetX is { } ix && w.InsetY is { } iy && double.IsFinite(ix) && double.IsFinite(iy))
            {
                // Where the user dragged the window's call insets (a fraction of the free space);
                // left out while they sit in the default corner. The macOS app's key.
                o["insetAnchor"] = new JsonObject { ["x"] = ix, ["y"] = iy };
            }
            array.Add((JsonNode)o);
        }
        return array.ToJsonString(JsonFile.Pretty);
    }

    /// <summary>Reads windows; entries that do not parse are skipped.</summary>
    public static List<SavedWindow> FromJson(JsonNode? doc)
    {
        var list = new List<SavedWindow>();
        foreach (JsonNode? item in doc as JsonArray ?? [])
        {
            if (item is not JsonObject o || WindowContent.From(o["content"]) is not { } content || o["frame"] is not JsonObject f)
            {
                continue;
            }
            static double D(JsonNode? n) => n is JsonValue v && v.TryGetValue(out double d) ? d : double.NaN;
            static bool B(JsonNode? n) => n is JsonValue v && v.TryGetValue(out bool b) && b;
            var frame = new Rect(D(f["x"]), D(f["y"]), D(f["width"]), D(f["height"]));
            double width = D(o["sidebarWidth"]);
            double? insetX = null, insetY = null;
            if (o["insetAnchor"] is JsonObject a && double.IsFinite(D(a["x"])) && double.IsFinite(D(a["y"])))
            {
                insetX = Math.Clamp(D(a["x"]), 0, 1);
                insetY = Math.Clamp(D(a["y"]), 0, 1);
            }
            list.Add(new SavedWindow(content, frame, B(o["fullScreen"]), B(o["sidebarCollapsed"]), double.IsFinite(width) ? width : 0, insetX, insetY));
        }
        return list;
    }

    public List<SavedWindow> Load() => path is null ? [] : FromJson(JsonFile.Read(path));

    public void Save(IEnumerable<SavedWindow> windows)
    {
        if (path is not null)
        {
            JsonFile.Write(path, ToJson(windows));
        }
    }
}

/// <summary>Which channels the sidebar lists.</summary>
public static class ChannelFilter
{
    /// <summary>The channels to list: all, or only connected ones when <paramref name="hideOffline"/>.</summary>
    public static IEnumerable<T> Visible<T>(IEnumerable<T> channels, Func<T, bool> connected, bool hideOffline) =>
        hideOffline ? channels.Where(connected) : channels;

    /// <summary>"3 of 8" when some are hidden, "8" otherwise, nothing for none.</summary>
    public static string? CountText(int shown, int total) =>
        total == 0 ? null : shown == total ? $"{total}" : $"{shown} of {total}";
}
