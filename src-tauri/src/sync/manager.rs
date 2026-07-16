// 同步管理器 — 协调 GitHub/WebDAV 同步流程
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;
use crate::error::{AppError, AppResult};
use crate::state::TrackInfo;
use crate::library::playlist::{Playlist, PlaylistStore};
use super::models::*;
use super::serializer;
use super::github_api::GitHubApiClient;
use super::webdav_api::WebDavApiClient;
use super::merge;

/// GitHub 同步（支持省流模式 backup.bin / 普通模式 backup.json）
pub async fn sync_github(
    http: &reqwest::Client,
    config: &mut GitHubSyncConfig,
    local_data: &SyncData,
) -> AppResult<SyncResult> {
    let api = GitHubApiClient::new(http, &config.token);
    let data_saver = config.data_saver;
    let primary_file = serializer::get_filename(data_saver);
    let fallback_file = serializer::get_filename(!data_saver);

    // 1. 验证 token
    let _username = api.validate_token().await
        .map_err(|_| AppError::Api("GitHub token invalid or expired".into()))?;

    // 2. 确保仓库存在
    if let Err(_) = api.check_repository(&config.owner, &config.repo).await {
        api.create_repository(&config.repo).await?;
    }

    // 3. 拉取远程文件（优先主格式，降级备用格式）
    let (remote_data, remote_sha, actual_file) = match fetch_and_parse(&api, config, primary_file).await {
        Ok(result) => result,
        Err(_) => {
            // 主格式失败，尝试备用格式
            match fetch_and_parse(&api, config, fallback_file).await {
                Ok(result) => result,
                Err(_) => {
                    // 两种格式都不存在或为空，初始上传
                    let content = serializer::serialize(local_data, data_saver)?;
                    let existing_sha = api.get_file_content(&config.owner, &config.repo, primary_file).await?
                        .map(|(_, sha)| sha).unwrap_or_default();
                    let new_sha = api.update_file_content(
                        &config.owner, &config.repo, primary_file,
                        &content, &existing_sha, "Initial sync from NeriPlayer Desktop",
                    ).await?;
                    config.last_remote_sha = new_sha;
                    config.last_sync_time = chrono::Utc::now().timestamp_millis();
                    return Ok(SyncResult {
                        success: true,
                        message: "Initial upload complete".into(),
                        ..Default::default()
                    });
                }
            }
        }
    };

    // 4. 检查远程是否有变化
    let remote_changed = remote_sha != config.last_remote_sha;

    // 5. 加载 base snapshot 并三方合并
    let base_snapshot = load_base_snapshot();
    let merged = merge::three_way_merge(local_data, &remote_data, config.last_sync_time, &base_snapshot);

    // 5.5 合并后回写本地歌单
    save_synced_playlists(&merged);
    // 保存本次合并结果作为下次同步的 base snapshot
    save_base_snapshot(&merged);

    // 6. 检查是否需要上传
    if !remote_changed && !merge::has_data_changed(&remote_data, &merged) {
        config.last_remote_sha = remote_sha;
        config.last_sync_time = chrono::Utc::now().timestamp_millis();
        return Ok(SyncResult {
            success: true,
            message: "Already up to date".into(),
            ..Default::default()
        });
    }

    // 7. 上传合并后的数据（始终用主格式）
    let content = serializer::serialize(&merged, data_saver)?;
    let upload_sha = if actual_file == primary_file {
        remote_sha.clone()
    } else {
        // 读取的是备用格式，上传需要主格式文件的 sha
        api.get_file_content(&config.owner, &config.repo, primary_file).await?
            .map(|(_, sha)| sha).unwrap_or_default()
    };
    let new_sha = api.update_file_content(
        &config.owner, &config.repo, primary_file,
        &content, &upload_sha, "Sync from NeriPlayer Desktop",
    ).await?;

    // 8. 统计
    let playlists_added = merged.playlists.len() as i32 - remote_data.playlists.len() as i32;
    let songs_added = merged.playlists.iter().map(|p| p.songs.len()).sum::<usize>() as i32
        - remote_data.playlists.iter().map(|p| p.songs.len()).sum::<usize>() as i32;

    config.last_remote_sha = new_sha;
    config.last_sync_time = chrono::Utc::now().timestamp_millis();

    Ok(SyncResult {
        success: true,
        message: "Sync complete".into(),
        playlists_added: playlists_added.max(0),
        playlists_updated: 0,
        playlists_deleted: 0,
        songs_added: songs_added.max(0),
        songs_removed: 0,
    })
}

