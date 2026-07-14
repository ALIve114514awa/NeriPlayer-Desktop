// 省流模式序列化/反序列化 — 与 Android SyncDataSerializer.kt 对齐
// backup.bin: Base64(GZIP(ProtoBuf))

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;
use std::io::{Read, Write};

use crate::error::{AppError, AppResult};
use super::models::*;
use super::proto_models::*;

/// 返回省流模式使用的文件名
pub fn get_filename(data_saver: bool) -> &'static str {
    if data_saver { "backup.bin" } else { "backup.json" }
}

/// 序列化 SyncData（省流模式: ProtoBuf + GZIP + Base64）
pub fn serialize_compressed(data: &SyncData) -> AppResult<String> {
    let proto = sync_data_to_proto(data);
    let proto_bytes = proto.encode_to_vec();

    // GZIP 压缩
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&proto_bytes)
        .map_err(|e| AppError::Other(format!("GZIP compress: {}", e)))?;
    let compressed = encoder.finish()
        .map_err(|e| AppError::Other(format!("GZIP finish: {}", e)))?;

    // Base64 编码
    Ok(BASE64.encode(&compressed))
}

/// 反序列化省流模式数据（Base64 -> GZIP -> ProtoBuf -> SyncData）
pub fn deserialize_compressed(content: &str) -> AppResult<SyncData> {
    // Base64 解码
    let compressed = BASE64.decode(content.trim())
        .map_err(|e| AppError::Other(format!("Base64 decode: {}", e)))?;

    // GZIP 解压
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut proto_bytes = Vec::new();
    decoder.read_to_end(&mut proto_bytes)
        .map_err(|e| AppError::Other(format!("GZIP decompress: {}", e)))?;

    // ProtoBuf 解码
    let proto = ProtoSyncData::decode(&proto_bytes[..])
        .map_err(|e| AppError::Other(format!("ProtoBuf decode: {}", e)))?;

    Ok(proto_to_sync_data(&proto))
}

/// 根据 data_saver 标志选择序列化方式
pub fn serialize(data: &SyncData, data_saver: bool) -> AppResult<String> {
    if data_saver {
        serialize_compressed(data)
    } else {
        serde_json::to_string_pretty(data)
            .map_err(|e| AppError::Other(format!("JSON serialize: {}", e)))
    }
}

/// 根据文件后缀判断格式并反序列化
pub fn deserialize(content: &str, is_binary: bool) -> AppResult<SyncData> {
    if is_binary {
        deserialize_compressed(content)
    } else {
        serde_json::from_str(content)
            .map_err(|e| AppError::Other(format!("JSON parse: {}", e)))
    }
}

// ---- Proto <-> SyncData 转换 ----

fn sync_data_to_proto(data: &SyncData) -> ProtoSyncData {
    ProtoSyncData {
        version: data.version.clone(),
        device_id: data.device_id.clone(),
        device_name: data.device_name.clone(),
        last_modified: data.last_modified,
        playlists: data.playlists.iter().map(sync_playlist_to_proto).collect(),
        favorite_playlists: data.favorite_playlists.iter().map(fav_playlist_to_proto).collect(),
        recent_plays: data.recent_plays.iter().map(recent_play_to_proto).collect(),
        sync_log: data.sync_log.iter().map(log_entry_to_proto).collect(),
        recent_play_deletions: data.recent_play_deletions.iter().map(deletion_to_proto).collect(),
        playback_stats: data.playback_stats.iter().map(track_stat_to_proto).collect(),
        playback_stats_cleared_at: data.playback_stats_cleared_at,
        playback_stat_buckets: data.playback_stat_buckets.iter().map(stat_bucket_to_proto).collect(),
        playlist_song_deletions: data.playlist_song_deletions.iter().map(playlist_song_deletion_to_proto).collect(),
    }
}

