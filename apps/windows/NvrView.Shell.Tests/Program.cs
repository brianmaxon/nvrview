// Runs every test of NvrView.Shell; see the project file.

using System.Runtime.CompilerServices;

namespace NvrView.Shell.Tests;

internal static class T
{
    private static string s_test = "";
    private static int s_checks;
    private static readonly List<string> s_failures = [];

    public static void Run(string name, Action body)
    {
        s_test = name;
        try
        {
            body();
        }
        catch (Exception e)
        {
            Fail($"threw {e.GetType().Name}: {e.Message}\n{e.StackTrace}");
        }
    }

    public static void Check(bool condition, string? note = null, [CallerArgumentExpression(nameof(condition))] string? expression = null)
    {
        s_checks++;
        if (!condition)
        {
            Fail($"{expression}{(note is null ? "" : $" ({note})")}");
        }
    }

    public static void Equal<TValue>(TValue expected, TValue actual, string? note = null, [CallerArgumentExpression(nameof(actual))] string? expression = null)
    {
        s_checks++;
        if (!EqualityComparer<TValue>.Default.Equals(expected, actual))
        {
            Fail($"{expression}: expected {Show(expected)}, got {Show(actual)}{(note is null ? "" : $" ({note})")}");
        }
    }

    public static TValue NotNull<TValue>(TValue? value, [CallerArgumentExpression(nameof(value))] string? expression = null) where TValue : class
    {
        s_checks++;
        if (value is null)
        {
            throw new InvalidOperationException($"{expression} is null");
        }
        return value;
    }

    public static TValue NotNull<TValue>(TValue? value, [CallerArgumentExpression(nameof(value))] string? expression = null) where TValue : struct
    {
        s_checks++;
        return value ?? throw new InvalidOperationException($"{expression} is null");
    }

    private static string Show<TValue>(TValue v) => v switch
    {
        null => "null",
        string s => $"\"{s}\"",
        System.Collections.IEnumerable e => "[" + string.Join(", ", e.Cast<object?>().Select(x => x?.ToString() ?? "null")) + "]",
        _ => v.ToString() ?? "",
    };

    private static void Fail(string what) => s_failures.Add($"FAIL {s_test}: {what}");

    public static int Summary()
    {
        foreach (string f in s_failures)
        {
            Console.WriteLine(f);
        }
        Console.WriteLine($"shell-tests: {s_checks} checks, {s_failures.Count} failed");
        Console.WriteLine(s_failures.Count == 0 ? "shell-tests: ok" : "shell-tests: FAIL");
        return s_failures.Count == 0 ? 0 : 1;
    }
}

internal static class Program
{
    private static int Main()
    {
        LayoutTreeTests.Run();
        LayoutJsonTests.Run();
        GeometryTests.Run();
        LeaseTests.Run();
        RevealTests.Run();
        StoreTests.Run();
        AllCamerasTests.Run();
        LayoutTemplateTests.Run();
        DiscoveryTests.Run();
        UpdateTests.Run();
        RecordingsTests.Run();
        PlaybackTests.Run();
        RecordingsBrowseTests.Run();
        RecordingsModelTests.Run();
        ThumbnailDecodingTests.Run();
        return T.Summary();
    }
}