/// 从 GitHub 获取并解析远程数据
async fn fetch_and_parse(
    api: &GitHubApiClient,
    config: &GitHubSyncConfig,
    filename: &str,
) -> AppResult<(SyncData, String, String)> {
    let (content, sha) = api.get_file_content(&config.owner, &config.repo, filename).await?
        .ok_or_else(|| AppError::NotFound("Remote file not found".into()))?;
    if content.trim().is_empty() {
        return Err(AppError::Other("Remote file is empty".into()));
    }
    let is_binary = filename.ends_with(".bin");
    let data = serializer::deserialize(&content, is_binary)?;
    Ok((data, sha, filename.to_string()))
}

/// WebDAV 同步
pub async fn sync_webdav(
    http: &reqwest::Client,
    config: &mut WebDavSyncConfig,
    local_data: &SyncData,
) -> AppResult<SyncResult> {
    let api = WebDavApiClient::new(http, &config.server_url, &config.username, &config.password, &config.base_path);

    // 1. 验证连接
    api.validate_connection().await?;

    // 2. 拉取远程文件
    let (remote_content, remote_fingerprint) = match api.get_file_content().await? {
        Some((content, fp)) if !content.trim().is_empty() => (content, fp),
        _ => {
            let content = serde_json::to_string_pretty(local_data)?;
            let fp = api.update_file_content(&content).await?;
            config.last_remote_fingerprint = fp;
            config.last_sync_time = chrono::Utc::now().timestamp_millis();
            return Ok(SyncResult {
                success: true,
                message: "Initial upload complete".into(),
                ..Default::default()
            });
        }
    };

    // 3. 解析远程数据（WebDAV 始终 JSON）
    let remote_data: SyncData = serde_json::from_str(&remote_content)
        .map_err(|e| AppError::Other(format!("Failed to parse remote sync data: {}", e)))?;

    let remote_changed = remote_fingerprint != config.last_remote_fingerprint;
    let base_snapshot = load_base_snapshot();
    let merged = merge::three_way_merge(local_data, &remote_data, config.last_sync_time, &base_snapshot);
    save_synced_playlists(&merged);
    save_base_snapshot(&merged);

    if !remote_changed && !merge::has_data_changed(&remote_data, &merged) {
        config.last_remote_fingerprint = remote_fingerprint;
        config.last_sync_time = chrono::Utc::now().timestamp_millis();
        return Ok(SyncResult {
            success: true,
            message: "Already up to date".into(),
            ..Default::default()
        });
    }

    let content = serde_json::to_string_pretty(&merged)?;
    let fp = api.update_file_content(&content).await?;

    let playlists_added = merged.playlists.len() as i32 - remote_data.playlists.len() as i32;
    let songs_added = merged.playlists.iter().map(|p| p.songs.len()).sum::<usize>() as i32
        - remote_data.playlists.iter().map(|p| p.songs.len()).sum::<usize>() as i32;

    config.last_remote_fingerprint = fp;
    config.last_sync_time = chrono::Utc::now().timestamp_millis();

    Ok(SyncResult {
        success: true,
        message: "Sync complete".into(),
        playlists_added: playlists_added.max(0),
        playlists_updated: 0,
        playlists_deleted: 0,
        songs_added: songs_added.max(0),
        songs_removed: 0,
    })
}