fn proto_to_sync_data(p: &ProtoSyncData) -> SyncData {
    SyncData {
        version: if p.version.is_empty() { "2.0".into() } else { p.version.clone() },
        device_id: p.device_id.clone(),
        device_name: p.device_name.clone(),
        last_modified: p.last_modified,
        playlists: p.playlists.iter().map(proto_to_sync_playlist).collect(),
        favorite_playlists: p.favorite_playlists.iter().map(proto_to_fav_playlist).collect(),
        recent_plays: p.recent_plays.iter().map(proto_to_recent_play).collect(),
        sync_log: p.sync_log.iter().map(proto_to_log_entry).collect(),
        recent_play_deletions: p.recent_play_deletions.iter().map(proto_to_deletion).collect(),
        playback_stats: p.playback_stats.iter().map(proto_to_track_stat).collect(),
        playback_stats_cleared_at: p.playback_stats_cleared_at,
        playback_stat_buckets: p.playback_stat_buckets.iter().map(proto_to_stat_bucket).collect(),
        playlist_song_deletions: p.playlist_song_deletions.iter().map(proto_to_playlist_song_deletion).collect(),
    }
}

fn sync_song_to_proto(s: &SyncSong) -> ProtoSyncSong {
    ProtoSyncSong {
        id: s.id.parse::<i64>().unwrap_or(0),
        name: s.name.clone(),
        artist: s.artist.clone(),
        album: s.album.clone(),
        album_id: s.album_id.parse::<i64>().unwrap_or(0),
        duration_ms: s.duration_ms,
        cover_url: if s.cover_url.is_empty() { None } else { Some(s.cover_url.clone()) },
        media_uri: if s.media_uri.is_empty() { None } else { Some(s.media_uri.clone()) },
        added_at: s.added_at,
        matched_lyric: s.lyric.clone(),
        matched_translated_lyric: s.translated_lyric.clone(),
        matched_lyric_source: s.lyric_source.clone(),
        matched_song_id: s.lyric_song_id.clone(),
        user_lyric_offset_ms: s.user_lyric_offset_ms.unwrap_or(0),
        custom_cover_url: s.custom_cover_url.clone(),
        custom_name: s.custom_name.clone(),
        custom_artist: s.custom_artist.clone(),
        original_name: s.original_name.clone(),
        original_artist: s.original_artist.clone(),
        original_cover_url: s.original_cover_url.clone(),
        original_lyric: None,
        original_translated_lyric: None,
        channel_id: s.channel_id.clone(),
        audio_id: s.audio_id.clone(),
        sub_audio_id: s.sub_audio_id.clone(),
        playlist_context_id: s.playlist_context_id.clone(),
        sync_membership_tokens: s.sync_membership_tokens.iter().map(causal_token_to_proto).collect(),
    }
}

fn proto_to_sync_song(p: &ProtoSyncSong) -> SyncSong {
    SyncSong {
        id: p.id.to_string(),
        name: p.name.clone(),
        artist: p.artist.clone(),
        album: p.album.clone(),
        album_id: p.album_id.to_string(),
        duration_ms: p.duration_ms,
        cover_url: p.cover_url.clone().unwrap_or_default(),
        media_uri: p.media_uri.clone().unwrap_or_default(),
        added_at: p.added_at,
        lyric: p.matched_lyric.clone(),
        translated_lyric: p.matched_translated_lyric.clone(),
        lyric_source: p.matched_lyric_source.clone(),
        lyric_song_id: p.matched_song_id.clone(),
        user_lyric_offset_ms: if p.user_lyric_offset_ms != 0 { Some(p.user_lyric_offset_ms) } else { None },
        custom_cover_url: p.custom_cover_url.clone(),
        custom_name: p.custom_name.clone(),
        custom_artist: p.custom_artist.clone(),
        original_cover_url: p.original_cover_url.clone(),
        original_name: p.original_name.clone(),
        original_artist: p.original_artist.clone(),
        channel_id: p.channel_id.clone(),
        audio_id: p.audio_id.clone(),
        sub_audio_id: p.sub_audio_id.clone(),
        playlist_context_id: p.playlist_context_id.clone(),
        sync_membership_tokens: p.sync_membership_tokens.iter().map(proto_to_causal_token).collect(),
    }
}

