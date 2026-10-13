// Device passwords in Windows Credential Manager (docs/SANITIZATION.md,
// "Credentials at runtime"): one generic credential per configured device,
// named "nvrview/device/<key>" after the device's own key in the app's
// settings (DeviceConfig.Key), so renaming or readdressing a device keeps its
// password. Persisted for this user on this computer
// (CRED_PERSIST_LOCAL_MACHINE: never roamed). Passwords are kept in memory
// only while needed; they are never written to a file or a setting.

using System.Runtime.InteropServices;
using System.Text;
using TerraFX.Interop.Windows;
using static TerraFX.Interop.Windows.Windows;

namespace NvrView.App;

/// <summary>Where passwords are kept.</summary>
internal interface ICredentialStore
{
    string? Read(string key);

    bool Write(string key, string user, string password);

    void Delete(string key);
}

/// <summary>Credential Manager (see the file comment).</summary>
internal sealed unsafe class WindowsCredentials : ICredentialStore
{
    private const string DevicePrefix = "nvrview/device/";
    private const uint CRED_TYPE_GENERIC = 1;
    private const uint CRED_PERSIST_LOCAL_MACHINE = 2;

    private readonly string _prefix;

    /// <summary>Device passwords by default; smart alerts use their own prefix.</summary>
    public WindowsCredentials(string prefix = DevicePrefix) => _prefix = prefix;

    /// <summary>The credential's target name for a device key.</summary>
    public static string Target(string key) => DevicePrefix + key;

    private string TargetFor(string key) => _prefix + key;

    public string? Read(string key)
    {
        CREDENTIALW* credential = null;
        fixed (char* target = TargetFor(key))
        {
            if (!CredReadW(target, CRED_TYPE_GENERIC, 0, &credential))
            {
                return null;
            }
        }
        try
        {
            if (credential->CredentialBlob == null || credential->CredentialBlobSize == 0)
            {
                return "";
            }
            return new string((char*)credential->CredentialBlob, 0, (int)credential->CredentialBlobSize / sizeof(char));
        }
        finally
        {
            CredFree(credential);
        }
    }

    public bool Write(string key, string user, string password)
    {
        byte[] blob = Encoding.Unicode.GetBytes(password);
        try
        {
            fixed (char* target = TargetFor(key))
            fixed (char* userName = user)
            fixed (char* comment = "nvrview device password")
            fixed (byte* b = blob)
            {
                CREDENTIALW credential = new()
                {
                    Type = CRED_TYPE_GENERIC,
                    TargetName = target,
                    Comment = comment,
                    CredentialBlobSize = (uint)blob.Length,
                    CredentialBlob = b,
                    Persist = CRED_PERSIST_LOCAL_MACHINE,
                    UserName = userName,
                };
                if (CredWriteW(&credential, 0))
                {
                    return true;
                }
            }
            AppLog.Warn($"Credential Manager refused to save a password (error {Marshal.GetLastSystemError()})");
            return false;
        }
        finally
        {
            Array.Clear(blob);
        }
    }

    public void Delete(string key)
    {
        fixed (char* target = TargetFor(key))
        {
            _ = CredDeleteW(target, CRED_TYPE_GENERIC, 0);
        }
    }

    /// <summary>
    /// Writes, reads back, and deletes a throwaway credential (the self-test's
    /// --credential-test). Returns a problem, or null.
    /// </summary>
    public static string? RoundTrip()
    {
        var store = new WindowsCredentials();
        string key = "selftest-" + Guid.NewGuid().ToString("N")[..12];
        string password = "example-" + Guid.NewGuid().ToString("N")[..8];
        try
        {
            if (!store.Write(key, "example-user", password))
            {
                return $"CredWrite failed (error {Marshal.GetLastSystemError()})";
            }
            string? back = store.Read(key);
            if (back != password)
            {
                return back is null ? "CredRead found nothing" : "CredRead returned another password";
            }
            store.Delete(key);
            return store.Read(key) is null ? null : "CredDelete left the credential";
        }
        finally
        {
            store.Delete(key);
        }
    }
}

/// <summary>For development and the self-test: memory only, never the user's Credential Manager.</summary>
internal sealed class MemoryCredentials : ICredentialStore
{
    private readonly Dictionary<string, string> _passwords = [];

    public string? Read(string key) => _passwords.GetValueOrDefault(key);

    public bool Write(string key, string user, string password)
    {
        _passwords[key] = password;
        return true;
    }

    public void Delete(string key) => _passwords.Remove(key);
}
