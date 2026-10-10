// Catches drift between these bindings and the C header. The C side is
// reported by apps/windows/layout/nvlayout.c, compiled against
// crates/viewer-ffi/include/nvrview.h with the platform's C compiler; its
// output has one fact per line:
//
//   abi 3
//   size NvAction 216
//   offset NvAction.generation 208
//   value NV_ERR_BUFFER_TOO_SMALL -6
//
// Function names come from the header text itself.

using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;

using NvrView.Interop;

namespace NvrView.Smoke;

/// <summary>Compares the C# declarations with what the C compiler and header report.</summary>
internal static partial class NvLayoutCheck
{
    /// <summary>
    /// Compares sizes, field offsets, constants, and the ABI version with the
    /// C helper's output. Returns one line per mismatch (empty when they agree)
    /// and the number of facts compared.
    /// </summary>
    public static (List<string> Mismatches, int Compared) CompareLayout(IEnumerable<string> cLines)
    {
        var problems = new List<string>();
        int compared = 0;
        var structFieldsSeen = new Dictionary<string, int>();
        var valuesSeen = new HashSet<string>();
        Dictionary<string, long> csValues = EnumConstants();
        Assembly asm = typeof(NvAction).Assembly;

        foreach (string raw in cLines)
        {
            string[] f = raw.Trim().Split(' ', StringSplitOptions.RemoveEmptyEntries);
            if (f.Length == 0)
            {
                continue;
            }
            compared++;
            switch (f[0])
            {
                case "abi" when f.Length == 2:
                    if (uint.Parse(f[1]) != NvNative.AbiVersion)
                    {
                        problems.Add($"NV_ABI_VERSION: C {f[1]}, C# {NvNative.AbiVersion}");
                    }
                    break;
                case "abi_minor" when f.Length == 2:
                    if (uint.Parse(f[1]) != NvNative.AbiMinor)
                    {
                        problems.Add($"NV_ABI_MINOR: C {f[1]}, C# {NvNative.AbiMinor}");
                    }
                    break;
                case "size" when f.Length == 3 && f[1] == "enum":
                    foreach (Type e in asm.GetTypes().Where(t => t.IsEnum && t.IsPublic))
                    {
                        if (ManagedSize(e) != int.Parse(f[2]))
                        {
                            problems.Add($"sizeof enum: C {f[2]}, C# {e.Name} {ManagedSize(e)}");
                        }
                    }
                    break;
                case "size" when f.Length == 3:
                {
                    Type? t = asm.GetType("NvrView.Interop." + f[1]);
                    if (t is null)
                    {
                        problems.Add($"struct {f[1]}: missing in C#");
                        break;
                    }
                    int managed = ManagedSize(t);
                    int marshaled = Marshal.SizeOf(t);
                    if (managed != int.Parse(f[2]) || marshaled != managed)
                    {
                        problems.Add($"sizeof({f[1]}): C {f[2]}, C# {managed} (marshaled {marshaled})");
                    }
                    structFieldsSeen.TryAdd(f[1], 0);
                    break;
                }
                case "offset" when f.Length == 3:
                {
                    string[] parts = f[1].Split('.');
                    Type? t = asm.GetType("NvrView.Interop." + parts[0]);
                    string csName = Pascal(parts[1]);
                    FieldInfo? field = t?.GetField(csName, BindingFlags.Instance | BindingFlags.Public);
                    if (t is null || field is null)
                    {
                        problems.Add($"{f[1]}: no field {parts[0]}.{csName} in C#");
                        break;
                    }
                    long offset = Marshal.OffsetOf(t, csName);
                    if (offset != long.Parse(f[2]))
                    {
                        problems.Add($"offsetof({f[1]}): C {f[2]}, C# {offset}");
                    }
                    structFieldsSeen[parts[0]] = structFieldsSeen.GetValueOrDefault(parts[0]) + 1;
                    break;
                }
                case "value" when f.Length == 3:
                    valuesSeen.Add(f[1]);
                    if (!csValues.TryGetValue(f[1], out long v))
                    {
                        problems.Add($"{f[1]}: no enum member with this CName in C#");
                    }
                    else if (v != long.Parse(f[2]))
                    {
                        problems.Add($"{f[1]}: C {f[2]}, C# {v}");
                    }
                    break;
                default:
                    problems.Add($"unrecognized line: {raw}");
                    break;
            }
        }

        // The other direction: nothing in C# that the C side did not report.
        foreach ((string name, int seen) in structFieldsSeen)
        {
            Type t = asm.GetType("NvrView.Interop." + name)!;
            int fields = t.GetFields(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic).Length;
            if (fields != seen)
            {
                problems.Add($"struct {name}: C reports {seen} fields, C# has {fields}");
            }
        }
        foreach (Type t in asm.GetTypes().Where(IsBoundStruct))
        {
            if (!structFieldsSeen.ContainsKey(t.Name))
            {
                problems.Add($"struct {t.Name}: not reported by the C helper");
            }
        }
        foreach (string name in csValues.Keys.Where(k => !valuesSeen.Contains(k)))
        {
            problems.Add($"{name}: in C# but not reported by the C helper");
        }
        return (problems, compared);
    }

