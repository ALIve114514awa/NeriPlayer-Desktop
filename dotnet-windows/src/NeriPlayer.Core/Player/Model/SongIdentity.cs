using System.Text.RegularExpressions;

namespace NeriPlayer.Core.Player.Model;

public static partial class SongIdentity
{
    /// <summary>生成跨版本稳定的歌曲标识：去重、同步、持久化主键（对标 SongIdentity.kt）</summary>
    public static string StableKey(this SongItem song)
    {
        if (song.IsLocalSong())
            return $"local|{NormalizePath(song.LocalFilePath ?? song.MediaUri ?? "")}";

        return song.ChannelId switch
        {
            "netease" => $"netease|{song.AudioId ?? song.Id.ToString()}",
            "bilibili" => $"bilibili|{song.AudioId}|{song.SubAudioId}",
            "youtube_music" => $"ytm|{ExtractYouTubeVideoId(song.MediaUri)}",
            _ => $"id|{song.Id}|{song.Album}|{song.MediaUri}"
        };
    }

    private static string NormalizePath(string p) =>
        p.Replace('\\', '/').TrimEnd('/').ToLowerInvariant();

    /// <summary>从 YouTube 链接/播放列表 URI 提取视频 ID</summary>
    public static string ExtractYouTubeVideoId(string? uri)
    {
        if (string.IsNullOrEmpty(uri)) return "";
        var m = YoutubeVideoIdRegex().Match(uri);
        return m.Success ? m.Groups[1].Value : "";
    }

    [GeneratedRegex(@"(?:v=|youtu\.be/|/shorts/)([A-Za-z0-9_-]{11})")]
    private static partial Regex YoutubeVideoIdRegex();
}