fn sync_playlist_to_proto(p: &SyncPlaylist) -> ProtoSyncPlaylist {
    ProtoSyncPlaylist {
        id: p.id.parse::<i64>().unwrap_or(0),
        name: p.name.clone(),
        songs: p.songs.iter().map(sync_song_to_proto).collect(),
        created_at: p.created_at,
        modified_at: p.modified_at,
        is_deleted: p.is_deleted,
        song_order_version: p.song_order_version,
    }
}

fn proto_to_sync_playlist(p: &ProtoSyncPlaylist) -> SyncPlaylist {
    SyncPlaylist {
        id: p.id.to_string(),
        name: p.name.clone(),
        songs: p.songs.iter().map(proto_to_sync_song).collect(),
        created_at: p.created_at,
        modified_at: p.modified_at,
        is_deleted: p.is_deleted,
        song_order_version: p.song_order_version,
    }
}

fn fav_playlist_to_proto(f: &SyncFavoritePlaylist) -> ProtoSyncFavoritePlaylist {
    ProtoSyncFavoritePlaylist {
        id: f.id.parse::<i64>().unwrap_or(0),
        name: f.name.clone(),
        cover_url: if f.cover_url.is_empty() { None } else { Some(f.cover_url.clone()) },
        track_count: f.track_count,
        source: f.source.clone(),
        songs: f.songs.iter().map(sync_song_to_proto).collect(),
        added_time: f.added_time,
        modified_at: f.modified_at,
        is_deleted: f.is_deleted,
        sort_order: f.sort_order as i64,
        browse_id: f.browse_id.clone(),
        playlist_id: f.playlist_id.clone(),
        subtitle: f.subtitle.clone(),
    }
}

fn proto_to_fav_playlist(p: &ProtoSyncFavoritePlaylist) -> SyncFavoritePlaylist {
    SyncFavoritePlaylist {
        id: p.id.to_string(),
        name: p.name.clone(),
        cover_url: p.cover_url.clone().unwrap_or_default(),
        track_count: p.track_count,
        source: p.source.clone(),
        songs: p.songs.iter().map(proto_to_sync_song).collect(),
        added_time: p.added_time,
        modified_at: p.modified_at,
        is_deleted: p.is_deleted,
        sort_order: p.sort_order as i32,
        browse_id: p.browse_id.clone(),
        playlist_id: p.playlist_id.clone(),
        subtitle: p.subtitle.clone(),
    }
}

fn recent_play_to_proto(r: &SyncRecentPlay) -> ProtoSyncRecentPlay {
    ProtoSyncRecentPlay {
        song_id: r.song_id.parse::<i64>().unwrap_or(0),
        song: Some(sync_song_to_proto(&r.song)),
        played_at: r.played_at,
        device_id: r.device_id.clone(),
    }
}

fn proto_to_recent_play(p: &ProtoSyncRecentPlay) -> SyncRecentPlay {
    SyncRecentPlay {
        song_id: p.song_id.to_string(),
        song: p.song.as_ref().map(proto_to_sync_song).unwrap_or_else(|| SyncSong {
            id: p.song_id.to_string(),
            ..Default::default()
        }),
        played_at: p.played_at,
        device_id: p.device_id.clone(),
    }
}

fn log_entry_to_proto(e: &SyncLogEntry) -> ProtoSyncLogEntry {
    ProtoSyncLogEntry {
        timestamp: e.timestamp,
        device_id: e.device_id.clone(),
        action: 0, // 简化：不映射 enum
        playlist_id: e.playlist_id.as_ref().and_then(|s| s.parse::<i64>().ok()),
        song_id: e.song_id.as_ref().and_then(|s| s.parse::<i64>().ok()),
        details: e.details.clone(),
    }
}

