// Tests of NvrView.Tiles on simulated time. A tiny runner (no framework to
// install): each test is a method, a failed check throws, the exit code is
// the number of failures. Nothing here sleeps or reads the system clock.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class Program
{
    private static int Main()
    {
        int failed = 0, run = 0;
        foreach ((string name, Action test) in Tests.All().Concat(FisheyeTests.All()).Concat(CallTests.All()).Concat(RetryTests.All()).Concat(InsetTests.All()).Concat(PresentationTests.All()).Concat(ZoomTests.All()))
        {
            run++;
            try
            {
                test();
            }
            catch (Exception e)
            {
                failed++;
                Console.WriteLine($"FAIL {name}: {e.Message}");
            }
        }
        Console.WriteLine(failed == 0 ? $"tiles-tests: ok ({run} tests)" : $"tiles-tests: {failed} of {run} failed");
        return failed;
    }
}

internal sealed class CheckException(string message) : Exception(message);

internal static class Check
{
    public static void True(bool condition, string what)
    {
        if (!condition)
        {
            throw new CheckException(what);
        }
    }

    public static void Equal<T>(T expected, T actual, string what)
    {
        if (!EqualityComparer<T>.Default.Equals(expected, actual))
        {
            throw new CheckException($"{what}: expected {expected}, got {actual}");
        }
    }

    public static void Items(string expected, IEnumerable<IndicatorItem> actual, string what) =>
        Equal(expected, string.Join(", ", actual.Select(i => i.Description)), what);
}
