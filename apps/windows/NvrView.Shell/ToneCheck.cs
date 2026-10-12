// Measuring a decoded tone (the encoder test: a tone goes through the AAC encoder and back, and
// its pitch and level must come out right). Pure arithmetic on 16-bit samples, so it is tested
// on every platform; the app's self-test applies it to what the system's decoder returns.

namespace NvrView.Shell;

/// <summary>Pitch and level of a decoded tone.</summary>
public static class ToneCheck
{
    /// <summary>The RMS level of <paramref name="count"/> samples from <paramref name="from"/>, as a fraction of full scale (1.0 is a full-scale square wave).</summary>
    public static double Rms(ReadOnlySpan<short> samples, int from, int count)
    {
        double sum = 0;
        for (int i = from; i < from + count; i++)
        {
            double v = samples[i] / 32768.0;
            sum += v * v;
        }
        return Math.Sqrt(sum / count);
    }

    /// <summary>The level in dB relative to full scale; very quiet reads as -120.</summary>
    public static double Decibels(double rms) => rms <= 1e-6 ? -120 : 20 * Math.Log10(rms);

    /// <summary>
    /// The frequency of a tone in <paramref name="count"/> samples from <paramref name="from"/>,
    /// from the spacing of its rising zero crossings (each placed by linear interpolation); 0 when
    /// fewer than two cross.
    /// </summary>
    public static double Frequency(ReadOnlySpan<short> samples, int from, int count, double rate)
    {
        double first = 0;
        double last = 0;
        int crossings = 0;
        for (int i = from + 1; i < from + count; i++)
        {
            short a = samples[i - 1];
            short b = samples[i];
            if (a < 0 && b >= 0)
            {
                double at = i - 1 + (double)-a / (b - a);
                if (crossings == 0)
                {
                    first = at;
                }
                last = at;
                crossings++;
            }
        }
        return crossings < 2 ? 0 : (crossings - 1) * rate / (last - first);
    }

    /// <summary>A sine of <paramref name="hertz"/> and <paramref name="amplitude"/> (0 to 1) as float samples, for the encoder.</summary>
    public static float[] Sine(double hertz, double amplitude, int samples, double rate)
    {
        var s = new float[samples];
        for (int i = 0; i < samples; i++)
        {
            s[i] = (float)(amplitude * Math.Sin(2 * Math.PI * hertz * i / rate));
        }
        return s;
    }
}
