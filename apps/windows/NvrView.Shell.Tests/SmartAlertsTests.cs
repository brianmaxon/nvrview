using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// Smart alerts (minor 12): the events as the core writes them, the
/// detector's answer, and FakeSmartAlerts' scripted verifications from a
/// trigger to a logged alert.
/// </summary>
internal static class SmartAlertsTests
{
    private static NvStatus StatusOf(Action call)
    {
        try
        {
            call();
            return NvStatus.Ok;
        }
        catch (NvException e)
        {
            return e.Status;
        }
    }

    public static void Run()
    {
        T.Run("smart alerts: the verify_request event", () =>
        {
            const string json = """{"type":"verify_request","request":"v3","purpose":"check","camera":{"device":1,"channel":2,"lens":0},"trigger_ids":[12],"codec":"h264","width":1280,"height":720,"pts_us":2000000,"bytes":5120,"frames":[{"pts_us":1000000,"bytes":5000,"keyframe":true},{"pts_us":2000000,"bytes":120,"keyframe":false}],"from_cover":false,"at_ms":1718900000250,"deadline_ms":1718900005250,"classes":["person"],"min_score":0.5,"crops":[{"what":"frame","x":0,"y":0,"w":1,"h":1}],"future":1}""";
            SmartAlertEvent e = T.NotNull(SmartAlertEvent.Parse(json));
            T.Equal("verify_request", e.Type);
            T.Equal("v3", e.Request);
            using JsonDocument d = JsonDocument.Parse(json);
            VerifyRequest r = T.NotNull(VerifyRequest.Parse(d.RootElement));
            T.Equal((1UL, 2, 0), (r.Device, r.Channel, r.Lens));
            T.Equal("5000,120", string.Join(",", r.FrameSizes));
            T.Equal(r.Bytes, r.FrameSizes.Sum());
            T.Equal(12UL, r.TriggerIds[0]);
            T.Equal("person", string.Join(",", r.Classes));
            T.Equal(null, SmartAlertEvent.Parse("""{"type":"recording"}"""));
            SmartAlertEvent a = T.NotNull(SmartAlertEvent.Parse("""{"type":"alert","state":"new","decision":{"trigger_id":7}}"""));
            T.Equal(("new", (ulong?)7), (a.State, a.TriggerId));
            SmartAlertEvent done = T.NotNull(SmartAlertEvent.Parse("""{"type":"verify_request_done","request":"v3","result":"timeout"}"""));
            T.Equal("timeout", done.State);
        });

        T.Run("smart alerts: the detector's answer", () =>
        {
            using JsonDocument d = JsonDocument.Parse(AlertDetection.Json([new AlertDetection("car", 0.75, 0.5, 0.25, 0.125, 0.5)]));
            JsonElement e = d.RootElement.GetProperty("detections")[0];
            T.Equal("car", e.GetProperty("label").GetString());
            T.Equal(0.125, e.GetProperty("box").GetProperty("w").GetDouble());
            T.Check(NvNative.AbiMinor >= 12);
        });

        T.Run("smart alerts: the fake from a trigger to a logged alert", () =>
        {
            var events = new List<string>();
            var fake = new FakeSmartAlerts(e => { lock (events) { events.Add(e); } }) { AlertWorkIsManual = true };
            ISmartAlerts core = fake;
            List<SmartAlertEvent> Of(string type)
            {
                lock (events)
                {
                    return [.. events.Select(SmartAlertEvent.Parse).OfType<SmartAlertEvent>().Where(e => e.Type == type)];
                }
            }
            const string trigger = """{"source":"device","camera":{"device":1,"channel":0},"kind":"people"}""";
            T.Equal(NvStatus.NotConfigured, StatusOf(() => core.AlertsTriggerPut(trigger)));
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => core.AlertsConfigure("{")));
            core.AlertsConfigure("{}");
            ulong id = core.AlertsTriggerPut(trigger);
            fake.CompleteAlertWork();
            List<SmartAlertEvent> requests = Of("verify_request");
            T.Equal(2, requests.Count);
            foreach (SmartAlertEvent r in requests)
            {
                AlertFrames f = core.AlertFramesGet(r.Request!);
                T.Equal(NvThumbKind.H264Keyframe, f.Kind);
                core.AlertDetectionsPut(r.Request!, AlertDetection.Json([new AlertDetection("person", 0.8, 0.4, 0.3, 0.1, 0.4)]));
                T.Equal(NvStatus.UnknownId, StatusOf(() => core.AlertDetectionsPut(r.Request!, """{"detections":[]}""")));
            }
            fake.CompleteAlertWork();
            SmartAlertEvent alert = Of("alert")[^1];
            T.Equal(("new", (ulong?)id), (alert.State, alert.TriggerId));
            using JsonDocument ad = JsonDocument.Parse(alert.Json);
            ulong alertId = ad.RootElement.GetProperty("decision").GetProperty("alert_id").GetUInt64();

            core.AlertSnapshotPut(alertId, [0xFF, 0xD8, 1, 0xFF, 0xD9]);
            T.Equal(5, core.AlertSnapshotGet(alertId).Length);
            T.Equal(NvStatus.UnknownId, StatusOf(() => core.AlertSnapshotPut(alertId + 9, [1])));
            core.AlertFeedback($$"""{"alert_id":{{alertId}},"vote":"down","reason":"nothing_there"}""");
            T.Equal(NvStatus.InvalidArgument, StatusOf(() => core.AlertFeedback($$"""{"alert_id":{{alertId}},"vote":"up","reason":"duplicate"}""")));
            string log = core.AlertsGet("""{"decision":["alert"]}""");
            T.Check(log.Contains("\"snapshot\":true", StringComparison.Ordinal) && log.Contains("nothing_there", StringComparison.Ordinal));

            core.AlertMute("""{"device":1,"channel":0}""", 600);
            core.AlertsTriggerPut(trigger);
            fake.CompleteAlertWork();
            using JsonDocument md = JsonDocument.Parse(Of("alert")[^1].Json);
            T.Equal("muted", md.RootElement.GetProperty("decision").GetProperty("reason").GetString());

            // No answer: fails open only for always-alert cameras.
            fake.AlwaysAlertCameras.Add((1, 1, 0));
            core.AlertMute("""{"device":1,"channel":0}""", 0);
            core.AlertsTriggerPut("""{"source":"device","camera":{"device":1,"channel":1},"kind":"people"}""");
            core.AlertsTriggerPut("""{"source":"device","camera":{"device":1,"channel":2},"kind":"people"}""");
            fake.ExpireRequests();
            fake.CompleteAlertWork();
            List<string> last = [.. Of("alert").TakeLast(2).Select(e => e.Json)];
            T.Check(last.Any(j => j.Contains("\"unverified\":true", StringComparison.Ordinal) && j.Contains("critical", StringComparison.Ordinal)));
            T.Check(last.Any(j => j.Contains("could_not_look", StringComparison.Ordinal)));
        });
    }
}
