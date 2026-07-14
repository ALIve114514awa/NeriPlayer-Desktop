// 同步数据模型 — 与 Android 端 SyncDataModels.kt 保持 JSON 字段兼容
use serde::{Deserialize, Deserializer, Serialize};

/// 反序列化辅助：同时接受 string 和 number 类型，统一转为 String
fn deserialize_string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct StringOrNumber;
    impl<'de> de::Visitor<'de> for StringOrNumber {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or number")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }

    deserializer.deserialize_any(StringOrNumber)
}

/// Option<String> 版本：接受 null / string / number
fn deserialize_opt_string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct OptStringOrNumber;
    impl<'de> de::Visitor<'de> for OptStringOrNumber {
        type Value = Option<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("null, a string, or a number")
        }

        fn visit_none<E: de::Error>(self) -> Result<Option<String>, E> { Ok(None) }
        fn visit_unit<E: de::Error>(self) -> Result<Option<String>, E> { Ok(None) }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<String>, E> {
            Ok(if v.is_empty() { None } else { Some(v.to_string()) })
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<Option<String>, E> {
            Ok(if v.is_empty() { None } else { Some(v) })
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Option<String>, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Option<String>, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<Option<String>, E> {
            Ok(Some(v.to_string()))
        }
    }

    deserializer.deserialize_any(OptStringOrNumber)
}

/// 同步数据根信封
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncData {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub last_modified: i64,
    #[serde(default)]
    pub playlists: Vec<SyncPlaylist>,
    #[serde(default)]
    pub favorite_playlists: Vec<SyncFavoritePlaylist>,
    #[serde(default)]
    pub recent_plays: Vec<SyncRecentPlay>,
    #[serde(default)]
    pub sync_log: Vec<SyncLogEntry>,
    #[serde(default)]
    pub recent_play_deletions: Vec<SyncRecentPlayDeletion>,
    #[serde(default)]
    pub playback_stats: Vec<SyncTrackStat>,
    #[serde(default)]
    pub playback_stats_cleared_at: i64,
    #[serde(default)]
    pub playback_stat_buckets: Vec<SyncPlaybackStatBucket>,
    #[serde(default)]
    pub playlist_song_deletions: Vec<SyncPlaylistSongDeletion>,
}