/// 构建本地同步数据（从 tauri-plugin-store 读取歌单等）
pub fn build_local_sync_data(app: &AppHandle) -> SyncData {
    // 从 store 读取本地歌单数据
    // 当前歌单系统使用文件存储，构建 SyncData
    let device_id = get_or_create_device_id(app);
    let hostname = whoami::fallible::hostname().unwrap_or_else(|_| "Desktop".into());

    SyncData {
        version: "2.0".into(),
        device_id,
        device_name: format!("NeriPlayer Desktop ({})", hostname),
        last_modified: chrono::Utc::now().timestamp_millis(),
        playlists: load_local_playlists(app),
        favorite_playlists: Vec::new(),
        recent_plays: Vec::new(),
        sync_log: Vec::new(),
        recent_play_deletions: Vec::new(),
        playback_stats: Vec::new(),
        playback_stats_cleared_at: 0,
        playback_stat_buckets: Vec::new(),
        playlist_song_deletions: Vec::new(),
    }
}

/// 获取或创建设备 ID
fn get_or_create_device_id(app: &AppHandle) -> String {
    use tauri_plugin_store::StoreExt;
    let store = app.store("sync-state.json").ok();

    if let Some(ref s) = store {
        if let Some(id) = s.get("deviceId").and_then(|v| v.as_str().map(String::from)) {
            return id;
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    if let Some(s) = store {
        let _ = s.set("deviceId", serde_json::json!(id));
    }
    id
}

/// 歌单文件路径（与 library_cmd 保持一致）
fn playlists_path() -> std::path::PathBuf {
    let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("NeriPlayer");
    path.push("playlists.json");
    path
}

pub fn tracks_to_sync_songs_pub(tracks: &[TrackInfo]) -> Vec<SyncSong> {
    tracks_to_sync_songs(tracks)
}

fn tracks_to_sync_songs(tracks: &[TrackInfo]) -> Vec<SyncSong> {
    tracks.iter()
        .map(track_to_sync_song)
        .collect()
}

/// TrackInfo -> SyncSong 转换（内部使用）
fn track_to_sync_song(track: &TrackInfo) -> SyncSong {
    let platform = sync_platform_identity(track);

    SyncSong {
        id: platform.id,
        name: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        album_id: String::new(),
        duration_ms: track.duration_ms as i64,
        cover_url: track.cover_url.clone().unwrap_or_default(),
        media_uri: platform.media_uri.unwrap_or_default(),
        added_at: track.added_at.max(0),
        matched_lyric: None,
        matched_translated_lyric: None,
        matched_lyric_source: None,
        matched_song_id: None,
        user_lyric_offset_ms: 0,
        custom_cover_url: None,
        custom_name: None,
        custom_artist: None,
        original_cover_url: None,
        original_name: None,
        original_artist: None,
        original_lyric: None,
        original_translated_lyric: None,
        channel_id: platform.channel_id,
        audio_id: platform.audio_id,
        sub_audio_id: platform.sub_audio_id,
        playlist_context_id: None,
        sync_membership_tokens: Vec::new(),
    }
}

struct SyncPlatformIdentity {
    id: String,
    media_uri: Option<String>,
    channel_id: Option<String>,
    audio_id: Option<String>,
    sub_audio_id: Option<String>,
}

fn sync_platform_identity(track: &TrackInfo) -> SyncPlatformIdentity {
    if let Some(nid) = track.id.strip_prefix("netease:") {
        return SyncPlatformIdentity {
            id: nid.to_string(),
            media_uri: None,
            channel_id: Some("netease".into()),
            audio_id: Some(nid.to_string()),
            sub_audio_id: None,
        };
    }

    if let Some(mid) = track.id.strip_prefix("qq:") {
        return SyncPlatformIdentity {
            id: stable_remote_android_id("qq", mid, "").to_string(),
            media_uri: None,
            channel_id: Some("qq".into()),
            audio_id: Some(mid.to_string()),
            sub_audio_id: None,
        };
    }

    if let Some(vid) = track.id.strip_prefix("youtube:") {
        return SyncPlatformIdentity {
            id: stable_sync_identity_id(vid).to_string(),
            media_uri: Some(build_youtube_music_media_uri(vid)),
            channel_id: Some("youtube_music".into()),
            audio_id: Some(vid.to_string()),
            sub_audio_id: None,
        };
    }

    if let Some(bili_id) = track.id.strip_prefix("bilibili:") {
        let sub_audio_id = bilibili_cid_from_album(&track.album);
        let sub_audio = sub_audio_id.as_deref().unwrap_or("");
        return SyncPlatformIdentity {
            id: stable_remote_android_id("bilibili", bili_id, sub_audio).to_string(),
            media_uri: None,
            channel_id: Some("bilibili".into()),
            audio_id: Some(bili_id.to_string()),
            sub_audio_id,
        };
    }

    SyncPlatformIdentity {
        id: numeric_id_or_zero(&track.id).to_string(),
        media_uri: None,
        channel_id: None,
        audio_id: None,
        sub_audio_id: None,
    }
}

fn stable_remote_android_id(channel: &str, audio: &str, sub_audio: &str) -> i64 {
    if channel == "netease" {
        return audio.parse::<i64>().unwrap_or_else(|_| stable_sync_identity_id(&format!("{channel}|{audio}")));
    }
    stable_sync_identity_id(&format!("{channel}|{audio}|{sub_audio}"))
}

fn numeric_id_or_zero(value: &str) -> i64 {
    value.parse::<i64>().unwrap_or(0)
}

fn bilibili_cid_from_album(album: &str) -> Option<String> {
    album
        .strip_prefix("Bilibili|")
        .filter(|cid| !cid.is_empty())
        .map(String::from)
}

/// SyncSong -> TrackInfo 转换
/// Android 端格式：
///   - 网易云: mediaUri 为空，id 为纯数字
///   - YouTube: mediaUri = "ytmusic://video/{videoId}"
///   - B站: album 以 "Bilibili" 开头，id 可能含 channelId 信息
///   - 本地: mediaUri 在同步时被清除
fn sync_song_to_track(song: &SyncSong) -> TrackInfo {
    use crate::state::TrackSource;

    let channel = song.channel_id.as_deref().unwrap_or("").to_ascii_lowercase();
    let is_youtube = channel == "youtube_music"
        || channel == "youtubemusic"
        || channel == "youtube"
        || song.media_uri.starts_with("ytmusic://");
    let is_bilibili = channel == "bilibili" || song.album.starts_with("Bilibili");
    let is_qq = channel == "qq";
    let is_netease = channel == "netease" || (!song.id.is_empty() && song.media_uri.is_empty());

    let (full_id, source) = if is_youtube {
        // ytmusic://video/{videoId}?playlistId=... -> 提取 videoId
        let video_id = song.audio_id.as_deref()
            .or_else(|| song.media_uri.strip_prefix("ytmusic://video/"))
            .unwrap_or(&song.id)
            .split('?')
            .next()
            .unwrap_or(&song.id);
        (format!("youtube:{}", video_id), TrackSource::Youtube)
    } else if is_bilibili {
        let bili_id = song.audio_id.as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&song.id);
        (format!("bilibili:{}", bili_id), TrackSource::Bilibili)
    } else if is_qq {
        let qq_id = song.audio_id.as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&song.id);
        (format!("qq:{}", qq_id), TrackSource::Qq)
    } else if is_netease {
        let netease_id = song.audio_id.as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&song.id);
        (format!("netease:{}", netease_id), TrackSource::Netease)
    } else {
        // 无法识别来源
        (song.id.clone(), TrackSource::Local)
    };

    TrackInfo {
        id: full_id,
        title: song.custom_name.clone().unwrap_or_else(|| song.name.clone()),
        artist: song.custom_artist.clone().unwrap_or_else(|| song.artist.clone()),
        album: song.album.clone(),
        duration_ms: song.duration_ms.max(0) as u64,
        source,
        url: String::new(), // URL 在播放时动态获取
        cover_url: if song.cover_url.is_empty() { None } else { Some(song.cover_url.clone()) },
        added_at: song.added_at.max(0),
    }
}

