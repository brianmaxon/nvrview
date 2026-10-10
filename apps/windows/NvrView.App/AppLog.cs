// The app's own log (the core keeps its own, docs/FFI.md "Logging"): one
// text file in the logs directory, rotated by size into a single older file,
// so it never holds more than about 2 MB. Repeated messages are rate-limited
// by key. No passwords or addresses are ever written here.
//
// A crash (an exception nothing handled: XAML's UnhandledException, the
// AppDomain's, an unobserved task's, or one out of startup) is written to its
// own crash-<time>.log next to app.log, keeping the newest few, so a start
// that dies before its window shows still says why.

using System.Diagnostics;
using System.Text;

namespace NvrView.App;

internal static class AppLog
{
    private const long MaxBytes = 1 << 20;
    private const int MaxCrashLogs = 8;
    private static readonly Lock s_lock = new();
    private static readonly Dictionary<string, long> s_lastByKey = [];
    private static string? s_file;
    private static string? s_directory;

    /// <summary>Starts logging into <paramref name="directory"/>.</summary>
    public static void Open(string directory)
    {
        Directory.CreateDirectory(directory);
        lock (s_lock)
        {
            s_file = Path.Combine(directory, "app.log");
            s_directory = directory;
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

    /// <summary>
    /// Writes <paramref name="e"/> to a new crash-*.log (in the logs directory,
    /// or <paramref name="fallbackDirectory"/> before it is open) and to app.log;
    /// never throws. Returns the file written, or null.
    /// </summary>
    public static string? Crash(string what, Exception e, string? fallbackDirectory = null)
    {
        Write("error", $"{what}: {e}");
        try
        {
            string directory;
            lock (s_lock)
            {
                directory = s_directory ?? fallbackDirectory ?? Path.GetTempPath();
            }
            Directory.CreateDirectory(directory);
            string file = Path.Combine(directory, $"crash-{DateTime.Now:yyyyMMdd-HHmmss-fff}-{Environment.ProcessId}.log");
            File.WriteAllText(file,
                $"nvrview {typeof(AppLog).Assembly.GetName().Version} {DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}{Environment.NewLine}" +
                $"{what}{Environment.NewLine}{e}{Environment.NewLine}", Encoding.UTF8);
            // Keep the newest few.
            foreach (FileInfo old in new DirectoryInfo(directory).GetFiles("crash-*.log")
                .OrderByDescending(f => f.Name, StringComparer.Ordinal).Skip(MaxCrashLogs))
            {
                old.Delete();
            }
            return file;
        }
        catch (Exception inner) when (inner is IOException or UnauthorizedAccessException or System.Security.SecurityException)
        {
            Startup($"{what}: {e}");
            return null;
        }
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