    /// <summary>
    /// Compares the functions declared in the header text with
    /// <see cref="NvNative"/>'s imports. Returns mismatches and the count compared.
    /// </summary>
    public static (List<string> Mismatches, int Compared) CompareFunctions(string headerText)
    {
        var header = new HashSet<string>(
            FunctionDeclaration().Matches(headerText).Select(m => m.Groups[1].Value));
        var bound = new HashSet<string>(
            typeof(NvNative).GetMethods(BindingFlags.Static | BindingFlags.Public)
                .Where(m => m.GetCustomAttribute<LibraryImportAttribute>() is not null)
                .Select(m => m.Name));
        var problems = new List<string>();
        problems.AddRange(header.Except(bound).Order().Select(n => $"{n}: in the header, not bound in C#"));
        problems.AddRange(bound.Except(header).Order().Select(n => $"{n}: bound in C#, not in the header"));
        if (header.Count == 0)
        {
            problems.Add("no function declarations found in the header");
        }
        return (problems, header.Count);
    }

    // A declaration starts at the beginning of a line with its return type;
    // comment lines start with "/" or " *".
    [GeneratedRegex(@"^[A-Za-z_][A-Za-z0-9_ ]*[ \*](nv_[a-z0-9_]+)\(", RegexOptions.Multiline)]
    private static partial Regex FunctionDeclaration();

    private static bool IsBoundStruct(Type t) =>
        t.IsValueType && !t.IsEnum && t.IsPublic && t.Name.StartsWith("Nv", StringComparison.Ordinal);

    private static Dictionary<string, long> EnumConstants()
    {
        var values = new Dictionary<string, long>();
        foreach (Type t in typeof(NvAction).Assembly.GetTypes().Where(t => t.IsEnum))
        {
            foreach (FieldInfo f in t.GetFields(BindingFlags.Public | BindingFlags.Static))
            {
                if (f.GetCustomAttribute<CNameAttribute>() is { } c)
                {
                    values[c.Name] = Convert.ToInt64(f.GetRawConstantValue());
                }
            }
        }
        return values;
    }

    private static int ManagedSize(Type t) =>
        (int)typeof(Unsafe).GetMethod(nameof(Unsafe.SizeOf))!.MakeGenericMethod(t).Invoke(null, null)!;

    // parameter_set_count -> ParameterSetCount
    private static string Pascal(string snake)
    {
        var sb = new StringBuilder(snake.Length);
        foreach (string part in snake.Split('_', StringSplitOptions.RemoveEmptyEntries))
        {
            sb.Append(char.ToUpperInvariant(part[0])).Append(part.AsSpan(1));
        }
        return sb.ToString();
    }
}