/// 从本地歌单存储加载，转换为同步格式
fn load_local_playlists(_app: &AppHandle) -> Vec<SyncPlaylist> {
    let path = playlists_path();
    let store = PlaylistStore::load(&path);

    store.playlists.iter().map(|pl| SyncPlaylist {
        id: pl.id.to_string(),
        name: pl.name.clone(),
        songs: tracks_to_sync_songs(&pl.tracks),
        created_at: pl.modified_at as i64,
        modified_at: pl.modified_at as i64,
        is_deleted: false,
        song_order_version: DISPLAY_ORDER_SONG_ORDER_VERSION,
    }).collect()
}

/// 系统歌单 ID（对齐 Android FavoritesPlaylist / LocalFilesPlaylist）
const SYSTEM_FAVORITES_ID: i64 = -1001;
const SYSTEM_LOCAL_ID: i64 = -1002;

/// 识别系统歌单的候选名称
const FAVORITES_NAMES: &[&str] = &["我喜欢的音乐", "我喜歡的音樂", "お気に入りの曲", "Liked Songs", "My Favorite Music"];
const LOCAL_NAMES: &[&str] = &["本地音乐", "本機音樂", "ローカル音楽", "Local Music"];

fn is_favorites_name(name: &str) -> bool { FAVORITES_NAMES.iter().any(|n| *n == name) }
fn is_local_name(name: &str) -> bool { LOCAL_NAMES.iter().any(|n| *n == name) }

