using NeriPlayer.Core.Player.Effects;

namespace NeriPlayer.Core.Tests;

public class FftAnalyzerTests
{
    [Fact]
    public void OfSine_ReturnsNonZeroPeak()
    {
        var fft = new FftAnalyzer(1024);
        var samples = new float[1024];
        for (var i = 0; i < 1024; i++)
            samples[i] = MathF.Sin(2 * MathF.PI * 440f * i / 44100f);   // 440Hz
        var bands = fft.Compute(samples);
        Assert.True(bands.Max() > 0f);
    }

    [Fact]
    public void OfSine_PeaksInLowerBands()
    {
        // 440Hz 落在 20Hz~20kHz 对数刻度的中低频段（约 band 28）
        var fft = new FftAnalyzer(1024);
        var samples = new float[1024];
        for (var i = 0; i < 1024; i++)
            samples[i] = MathF.Sin(2 * MathF.PI * 440f * i / 44100f);
        var bands = fft.Compute(samples);
        var peak = Array.IndexOf(bands, bands.Max());
        Assert.InRange(peak, 20, 40);
    }

    [Fact]
    public void Silence_IsZero()
    {
        var fft = new FftAnalyzer(1024);
        var bands = fft.Compute(new float[1024]);
        Assert.All(bands, b => Assert.Equal(0f, b));
    }
}
