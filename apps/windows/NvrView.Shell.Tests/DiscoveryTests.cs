using System.Text;

namespace NvrView.Shell.Tests;

/// <summary>Device discovery's events and the list (docs/FFI.md, "Device discovery"). Documentation addresses only.</summary>
internal static class DiscoveryTests
{
    private static DiscoveryEvent? Parse(string json) => DiscoveryEvent.Parse(Encoding.UTF8.GetBytes(json));

    private static DiscoveryEvent Found(ulong search, string key, string? name = null, string? model = null) =>
        new(null, new DiscoveredDevice(search, key, "nvr", name, model, "192.0.2." + key.Length, 9000, null, null, []), null);

    private static DiscoveryEvent Started(ulong search) => new(search, null, null);

    private static DiscoveryEvent Done(ulong search, int count, bool stopped, params string[] errors) =>
        new(null, null, new DiscoveryEnd(search, count, stopped, errors));

    public static void Run()
    {
        T.Run("discovery: a discovered event carries what the answer said", () =>
        {
            DiscoveredDevice d = T.NotNull(Parse(
                """{"type":"discovered","search":2,"key":"d1","kind":"nvr","name":"NVR 1","address":"192.0.2.10","port":9000,"channels":16,"services":[{"kind":"native","port":9000}],"future":1}""")?.Device);
            T.Equal(2UL, d.Search);
            T.Equal("d1", d.Key);
            T.Equal("NVR 1", d.Title);
            T.Equal(16, d.Channels);
            T.Equal(9000, d.SuggestedPort);
            T.Check(d.IsNative);
            T.Equal("192.0.2.10:9000", d.Detail);
            DiscoveredDevice cam = T.NotNull(Parse(
                """{"type":"discovered","search":2,"key":"d2","kind":"camera","model":"EXAMPLE-CAM","address":"192.0.2.20","battery_percent":80,"services":[{"kind":"onvif","port":8000,"url":"http://192.0.2.20:8000/onvif/device_service"}]}""")?.Device);
            T.Equal("Camera", cam.Title);
            T.Equal(80, cam.BatteryPercent);
            T.Check(!cam.IsNative);
            T.Equal(9000, cam.SuggestedPort);
            T.Equal("EXAMPLE-CAM · 192.0.2.20", cam.Detail);
            T.Check(Parse("""{"type":"discovered","search":2,"address":"192.0.2.20"}""") is null);
            T.Check(Parse("""{"type":"device_state","search":2}""") is null);
            T.Check(Parse("{") is null);
        });

        T.Run("discovery: started and done events", () =>
        {
            T.Equal(3UL, T.NotNull(Parse("""{"type":"discovery_started","search":3}""")?.Started));
            DiscoveryEnd end = T.NotNull(Parse("""{"type":"discovery_done","search":3,"count":2,"stopped":true,"errors":["en1: busy"]}""")?.Done);
            T.Equal(2, end.Count);
            T.Check(end.Stopped);
            T.Equal("en1: busy", end.Errors[0]);
        });

        T.Run("discovery: added means the same address and port", () =>
        {
            var d = new DiscoveredDevice(1, "d1", "nvr", null, null, "192.0.2.10", 9000, null, null, []);
            T.Check(d.IsAdded([("192.0.2.10", 9000)]));
            T.Check(!d.IsAdded([("192.0.2.10", 9001)]));
            T.Check(!d.IsAdded([("192.0.2.11", 9000)]));
        });

        T.Run("discovery list: fills, replaces by key, finishes", () =>
        {
            var list = new DiscoveryList();
            list.Requested();
            T.Check(list.IsSearching && list.Devices.Count == 0);
            list.Apply(Started(5));
            list.Apply(Found(5, "d1", "NVR 1"));
            list.Apply(Found(5, "d2"));
            list.Apply(Found(5, "d1", "NVR 1", "EXAMPLE-MODEL"));
            T.Equal("d1,d2", string.Join(",", list.Devices.Select(d => d.Key)));
            T.Equal("EXAMPLE-MODEL", list.Devices[0].Model);
            list.Apply(Done(5, 2, false));
            T.Equal(DiscoveryList.PhaseKind.Finished, list.Phase);
            T.Check(!list.FoundNothing && list.Devices.Count == 2);
        });

        T.Run("discovery list: nothing found, and refused sends point at the permission", () =>
        {
            var list = new DiscoveryList();
            list.Requested();
            list.Apply(Started(1));
            list.Apply(Done(1, 0, false, "en1: address in use"));
            T.Check(list.FoundNothing && !list.PermissionLikelyBlocked);
            T.Equal("en1: address in use", list.Errors[0]);
            list.Requested();
            list.Apply(Started(2));
            list.Apply(Done(2, 0, false, "ONVIF from 192.0.2.5: send failed: No route to host (os error 65)"));
            T.Check(list.FoundNothing && list.PermissionLikelyBlocked);
        });

        T.Run("discovery list: a restart ignores the stopped search's tail", () =>
        {
            var list = new DiscoveryList();
            list.Requested();
            list.Apply(Started(1));
            list.Apply(Found(1, "d1"));
            // Search Again twice in a row; both starts are acknowledged in order,
            // with the first one's end in between.
            list.Requested();
            list.Requested();
            T.Equal(0, list.Devices.Count);
            list.Apply(Found(1, "d9"));
            list.Apply(Done(1, 1, true));
            list.Apply(Started(2));
            list.Apply(Found(2, "d1"));
            list.Apply(Done(2, 1, true));
            list.Apply(Started(3));
            list.Apply(Found(3, "d1"));
            list.Apply(Found(3, "d2"));
            T.Equal("d1,d2", string.Join(",", list.Devices.Select(d => d.Key)));
            T.Check(list.Devices.All(d => d.Search == 3));
            T.Check(list.IsSearching);
            T.Equal(3UL, list.CurrentSearch);
        });

        T.Run("discovery list: stopping and refusal", () =>
        {
            var list = new DiscoveryList();
            list.Requested();
            list.Apply(Started(1));
            list.Stopped();
            list.Apply(Found(1, "d1"));
            list.Apply(Done(1, 0, true));
            T.Equal(DiscoveryList.PhaseKind.Idle, list.Phase);
            T.Equal(0, list.Devices.Count);
            list.FailedToStart();
            T.Check(list.Unavailable && !list.IsSearching);
        });
    }
}
