using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell.Tests;

/// <summary>The settings document built for the core: device keys, the offset, and credentials from the store.</summary>
internal static class AlertSettingsTests
{
    private const string Stored = """
        {"enabled":true,
         "cameras":[{"camera":{"device_key":"k-a","channel":0}},{"camera":{"device_key":"k-gone","channel":1}}],
         "sources":[{"key":"sensor-1","cameras":[{"camera":{"device_key":"k-a","channel":2},"weight":1.0}]}],
         "inputs":[{"kind":"mqtt","id":"sensor-1","username":"example-user","topic":"example/topic"}],
         "outputs":[{"kind":"desktop"},{"kind":"home_assistant","id":"ha","endpoint":"http://192.0.2.10:8123"}],
         "secrets":{"sensor-1":"from-the-file"}}
        """;

    public static void Run()
    {
        T.Run("alert settings: device keys become this run's IDs and unknown devices are left out", () =>
        {
            BuiltAlertSettings built = T.NotNull(AlertSettingsDocument.Build(
                Stored, k => k == "k-a" ? 7UL : null, -300, new MemoryAlertSecrets()));
            JsonObject doc = T.NotNull(JsonNode.Parse(built.Json)?.AsObject());
            JsonArray cameras = T.NotNull(doc["cameras"]?.AsArray());
            T.Equal(1, cameras.Count);
            T.Equal(7, (int)T.NotNull(cameras[0]?["camera"]?["device"]));
            T.Check(!built.Json.Contains("device_key", StringComparison.Ordinal));
            T.Equal(1, built.Skipped);
            T.Equal(-300, (int)T.NotNull(doc["utc_offset_min"]));
            T.Check(built.Enabled);
        });

        T.Run("alert settings: secrets come from the store, never the file", () =>
        {
            MemoryAlertSecrets secrets = new(new Dictionary<string, string>
            {
                ["sensor-1"] = "mqtt-password-example", ["ha"] = "token-example", ["unused"] = "x",
            });
            BuiltAlertSettings built = T.NotNull(AlertSettingsDocument.Build(Stored, _ => 1UL, 0, secrets));
            JsonObject found = T.NotNull(JsonNode.Parse(built.Json)?["secrets"]?.AsObject());
            T.Equal(2, found.Count);
            T.Equal("mqtt-password-example", (string)T.NotNull(found["sensor-1"]));
            T.Equal("token-example", (string)T.NotNull(found["ha"]));
            T.Check(!built.Json.Contains("from-the-file", StringComparison.Ordinal));
            BuiltAlertSettings none = T.NotNull(AlertSettingsDocument.Build(Stored, _ => 1UL, 0, new MemoryAlertSecrets()));
            T.Check(JsonNode.Parse(none.Json)?["secrets"] is null);
        });

        T.Run("alert settings: off unless enabled, and not-an-object is refused", () =>
        {
            BuiltAlertSettings off = T.NotNull(AlertSettingsDocument.Build("{}", _ => null, 0, new MemoryAlertSecrets()));
            T.Check(!off.Enabled);
            T.Check(AlertSettingsDocument.Build("[1]", _ => null, 0, new MemoryAlertSecrets()) is null);
            T.Check(AlertSettingsDocument.Build("{", _ => null, 0, new MemoryAlertSecrets()) is null);
        });
    }
}