fn proto_to_log_entry(p: &ProtoSyncLogEntry) -> SyncLogEntry {
    SyncLogEntry {
        timestamp: p.timestamp,
        device_id: p.device_id.clone(),
        action: String::new(),
        playlist_id: p.playlist_id.map(|v| v.to_string()),
        song_id: p.song_id.map(|v| v.to_string()),
        details: p.details.clone(),
    }
}

fn deletion_to_proto(d: &SyncRecentPlayDeletion) -> ProtoSyncRecentPlayDeletion {
    ProtoSyncRecentPlayDeletion {
        song_id: d.song_id.parse::<i64>().unwrap_or(0),
        album: d.album.clone(),
        media_uri: if d.media_uri.is_empty() { None } else { Some(d.media_uri.clone()) },
        deleted_at: d.deleted_at,
        device_id: d.device_id.clone(),
    }
}

fn proto_to_deletion(p: &ProtoSyncRecentPlayDeletion) -> SyncRecentPlayDeletion {
    SyncRecentPlayDeletion {
        song_id: p.song_id.to_string(),
        album: p.album.clone(),
        media_uri: p.media_uri.clone().unwrap_or_default(),
        deleted_at: p.deleted_at,
        device_id: p.device_id.clone(),
    }
}

// ---- 新增：Android 对齐的转换函数 ----

fn causal_token_to_proto(t: &SyncCausalToken) -> ProtoSyncCausalToken {
    ProtoSyncCausalToken {
        device_id: t.device_id.clone(),
        counter: t.counter,
    }
}

fn proto_to_causal_token(p: &ProtoSyncCausalToken) -> SyncCausalToken {
    SyncCausalToken {
        device_id: p.device_id.clone(),
        counter: p.counter,
    }
}

fn counter_shard_to_proto(s: &SyncPlaybackCounterShard) -> ProtoSyncPlaybackCounterShard {
    ProtoSyncPlaybackCounterShard {
        device_id: s.device_id.clone(),
        epoch_started_at: s.epoch_started_at,
        total_listen_ms: s.total_listen_ms,
        play_count: s.play_count,
        first_played_at: s.first_played_at,
        last_played_at: s.last_played_at,
    }
}

fn proto_to_counter_shard(p: &ProtoSyncPlaybackCounterShard) -> SyncPlaybackCounterShard {
    SyncPlaybackCounterShard {
        device_id: p.device_id.clone(),
        epoch_started_at: p.epoch_started_at,
        total_listen_ms: p.total_listen_ms,
        play_count: p.play_count,
        first_played_at: p.first_played_at,
        last_played_at: p.last_played_at,
    }
}

fn track_stat_to_proto(s: &SyncTrackStat) -> ProtoSyncTrackStat {
    ProtoSyncTrackStat {
        identity_key: s.identity_key.clone(),
        name: s.name.clone(),
        artist: s.artist.clone(),
        album: s.album.clone(),
        total_listen_ms: s.total_listen_ms,
        play_count: s.play_count,
        last_played_at: s.last_played_at,
        first_played_at: s.first_played_at,
        cover_url: s.cover_url.clone(),
        duration_ms: s.duration_ms,
        media_uri: s.media_uri.clone(),
        id: s.id.parse::<i64>().unwrap_or(0),
        album_id: s.album_id.parse::<i64>().unwrap_or(0),
        counter_base_listen_ms: s.counter_base_listen_ms,
        counter_base_play_count: s.counter_base_play_count,
        counter_shards: s.counter_shards.iter().map(counter_shard_to_proto).collect(),
    }
}

