using NeriPlayer.Core.Player.Effects;

namespace NeriPlayer.Core.Tests;

public class EqualizerEffectTests
{
    [Fact]
    public void BandsHz_HasTenBands_AndSevenPresets()
    {
        Assert.Equal(10, EqualizerEffect.BandsHz.Length);
        Assert.Equal(7, EqualizerEffect.Presets.Count);
    }

    [Fact]
    public void Default_IsTransparent()
    {
        var eq = new EqualizerEffect(44100);
        eq.ApplyGains(new double[10]);   // 全 0 dB
        Assert.Equal(1.0f, eq.Process(1.0f), 3);   // 输出 ≈ 输入
    }

    [Fact]
    public void Biquad_ZeroDb_IsUnityGain()
    {
        // RBJ 归一化正确性验证：0dB Peaking 稳态增益必须为 1
        var f = new BiquadFilter();
        f.Configure(BiquadFilter.FilterType.Peaking, 1000, 0, 44100);
        var y = 0f;
        for (var i = 0; i < 2000; i++) y = f.Process(1.0f);
        Assert.Equal(1.0f, y, 3);
    }

    [Fact]
    public void Equalizer_GainApplies_ChangesOutput()
    {
        var eq = new EqualizerEffect(44100);
        var gain = new double[10];
        for (var i = 0; i < gain.Length; i++) gain[i] = 5.0;   // 全部 +5dB
        eq.ApplyGains(gain);
        Assert.NotEqual(1.0f, eq.Process(1.0f), 3);   // 增益后不再是直通
    }

    [Fact]
    public void Equalizer_OutOfRangeGain_IsClamped_AndStable()
    {
        var eq = new EqualizerEffect(44100);
        var gain = new double[10];
        for (var i = 0; i < gain.Length; i++) gain[i] = 100.0;   // 超限 ±20dB
        eq.ApplyGains(gain);   // 不应抛异常
        var output = eq.Process(1.0f);
        Assert.True(float.IsFinite(output));   // clamp 后数值稳定
    }
}