/// 解析 SyncPlaylist ID，识别系统歌单
fn resolve_system_id(sp_id: &str, sp_name: &str) -> i64 {
    if let Ok(id) = sp_id.parse::<i64>() {
        if id == -1001 || is_favorites_name(sp_name) { return SYSTEM_FAVORITES_ID; }
        if id == -1002 || is_local_name(sp_name) { return SYSTEM_LOCAL_ID; }
        if id > 0 { return id; }
    }
    if is_favorites_name(sp_name) { return SYSTEM_FAVORITES_ID; }
    if is_local_name(sp_name) { return SYSTEM_LOCAL_ID; }
    0 // 需要分配新 ID
}

/// 将同步合并后的歌单回写到本地存储（对齐 Android applyMergedDataToLocal）
pub fn save_synced_playlists(merged: &SyncData) {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);

    let mut new_playlists: Vec<Playlist> = Vec::new();
    let mut max_id: i64 = store.playlists.iter().map(|p| p.id).filter(|&id| id > 0).max().unwrap_or(0);

    // 歌单级别去重：同名歌单只保留第一个
    let mut seen_names = std::collections::HashSet::new();
    let now = chrono::Utc::now().timestamp_millis();

    for sp in &merged.playlists {
        if sp.is_deleted { continue; }
        let playlist = sp.normalized_for_display_order(now);

        let normalized_name = playlist.name.trim().to_string();
        if !seen_names.insert(normalized_name.clone()) {
            continue;
        }

        let mut local_id = resolve_system_id(&playlist.id, &playlist.name);
        if local_id == 0 {
            local_id = playlist.id.parse::<i64>().ok()
                .filter(|&id| id > 0)
                .or_else(|| store.playlists.iter().find(|p| p.name == playlist.name).map(|p| p.id))
                .unwrap_or_else(|| { max_id += 1; max_id });
        }

        // 检查 ID 冲突
        if new_playlists.iter().any(|p| p.id == local_id) {
            max_id += 1;
            local_id = max_id;
        }

        // 歌曲去重
        let mut seen_tracks = std::collections::HashSet::new();
        let tracks: Vec<TrackInfo> = playlist.songs.iter()
            .filter_map(|song| {
                let track = sync_song_to_track(song);
                if track.id.is_empty() || !seen_tracks.insert(track.id.clone()) {
                    None
                } else {
                    Some(track)
                }
            })
            .collect();

        new_playlists.push(Playlist {
            id: local_id,
            name: playlist.name,
            tracks,
            modified_at: playlist.modified_at.max(0) as u64,
        });
    }

    // 排序：我喜欢的音乐始终第一，本地文件始终最后，其余保持原序
    new_playlists.sort_by(|a, b| {
        let rank = |p: &Playlist| -> i32 {
            if p.id == SYSTEM_FAVORITES_ID { -1 }
            else if p.id == SYSTEM_LOCAL_ID { i32::MAX }
            else { 0 }
        };
        rank(a).cmp(&rank(b))
    });

    store.playlists = new_playlists;
    store.fix_next_id();
    let _ = store.save(&path);

    // 保存收藏歌单到独立文件
    save_favorite_playlists(merged);
}