fn proto_to_track_stat(p: &ProtoSyncTrackStat) -> SyncTrackStat {
    SyncTrackStat {
        identity_key: p.identity_key.clone(),
        name: p.name.clone(),
        artist: p.artist.clone(),
        album: p.album.clone(),
        total_listen_ms: p.total_listen_ms,
        play_count: p.play_count,
        last_played_at: p.last_played_at,
        first_played_at: p.first_played_at,
        cover_url: p.cover_url.clone(),
        duration_ms: p.duration_ms,
        media_uri: p.media_uri.clone(),
        id: p.id.to_string(),
        album_id: p.album_id.to_string(),
        counter_base_listen_ms: p.counter_base_listen_ms,
        counter_base_play_count: p.counter_base_play_count,
        counter_shards: p.counter_shards.iter().map(proto_to_counter_shard).collect(),
    }
}

fn stat_bucket_to_proto(b: &SyncPlaybackStatBucket) -> ProtoSyncPlaybackStatBucket {
    ProtoSyncPlaybackStatBucket {
        day_start_at: b.day_start_at,
        identity_key: b.identity_key.clone(),
        name: b.name.clone(),
        artist: b.artist.clone(),
        album: b.album.clone(),
        total_listen_ms: b.total_listen_ms,
        play_count: b.play_count,
        last_played_at: b.last_played_at,
        first_played_at: b.first_played_at,
        cover_url: b.cover_url.clone(),
        duration_ms: b.duration_ms,
        media_uri: b.media_uri.clone(),
        id: b.id.parse::<i64>().unwrap_or(0),
        album_id: b.album_id.parse::<i64>().unwrap_or(0),
        counter_base_listen_ms: b.counter_base_listen_ms,
        counter_base_play_count: b.counter_base_play_count,
        counter_shards: b.counter_shards.iter().map(counter_shard_to_proto).collect(),
    }
}

fn proto_to_stat_bucket(p: &ProtoSyncPlaybackStatBucket) -> SyncPlaybackStatBucket {
    SyncPlaybackStatBucket {
        day_start_at: p.day_start_at,
        identity_key: p.identity_key.clone(),
        name: p.name.clone(),
        artist: p.artist.clone(),
        album: p.album.clone(),
        total_listen_ms: p.total_listen_ms,
        play_count: p.play_count,
        last_played_at: p.last_played_at,
        first_played_at: p.first_played_at,
        cover_url: p.cover_url.clone(),
        duration_ms: p.duration_ms,
        media_uri: p.media_uri.clone(),
        id: p.id.to_string(),
        album_id: p.album_id.to_string(),
        counter_base_listen_ms: p.counter_base_listen_ms,
        counter_base_play_count: p.counter_base_play_count,
        counter_shards: p.counter_shards.iter().map(proto_to_counter_shard).collect(),
    }
}

fn playlist_song_deletion_to_proto(d: &SyncPlaylistSongDeletion) -> ProtoSyncPlaylistSongDeletion {
    ProtoSyncPlaylistSongDeletion {
        playlist_id: d.playlist_id.parse::<i64>().unwrap_or(0),
        song_id: d.song_id.parse::<i64>().unwrap_or(0),
        album: d.album.clone(),
        media_uri: d.media_uri.clone(),
        deleted_at: d.deleted_at,
        device_id: d.device_id.clone(),
        removed_membership_tokens: d.removed_membership_tokens.iter().map(causal_token_to_proto).collect(),
    }
}

fn proto_to_playlist_song_deletion(p: &ProtoSyncPlaylistSongDeletion) -> SyncPlaylistSongDeletion {
    SyncPlaylistSongDeletion {
        playlist_id: p.playlist_id.to_string(),
        song_id: p.song_id.to_string(),
        album: p.album.clone(),
        media_uri: if p.media_uri.as_deref() == Some("") { None } else { p.media_uri.clone() },
        deleted_at: p.deleted_at,
        device_id: p.device_id.clone(),
        removed_membership_tokens: p.removed_membership_tokens.iter().map(proto_to_causal_token).collect(),
    }
}
