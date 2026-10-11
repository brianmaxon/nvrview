// Keeps the doorbell's own microphone from putting our words into the "Visitor" captions
// (docs/UX.md "Captions", docs/AUDIO.md "Talk-back"). The doorbell plays our talk-back about a
// second late and its microphone hears its speaker, so while talk-back plays, and for a short while
// after it ends, a visitor caption that closely matches what we recently said or typed is echo: it
// is not shown, not saved, and not visitor activity. Visitor text that does not resemble what we
// sent is never filtered. Pure, with the viewer clock passed in; the same rules and vectors as
// CaptionEcho in the Mac app's NVRCore.

namespace NvrView.Tiles;

public sealed class CaptionEcho
{
    /// <summary>How long after talk-back has ended a visitor caption can still be our own words coming back: the device lag (~1 s), the recognizer's delay, and a margin. The talk ends with the core's stopped event, which comes after its 1.5 s silent tail (docs/AUDIO.md), so the tail is already inside "while talk-back plays".</summary>
    public const double Window = 3.0;

    /// <summary>What we said is remembered this long.</summary>
    public const double Memory = 30.0;

    /// <summary>Share of the longer phrase's words that must match in order.</summary>
    public const double Similarity = 0.6;

    /// <summary>A visitor line that is a fragment of what we said (captions arrive incrementally): share of its own words that must match in order.</summary>
    public const double Fragment = 0.8;

    private readonly List<(string[] Tokens, double At)> _phrases = [];
    private (string[] Tokens, double At)? _partial;
    private bool _talking;
    private double _openUntil = double.NegativeInfinity;

    /// <summary>Visitor captions dropped as echo (diagnostics, tests).</summary>
    public int Dropped { get; private set; }

    /// <summary>We said or typed <paramref name="text"/> at <paramref name="now"/>: a final microphone phrase, a volatile one (<paramref name="final"/> false, replaces the previous volatile one), or a typed reply (final).</summary>
    public void Said(string text, double now, bool final = true)
    {
        string[] tokens = Tokens(text);
        if (tokens.Length == 0)
        {
            if (!final)
            {
                _partial = null;
            }
            return;
        }
        _openUntil = Math.Max(_openUntil, now + Window);
        if (final)
        {
            _partial = null;
            _phrases.Add((tokens, now));
        }
        else
        {
            _partial = (tokens, now);
        }
        _phrases.RemoveAll(p => now - p.At > Memory);
    }

    /// <summary>Whether talk-back is being asked for, starting or playing. Going quiet (the core's stopped event, tail included) starts the window.</summary>
    public void Talking(bool on, double now)
    {
        if (_talking && !on)
        {
            _openUntil = Math.Max(_openUntil, now + Window);
        }
        _talking = on;
    }

    /// <summary>Whether our own talk-back could be coming back at <paramref name="now"/>.</summary>
    public bool IsOpen(double now) => _talking || now <= _openUntil;

    /// <summary>Whether the visitor caption <paramref name="text"/> is our words coming back.</summary>
    public bool IsEcho(string text, double now)
    {
        if (!IsOpen(now))
        {
            return false;
        }
        string[] heard = Tokens(text);
        if (heard.Length == 0)
        {
            return false;
        }
        List<string[]> candidates = [.. _phrases.Where(p => now - p.At <= Memory).Select(p => p.Tokens)];
        if (_partial is { } partial)
        {
            candidates.Add(partial.Tokens);
        }
        // Phrases said one after the other can come back as one caption.
        if (candidates.Count > 1)
        {
            candidates.Add([.. candidates.SelectMany(c => c)]);
        }
        return candidates.Any(said => Matches(heard, said));
    }

    /// <summary>IsEcho, counting the drop.</summary>
    public bool Filter(string text, double now)
    {
        bool echo = IsEcho(text, now);
        if (echo)
        {
            Dropped++;
        }
        return echo;
    }

    // ---- matching ----

    internal static bool Matches(string[] heard, string[] said)
    {
        if (heard.Length == 0 || said.Length == 0)
        {
            return false;
        }
        double common = Lcs(heard, said);
        if (common / Math.Max(heard.Length, said.Length) >= Similarity)
        {
            return true;
        }
        return common >= 2 && common / heard.Length >= Fragment;
    }

    /// <summary>Lowercase words without punctuation, digits as words ("1 2" -> "one two"; 0-99).</summary>
    internal static string[] Tokens(string text)
    {
        List<string> output = [];
        System.Text.StringBuilder word = new();
        void Flush()
        {
            if (word.Length > 0)
            {
                output.AddRange(Expand(word.ToString()));
            }
            word.Clear();
        }
        foreach (char c in text.ToLowerInvariant())
        {
            if (char.IsLetterOrDigit(c))
            {
                word.Append(c);
            }
            else if (c is '\'' or '’')
            {
                continue;
            }
            else
            {
                Flush();
            }
        }
        Flush();
        return [.. output];
    }

    private static readonly string[] Ones =
    [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
        "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
    ];

    private static readonly string[] Tens = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

    private static string[] Expand(string word)
    {
        if (word.Length > 2 || !word.All(c => c is >= '0' and <= '9'))
        {
            return [word];
        }
        int n = int.Parse(word, System.Globalization.CultureInfo.InvariantCulture);
        if (n < 20)
        {
            return [Ones[n]];
        }
        return n % 10 == 0 ? [Tens[n / 10]] : [Tens[n / 10], Ones[n % 10]];
    }

    private static bool SameWord(string a, string b)
    {
        if (a == b)
        {
            return true;
        }
        // Recognizers garble: one letter off counts for words of four or more.
        return a.Length >= 4 && b.Length >= 4 && Math.Abs(a.Length - b.Length) <= 1 && WithinOneEdit(a, b);
    }

    private static bool WithinOneEdit(string a, string b)
    {
        int i = 0;
        while (i < a.Length && i < b.Length && a[i] == b[i])
        {
            i++;
        }
        if (a.Length == b.Length)
        {
            return a[(i + 1)..] == b[(i + 1)..];
        }
        (string longer, string shorter) = a.Length > b.Length ? (a, b) : (b, a);
        return longer[(i + 1)..] == shorter[i..];
    }

    internal static int Lcs(string[] a, string[] b)
    {
        int[] row = new int[b.Length + 1];
        foreach (string x in a)
        {
            int diagonal = 0;
            for (int j = 0; j < b.Length; j++)
            {
                int above = row[j + 1];
                row[j + 1] = SameWord(x, b[j]) ? diagonal + 1 : Math.Max(above, row[j]);
                diagonal = above;
            }
        }
        return row[b.Length];
    }
}
