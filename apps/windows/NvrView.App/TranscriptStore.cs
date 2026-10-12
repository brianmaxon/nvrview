// The folder of saved call transcripts (docs/UX.md "Doorbell calls"): %LOCALAPPDATA%\NvrView\Transcripts
// (under --data-dir when given; a temporary folder in the self-test). Each file is written atomically
// (a temporary file, then a move); then the folder is trimmed to the newest TranscriptFiles.MaxFiles
// files and MaxBytes bytes, oldest first. Only used while "Save call transcripts" is on. Call
// recordings (docs/CAPTURE.md "Recording calls") go in the same folder under the same stem, and are
// trimmed on their own (the newest 100, at most 10 GB).

using System.Diagnostics;
using System.Text;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal sealed class TranscriptStore(string folder)
{
    public string Folder { get; set; } = folder;

    /// <summary>Writes one transcript under a name not yet taken, then trims the folder. Returns the file, or null when it could not be written.</summary>
    public string? Save(string name, string text)
    {
        try
        {
            Directory.CreateDirectory(Folder);
            string[] taken = [.. Directory.EnumerateFiles(Folder).Select(Path.GetFileName).OfType<string>()];
            string file = Path.Combine(Folder, TranscriptFiles.UniqueName(name, taken));
            string temp = Path.Combine(Folder, $".{Guid.NewGuid():N}.tmp");
            File.WriteAllText(temp, text, new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            File.Move(temp, file, overwrite: false);
            Prune();
            return file;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Warn($"saving a call transcript failed: {e.GetType().Name}");
            return null;
        }
    }

    /// <summary>Deletes the oldest transcripts beyond the bounds (only .txt files; anything else in the folder is left alone).</summary>
    public void Prune()
    {
        var files = new List<StoredTranscript>();
        foreach (string path in Directory.EnumerateFiles(Folder, "*.txt"))
        {
            var info = new FileInfo(path);
            files.Add(new StoredTranscript(info.Name, info.Length, info.LastWriteTimeUtc));
        }
        foreach (string name in TranscriptFiles.ToDelete(files))
        {
            try
            {
                File.Delete(Path.Combine(Folder, name));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                AppLog.Warn($"an old call transcript could not be deleted: {e.GetType().Name}");
            }
        }
    }

    /// <summary>
    /// Deletes the oldest call recordings (<c>.mp4</c>, never the <c>.partial.mp4</c> files being written)
    /// beyond <see cref="CallRecordingFiles.MaxRecordings"/> files and <see cref="CallRecordingFiles.MaxRecordingBytes"/> bytes.
    /// </summary>
    public void PruneRecordings()
    {
        if (!Directory.Exists(Folder))
        {
            return;
        }
        var files = new List<StoredTranscript>();
        foreach (string path in Directory.EnumerateFiles(Folder, "*.mp4"))
        {
            var info = new FileInfo(path);
            files.Add(new StoredTranscript(info.Name, info.Length, info.LastWriteTimeUtc));
        }
        foreach (string name in CallRecordingFiles.ToDelete(files))
        {
            try
            {
                File.Delete(Path.Combine(Folder, name));
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                AppLog.Warn($"an old call recording could not be deleted: {e.GetType().Name}");
            }
        }
    }

    /// <summary>The saved transcripts, by name (self-test).</summary>
    public string[] Names() =>
        Directory.Exists(Folder) ? [.. Directory.EnumerateFiles(Folder, "*.txt").Select(Path.GetFileName).OfType<string>().Order(StringComparer.Ordinal)] : [];

    /// <summary>"Open transcripts folder": creates it if needed and shows it in File Explorer.</summary>
    public void OpenFolder()
    {
        try
        {
            Directory.CreateDirectory(Folder);
            Process.Start(new ProcessStartInfo { FileName = Folder, UseShellExecute = true })?.Dispose();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AppLog.Warn($"the transcripts folder could not be opened: {e.GetType().Name}");
        }
    }
}
