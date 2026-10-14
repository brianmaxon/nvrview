// The Export tab's decisions (docs/UX.md "Recordings", "Export tab"). The vectors are
// fixtures/synthetic/export-panel.json (copied beside the test binary), which the macOS app's NVRCoreTests
// (ExportPanelTests.swift) read too.

using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

internal static class ExportPanelTests
{
    private static JsonDocument Vectors() =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "export-panel.json")));

    private static ExportKind Kind(JsonElement e)
    {
        ExportKind? kind = ExportPanel.KindOf(e.GetString()!);
        T.Check(kind is not null, $"unknown kind {e}");
        return kind ?? ExportKind.TrimCopy;
    }

    private static bool Flag(JsonElement e, string name, bool fallback = false) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind != JsonValueKind.Null ? v.GetBoolean() : fallback;

    private static long? Number(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Number ? v.GetInt64() : null;

    private static string? Text(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    public static void Run()
    {
        T.Run("export panel: the kinds follow what is open", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("kinds").EnumerateArray())
            {
                JsonElement c = v.GetProperty("context");
                var context = new ExportContext(
                    Flag(c, "hasRange"), Flag(c, "hasPictures"), Flag(c, "editable"), Flag(c, "canWriteEdit", true),
                    (int)(Number(c, "sourceCameras") ?? 0), (int)(Number(c, "checkedCameras") ?? 0));
                string want = string.Join(",", v.GetProperty("kinds").EnumerateArray().Select(k => Kind(k).Id()));
                T.Equal(want, string.Join(",", ExportPanel.Kinds(context).Select(k => k.Id())), Text(v, "note"));
            }
        });

        T.Run("export panel: the default keeps the choice while it applies", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("defaults").EnumerateArray())
            {
                List<ExportKind> available = [.. v.GetProperty("available").EnumerateArray().Select(Kind)];
                ExportKind? current = Text(v, "current") is { } id ? ExportPanel.KindOf(id) : null;
                ExportKind? want = Text(v, "kind") is { } k ? ExportPanel.KindOf(k) : null;
                T.Equal(want, ExportPanel.DefaultKind(available, current));
            }
        });

        T.Run("export panel: options show only where they apply", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("applies").EnumerateArray())
            {
                ExportKind kind = Kind(v.GetProperty("kind"));
                T.Equal(Flag(v, "cameras"), ExportPanel.UsesCameras(kind), $"cameras for {kind}");
                T.Equal(Flag(v, "sound"), ExportPanel.UsesSound(kind), $"sound for {kind}");
                T.Equal(Flag(v, "quality"), ExportPanel.UsesQuality(kind, Flag(v, "editable")), $"quality for {kind}");
            }
        });

        T.Run("export panel: problems stop an export before it starts", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("problems").EnumerateArray())
            {
                string? got = ExportPanel.Problem(
                    Kind(v.GetProperty("kind")), Number(v, "start") ?? 0, Number(v, "end") ?? 0, (int)(Number(v, "cameras") ?? 0), Flag(v, "busy"));
                T.Equal(Text(v, "problem"), got, v.ToString());
            }
        });

        T.Run("export panel: the summary names cameras, duration and size", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("summaries").EnumerateArray())
            {
                string got = ExportPanel.Summary(
                    Kind(v.GetProperty("kind")), Number(v, "durationMs") ?? 0, (int)(Number(v, "cameras") ?? 0), Number(v, "bytes"));
                T.Equal(Text(v, "text"), got);
            }
        });

        T.Run("export panel: the size is estimated from the spans the range covers", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("estimates").EnumerateArray())
            {
                var spans = v.GetProperty("spans").EnumerateArray().Select(s => (s[0].GetInt64(), s[1].GetInt64(), s[2].GetInt64())).ToList();
                T.Equal(Number(v, "bytes"), ExportPanel.Estimate(spans, Number(v, "start") ?? 0, Number(v, "end") ?? 0));
            }
        });

        T.Run("export panel: times are parsed and shown on the day", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("times").EnumerateArray())
            {
                T.Equal(Number(v, "ms"), ExportPanel.ParseTime(Text(v, "text")!, Number(v, "dayStartMs") ?? 0), Text(v, "text"));
            }
            foreach (JsonElement v in doc.RootElement.GetProperty("timeText").EnumerateArray())
            {
                T.Equal(Text(v, "text"), ExportPanel.TimeText(Number(v, "ms") ?? 0));
            }
        });

        T.Run("export panel: the quality is the request's tier", () =>
        {
            using JsonDocument doc = Vectors();
            foreach (JsonElement v in doc.RootElement.GetProperty("quality").EnumerateArray())
            {
                ExportQuality q = Text(v, "quality") == "sub" ? ExportQuality.Sub : ExportQuality.Main;
                T.Equal(Text(v, "tier"), q.Tier());
                T.Equal(q, ExportPanel.QualityOf(Text(v, "tier")));
            }
            var camera = new RecordingCamera("dev", 1);
            string sub = ExportRequest.Range(1000, 5000, [camera], null, new ExportOptions(), ExportQuality.Sub);
            T.Check(sub.Contains("\"tier\":\"sub\"") && !sub.Contains("\"tier\":\"main\""), sub);
            T.Check(ExportRequest.Range(1000, 5000, [camera], null, new ExportOptions()).Contains("\"tier\":\"main\""));
        });
    }
}
