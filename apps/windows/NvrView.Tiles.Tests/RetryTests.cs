// Tests of the Retry button's rules (NvrView.Tiles/Retry.cs) on a manual clock: nothing waits.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class RetryTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("retry: only messages a retry can help", Applicable),
        ("retry: the button appears after 30 s on the viewer clock", AppearsAfterTheDelay),
        ("retry: a picture or a final state resets the delay", ResetByPictureOrFinalState),
        ("retry: pressing it starts the delay again; tiles are independent", RestartAndIndependence),
    ];

    private static void Applicable()
    {
        foreach (TileMessage m in new[]
            { TileMessage.Connecting, TileMessage.Reconnecting, TileMessage.Starting, TileMessage.WaitingForCamera, TileMessage.Refused, TileMessage.Recovering })
        {
            Check.True(TileRetryRule.Applies(m), $"{m} can be helped");
        }
        foreach (TileMessage m in new[]
            { TileMessage.None, TileMessage.LoginFailed, TileMessage.NoPassword, TileMessage.InvalidAddress, TileMessage.Disconnected,
              TileMessage.DeviceRemoved, TileMessage.CameraOffline, TileMessage.Undecodable })
        {
            Check.True(!TileRetryRule.Applies(m), $"{m} is not helped by a retry");
        }
    }

    private static void AppearsAfterTheDelay()
    {
        var clock = new ManualViewerClock(1000);
        var watch = new TileRetryWatch<int>();
        Check.True(!watch.Offered(1, TileMessage.Starting, clock.Now), "not at once");
        clock.Advance(TileRetryRule.OfferAfter - 0.5);
        Check.True(!watch.Offered(1, TileMessage.Starting, clock.Now), "not just before");
        clock.Advance(0.5);
        Check.True(watch.Offered(1, TileMessage.Starting, clock.Now), "at 30 s");
        // A different message a retry can help keeps the same delay running.
        clock.Advance(600);
        Check.True(watch.Offered(1, TileMessage.Reconnecting, clock.Now), "still offered");
    }

    private static void ResetByPictureOrFinalState()
    {
        var clock = new ManualViewerClock(0);
        var watch = new TileRetryWatch<int>();
        watch.Offered(1, TileMessage.Starting, clock.Now);
        clock.Advance(40);
        Check.True(watch.Offered(1, TileMessage.Starting, clock.Now), "offered");
        Check.True(!watch.Offered(1, TileMessage.None, clock.Now), "a picture takes it away");
        clock.Advance(1);
        Check.True(!watch.Offered(1, TileMessage.Starting, clock.Now), "and the delay starts again");
        clock.Advance(TileRetryRule.OfferAfter);
        Check.True(watch.Offered(1, TileMessage.Starting, clock.Now), "offered again");
        Check.True(!watch.Offered(1, TileMessage.LoginFailed, clock.Now), "never for a refused login");
        clock.Advance(TileRetryRule.OfferAfter + 1);
        Check.True(!watch.Offered(1, TileMessage.LoginFailed, clock.Now), "however long it lasts");
    }

    private static void RestartAndIndependence()
    {
        var clock = new ManualViewerClock(0);
        var watch = new TileRetryWatch<int>();
        watch.Offered(1, TileMessage.Starting, clock.Now);
        clock.Advance(20);
        watch.Offered(2, TileMessage.Starting, clock.Now);
        clock.Advance(15);
        Check.True(watch.Offered(1, TileMessage.Starting, clock.Now), "tile 1 offered");
        Check.True(!watch.Offered(2, TileMessage.Starting, clock.Now), "tile 2 not yet");
        watch.Restart(1, clock.Now);
        Check.True(!watch.Offered(1, TileMessage.Starting, clock.Now), "pressed: gone");
        clock.Advance(TileRetryRule.OfferAfter);
        Check.True(watch.Offered(1, TileMessage.Starting, clock.Now), "back after 30 s");
        Check.True(watch.Offered(2, TileMessage.Starting, clock.Now), "tile 2 offered");
        watch.Keep([1]);
        Check.True(!watch.Offered(2, TileMessage.Starting, clock.Now), "forgotten tiles start over");
    }
}