/// 收藏歌单存储路径
fn favorites_path() -> std::path::PathBuf {
    let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("NeriPlayer");
    path.push("favorites.json");
    path
}

/// 保存收藏歌单（FavoritePlaylist）
fn save_favorite_playlists(merged: &SyncData) {
    let path = favorites_path();
    let favorites: Vec<&SyncFavoritePlaylist> = merged.favorite_playlists.iter()
        .filter(|f| !f.is_deleted)
        .collect();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&favorites).unwrap_or_default());
}

/// 读取收藏歌单（供 list 命令调用）
pub fn load_favorite_playlists() -> Vec<SyncFavoritePlaylist> {
    let path = favorites_path();
    if !path.exists() { return Vec::new(); }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

// ===== Base Snapshot — 用于三方歌曲合并的删除检测 =====

/// snapshot 文件路径
fn base_snapshot_path() -> std::path::PathBuf {
    let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("NeriPlayer");
    path.push("sync-base-snapshot.json");
    path
}

/// 加载上次同步后每个歌单的歌曲 stable_key 集合
/// 格式: { "playlist_id": ["key1", "key2", ...], ... }
pub fn load_base_snapshot() -> HashMap<String, HashSet<String>> {
    let path = base_snapshot_path();
    if !path.exists() {
        return HashMap::new();
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let raw: HashMap<String, Vec<String>> = serde_json::from_str(&content).unwrap_or_default();
    raw.into_iter()
        .map(|(k, v)| (k, v.into_iter().collect()))
        .collect()
}

/// 保存当前合并结果作为下次同步的 base snapshot
fn save_base_snapshot(merged: &SyncData) {
    let snapshot: HashMap<String, Vec<String>> = merged.playlists.iter()
        .filter(|p| !p.is_deleted)
        .map(|p| {
            let keys: Vec<String> = p.songs.iter()
                .map(|s| s.identity().stable_key())
                .collect();
            (p.id.clone(), keys)
        })
        .collect();

    let path = base_snapshot_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, serde_json::to_string(&snapshot).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{TrackInfo, TrackSource};

    #[test]
    fn sync_song_conversion_preserves_playlist_added_at() {
        let songs = tracks_to_sync_songs_pub(&[
            track("netease:1", 123),
            track("netease:2", 0),
        ]);

        assert_eq!(songs.iter().map(|song| song.added_at).collect::<Vec<_>>(), vec![123, 0]);

        let imported = sync_song_to_track(&SyncSong {
            id: "42".into(),
            name: "Remote".into(),
            album: "netease".into(),
            added_at: 777,
            channel_id: Some("netease".into()),
            audio_id: Some("42".into()),
            ..Default::default()
        });
        let roundtrip = tracks_to_sync_songs_pub(&[imported]);

        assert_eq!(roundtrip[0].added_at, 777);
    }

    fn track(id: &str, added_at: i64) -> TrackInfo {
        TrackInfo {
            id: id.into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 1_000,
            source: TrackSource::Netease,
            url: String::new(),
            cover_url: None,
            added_at,
        }
    }
}
