using NeriPlayer.Core.Player.Model;
using Xunit;

namespace NeriPlayer.Core.Tests;

public class SongIdentityTests
{
    [Fact]
    public void LocalSong_StableKey_IsNormalizedPath()
    {
        var song = new SongItem
        {
            Id = 1, Name = "A", Artist = "B", Album = "C",
            ChannelId = "local", LocalFilePath = @"D:\Music\a\b\c.flac"
        };
        Assert.Equal("local|d:/music/a/b/c.flac", song.StableKey());
    }

    [Theory]
    [InlineData("https://www.youtube.com/watch?v=abcDEF12345")]
    [InlineData("https://youtu.be/abcDEF12345?si=xxx")]
    public void YouTube_ExtractVideoId_Works(string uri)
    {
        var song = new SongItem
        {
            Id = 2, Name = "A", Artist = "B", Album = "C",
            ChannelId = "youtube_music", MediaUri = uri
        };
        Assert.Equal("abcDEF12345", SongIdentity.ExtractYouTubeVideoId(uri));
        Assert.Equal("ytm|abcDEF12345", song.StableKey());
    }

    [Fact]
    public void Netease_StableKey_UsesAudioId()
    {
        var song = new SongItem
        {
            Id = 9, Name = "A", Artist = "B", Album = "C",
            ChannelId = "netease", AudioId = "3456789"
        };
        Assert.Equal("netease|3456789", song.StableKey());
    }
}