fn default_version() -> String { "2.0".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlaylist {
    #[serde(deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub songs: Vec<SyncSong>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub modified_at: i64,
    #[serde(default)]
    pub is_deleted: bool,
    #[serde(default)]
    pub song_order_version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncSong {
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub album_id: String,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub cover_url: String,
    #[serde(default)]
    pub media_uri: String,
    #[serde(default)]
    pub added_at: i64,
    // 歌词相关
    #[serde(default)]
    pub lyric: Option<String>,
    #[serde(default)]
    pub translated_lyric: Option<String>,
    #[serde(default)]
    pub lyric_source: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub lyric_song_id: Option<String>,
    #[serde(default)]
    pub user_lyric_offset_ms: Option<i64>,
    // 自定义覆盖
    #[serde(default)]
    pub custom_cover_url: Option<String>,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub custom_artist: Option<String>,
    // 原始元数据
    #[serde(default)]
    pub original_cover_url: Option<String>,
    #[serde(default)]
    pub original_name: Option<String>,
    #[serde(default)]
    pub original_artist: Option<String>,
    // 平台相关
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub channel_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub audio_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub sub_audio_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub playlist_context_id: Option<String>,
    #[serde(default)]
    pub sync_membership_tokens: Vec<SyncCausalToken>,
}

impl SyncSong {
    /// 歌曲唯一标识（与 Android SongIdentity 对齐）
    pub fn identity(&self) -> SongIdentity {
        SongIdentity {
            id: self.id.clone(),
            album: self.album.clone(),
            media_uri: self.media_uri.clone(),
        }
    }
}

/// 歌曲身份标识，用于去重
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SongIdentity {
    pub id: String,
    pub album: String,
    pub media_uri: String,
}

impl SongIdentity {
    pub fn stable_key(&self) -> String {
        format!("{}|{}|{}", self.id, self.album, self.media_uri)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRecentPlay {
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub song_id: String,
    pub song: SyncSong,
    #[serde(default)]
    pub played_at: i64,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRecentPlayDeletion {
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub song_id: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub media_uri: String,
    #[serde(default)]
    pub deleted_at: i64,
    #[serde(default)]
    pub device_id: String,
}

impl SyncRecentPlayDeletion {
    pub fn identity(&self) -> SongIdentity {
        SongIdentity {
            id: self.song_id.clone(),
            album: self.album.clone(),
            media_uri: self.media_uri.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFavoritePlaylist {
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_url: String,
    #[serde(default)]
    pub track_count: i32,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub songs: Vec<SyncSong>,
    #[serde(default)]
    pub added_time: i64,
    #[serde(default)]
    pub modified_at: i64,
    #[serde(default)]
    pub is_deleted: bool,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub browse_id: Option<String>,
    #[serde(default)]
    pub playlist_id: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
}

impl SyncFavoritePlaylist {
    /// 分组键
    pub fn group_key(&self) -> String {
        format!("{}_{}", self.id, self.source)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLogEntry {
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub playlist_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
    pub song_id: Option<String>,
    #[serde(default)]
    pub details: Option<String>,
}

/// 因果一致性令牌
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncCausalToken {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub counter: i64,
}

/// 单曲播放统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncTrackStat {
    #[serde(default)]
    pub identity_key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub total_listen_ms: i64,
    #[serde(default)]
    pub play_count: i32,
    #[serde(default)]
    pub last_played_at: i64,
    #[serde(default)]
    pub first_played_at: i64,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub media_uri: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub album_id: String,
    #[serde(default)]
    pub counter_base_listen_ms: i64,
    #[serde(default)]
    pub counter_base_play_count: i32,
    #[serde(default)]
    pub counter_shards: Vec<SyncPlaybackCounterShard>,
}

/// 播放计数分片
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlaybackCounterShard {
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub epoch_started_at: i64,
    #[serde(default)]
    pub total_listen_ms: i64,
    #[serde(default)]
    pub play_count: i32,
    #[serde(default)]
    pub first_played_at: i64,
    #[serde(default)]
    pub last_played_at: i64,
}

/// 按天分桶的播放统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlaybackStatBucket {
    #[serde(default)]
    pub day_start_at: i64,
    #[serde(default)]
    pub identity_key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub total_listen_ms: i64,
    #[serde(default)]
    pub play_count: i32,
    #[serde(default)]
    pub last_played_at: i64,
    #[serde(default)]
    pub first_played_at: i64,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub media_uri: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub id: String,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub album_id: String,
    #[serde(default)]
    pub counter_base_listen_ms: i64,
    #[serde(default)]
    pub counter_base_play_count: i32,
    #[serde(default)]
    pub counter_shards: Vec<SyncPlaybackCounterShard>,
}

/// 歌单内歌曲删除记录
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPlaylistSongDeletion {
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub playlist_id: String,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    pub song_id: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub media_uri: Option<String>,
    #[serde(default)]
    pub deleted_at: i64,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub removed_membership_tokens: Vec<SyncCausalToken>,
}

impl SyncPlaylistSongDeletion {
    pub fn identity(&self) -> String {
        format!("{}|{}|{}", self.playlist_id, self.song_id, self.album)
    }
}

/// 同步结果
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub success: bool,
    pub message: String,
    #[serde(default)]
    pub playlists_added: i32,
    #[serde(default)]
    pub playlists_updated: i32,
    #[serde(default)]
    pub playlists_deleted: i32,
    #[serde(default)]
    pub songs_added: i32,
    #[serde(default)]
    pub songs_removed: i32,
}

/// 同步配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitHubSyncConfig {
    pub token: String,
    pub owner: String,
    pub repo: String,
    #[serde(default)]
    pub last_remote_sha: String,
    #[serde(default)]
    pub last_sync_time: i64,
    #[serde(default)]
    pub auto_sync: bool,
    #[serde(default = "default_true")]
    pub data_saver: bool,
    #[serde(default)]
    pub silent_failures: bool,
    #[serde(default = "default_history_mode")]
    pub history_update_mode: String,
}

fn default_true() -> bool { true }
fn default_history_mode() -> String { "immediate".into() }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebDavSyncConfig {
    pub server_url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub last_remote_fingerprint: String,
    #[serde(default)]
    pub last_sync_time: i64,
    #[serde(default)]
    pub auto_sync: bool,
}
