// The app's own log (the core keeps its own, docs/FFI.md "Logging"): one
// text file in the logs directory, rotated by size into a single older file,
// so it never holds more than about 2 MB. Repeated messages are rate-limited
// by key. No passwords or addresses are ever written here.

using System.Diagnostics;
using System.Text;

namespace NvrView.App;

internal static class AppLog
{
    private const long MaxBytes = 1 << 20;
    private static readonly Lock s_lock = new();
    private static readonly Dictionary<string, long> s_lastByKey = [];
    private static string? s_file;

    /// <summary>Starts logging into <paramref name="directory"/>.</summary>
    public static void Open(string directory)
    {
        Directory.CreateDirectory(directory);
        lock (s_lock)
        {
            s_file = Path.Combine(directory, "app.log");
        }
        Info($"nvrview {typeof(AppLog).Assembly.GetName().Version} started");
    }

    public static void Info(string message) => Write("info", message);

    public static void Warn(string message) => Write("warn", message);

    /// <summary>Logs at most once per <paramref name="seconds"/> for each key.</summary>
    public static void Limited(string key, double seconds, string message)
    {
        long now = Stopwatch.GetTimestamp();
        lock (s_lock)
        {
            if (s_lastByKey.TryGetValue(key, out long last) &&
                (now - last) < seconds * Stopwatch.Frequency)
            {
                return;
            }
            if (s_lastByKey.Count > 1000)
            {
                s_lastByKey.Clear();
            }
            s_lastByKey[key] = now;
        }
        Write("warn", message);
    }

    /// <summary>Before the log is open (bad command line): the temp directory.</summary>
    public static void Startup(string message)
    {
        try
        {
            File.AppendAllText(Path.Combine(Path.GetTempPath(), "nvrview-startup.log"),
                $"{DateTime.Now:yyyy-MM-dd HH:mm:ss} {message}{Environment.NewLine}");
        }
        catch (IOException)
        {
        }
    }

    private static void Write(string level, string message)
    {
        lock (s_lock)
        {
            if (s_file is null)
            {
                return;
            }
            try
            {
                var info = new FileInfo(s_file);
                if (info.Exists && info.Length > MaxBytes)
                {
                    File.Move(s_file, s_file + ".1", overwrite: true);
                }
                File.AppendAllText(s_file,
                    $"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {level} {message}{Environment.NewLine}", Encoding.UTF8);
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }
    }
}
