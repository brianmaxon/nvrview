// A tools/mock-nvr process the app starts for --mock-nvr and the self-test,
// on a free port (never 9000 or 8765, which a developer's own mock may use).

using System.Diagnostics;
using System.Text;

namespace NvrView.App;

internal sealed class MockNvr : IDisposable
{
    private readonly Process _process;

    private MockNvr(Process process, string host, ushort port)
    {
        _process = process;
        Host = host;
        Port = port;
    }

    public string Host { get; }

    public ushort Port { get; }

    /// <summary>Starts mock-nvr on a free port with <paramref name="args"/>; the password goes in its environment.</summary>
    public static MockNvr Start(string exe, string password, IEnumerable<string> args)
    {
        var psi = new ProcessStartInfo(exe)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        psi.ArgumentList.Add("--port");
        psi.ArgumentList.Add("0");
        foreach (string a in args)
        {
            psi.ArgumentList.Add(a);
        }
        psi.Environment["MOCK_NVR_PASSWORD"] = password;
        Process p = Process.Start(psi) ?? throw new InvalidOperationException("cannot start mock-nvr");
        var stderr = new StringBuilder();
        p.ErrorDataReceived += (_, e) =>
        {
            lock (stderr)
            {
                if (stderr.Length < 4096)
                {
                    stderr.AppendLine(e.Data);
                }
            }
        };
        p.BeginErrorReadLine();
        Task<string?> line = p.StandardOutput.ReadLineAsync();
        if (!line.Wait(TimeSpan.FromSeconds(15)) || line.Result is not { } text ||
            !text.StartsWith("mock-nvr listening on ", StringComparison.Ordinal))
        {
            p.Kill(entireProcessTree: true);
            lock (stderr)
            {
                throw new InvalidOperationException($"mock-nvr did not start: {stderr}");
            }
        }
        // Keep reading so its output never fills the pipe.
        _ = Task.Run(async () =>
        {
            while (await p.StandardOutput.ReadLineAsync() is not null)
            {
            }
        });
        string addr = text["mock-nvr listening on ".Length..].Trim();
        int colon = addr.LastIndexOf(':');
        return new MockNvr(p, addr[..colon], ushort.Parse(addr[(colon + 1)..]));
    }

    public void Dispose()
    {
        try
        {
            if (!_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
                _process.WaitForExit(5000);
            }
        }
        catch (InvalidOperationException)
        {
        }
        _process.Dispose();
    }
}
