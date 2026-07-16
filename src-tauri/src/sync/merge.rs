// 三方合并算法 — 对齐 Android GitHubSyncManager.performThreeWayMerge
use std::collections::{HashMap, HashSet};
use super::models::*;

const MAX_RECENT_PLAYS: usize = 500;
const MAX_DELETIONS: usize = 500;
const MAX_SYNC_LOG: usize = 100;

/// 执行三方合并
/// base_snapshot: 上次同步后每个歌单的歌曲 stable_key 集合，用于检测本地/远程删除
pub fn three_way_merge(
    local: &SyncData,
    remote: &SyncData,
    last_sync_time: i64,
    base_snapshot: &HashMap<String, HashSet<String>>,
) -> SyncData {
    let playlists = merge_playlists(&local.playlists, &remote.playlists, last_sync_time, base_snapshot);
    let favorites = merge_favorite_playlists(&local.favorite_playlists, &remote.favorite_playlists, last_sync_time);
    let deletions = merge_recent_play_deletions(&local.recent_play_deletions, &remote.recent_play_deletions);
    let recent = merge_recent_plays(&local.recent_plays, &remote.recent_plays, &deletions);
    let log = merge_sync_log(&local.sync_log, &remote.sync_log);

    SyncData {
        version: "2.0".into(),
        device_id: local.device_id.clone(),
        device_name: local.device_name.clone(),
        last_modified: chrono::Utc::now().timestamp_millis(),
        playlists,
        favorite_playlists: favorites,
        recent_plays: recent,
        sync_log: log,
        recent_play_deletions: deletions,
        playback_stats: merge_playback_stats(&local.playback_stats, &remote.playback_stats),
        playback_stats_cleared_at: local.playback_stats_cleared_at.max(remote.playback_stats_cleared_at),
        playback_stat_buckets: merge_stat_buckets(&local.playback_stat_buckets, &remote.playback_stat_buckets),
        playlist_song_deletions: merge_playlist_song_deletions(&local.playlist_song_deletions, &remote.playlist_song_deletions),
    }
}

// ===== 歌单合并 =====

fn merge_playlists(local: &[SyncPlaylist], remote: &[SyncPlaylist], last_sync_time: i64, base_snapshot: &HashMap<String, HashSet<String>>) -> Vec<SyncPlaylist> {
    let local_map: HashMap<&str, &SyncPlaylist> = local.iter().map(|p| (p.id.as_str(), p)).collect();
    let remote_map: HashMap<&str, &SyncPlaylist> = remote.iter().map(|p| (p.id.as_str(), p)).collect();

    let all_ids: HashSet<&str> = local_map.keys().chain(remote_map.keys()).copied().collect();
    let mut merged: HashMap<String, SyncPlaylist> = HashMap::new();

    for id in all_ids {
        let local_pl = local_map.get(id).copied();
        let remote_pl = remote_map.get(id).copied();

        let result = match (local_pl, remote_pl) {
            (Some(l), None) => {
                if !l.is_deleted { Some(l.clone()) } else { None }
            }
            (None, Some(r)) => {
                if !r.is_deleted { Some(r.clone()) } else { None }
            }
            (Some(l), Some(r)) => {
                // 任一标记删除 -> 删除（墓碑胜出）
                if l.is_deleted || r.is_deleted {
                    None
                } else {
                    let base_songs = base_snapshot.get(id).cloned().unwrap_or_default();
                    Some(merge_single_playlist(l, r, last_sync_time, &base_songs))
                }
            }
            (None, None) => None,
        };

        if let Some(p) = result {
            merged.insert(id.to_string(), p);
        }
    }

    // 排序：以修改时间较新的一方的顺序为主
    let now = chrono::Utc::now().timestamp_millis();
    order_merged_playlists(&merged, local, remote, last_sync_time)
        .into_iter()
        .map(|playlist| playlist.normalized_for_display_order(now))
        .collect()
}

fn merge_single_playlist(local: &SyncPlaylist, remote: &SyncPlaylist, last_sync_time: i64, base_songs: &HashSet<String>) -> SyncPlaylist {
    // 名称：取更新的
    let name = if local.name == remote.name {
        local.name.clone()
    } else if remote.modified_at > last_sync_time && local.modified_at <= last_sync_time {
        remote.name.clone()
    } else {
        local.name.clone() // 本地优先
    };

    // 歌曲合并
    let songs = merge_songs(&local.songs, &remote.songs, last_sync_time, base_songs);

    SyncPlaylist {
        id: local.id.clone(),
        name,
        songs,
        created_at: local.created_at.min(remote.created_at),
        modified_at: local.modified_at.max(remote.modified_at),
        is_deleted: false,
        song_order_version: DISPLAY_ORDER_SONG_ORDER_VERSION,
    }
}

/// 三方歌曲合并：
/// - base_songs: 上次同步后的歌曲 stable_key 集合
/// - 在 base 中存在但本地不存在 -> 本地删除，从结果中排除（即使远程还有）
/// - 在 base 中存在但远程不存在 -> 远程删除，从结果中排除（即使本地还有）
/// - 不在 base 中且仅在本地/远程 -> 新增，保留
/// - base 为空（首次同步）-> 退化为 additive 合并
fn merge_songs(local: &[SyncSong], remote: &[SyncSong], _last_sync_time: i64, base_songs: &HashSet<String>) -> Vec<SyncSong> {
    if remote.is_empty() && !local.is_empty() {
        return local.to_vec();
    }
    if local.is_empty() && !remote.is_empty() {
        return remote.to_vec();
    }

    let local_keys: HashSet<String> = local.iter().map(|s| s.identity().stable_key()).collect();
    let remote_keys: HashSet<String> = remote.iter().map(|s| s.identity().stable_key()).collect();
    let remote_by_key: HashMap<String, &SyncSong> = remote
        .iter()
        .map(|song| (song.identity().stable_key(), song))
        .collect();

    // 计算删除集合
    // 在 base 中存在但本地不存在 -> 本地删除
    let locally_deleted: HashSet<&String> = base_songs.iter()
        .filter(|k| !local_keys.contains(*k))
        .collect();
    // 在 base 中存在但远程不存在 -> 远程删除
    let remotely_deleted: HashSet<&String> = base_songs.iter()
        .filter(|k| !remote_keys.contains(*k))
        .collect();

    // 以本地为基准
    let mut result: Vec<SyncSong> = Vec::new();

    for song in local {
        let key = song.identity().stable_key();
        // 跳过远程删除的歌曲
        if remotely_deleted.contains(&key) {
            continue;
        }
        result.push(merge_song_metadata(song, remote_by_key.get(&key).copied()));
    }

    // 追加远程独有（不在本地中、且非本地删除的）
    let result_keys: HashSet<String> = result.iter().map(|s| s.identity().stable_key()).collect();
    for song in remote {
        let key = song.identity().stable_key();
        if !result_keys.contains(&key) && !locally_deleted.contains(&key) {
            result.push(song.clone());
        }
    }

    result
}

fn merge_song_metadata(local: &SyncSong, remote: Option<&SyncSong>) -> SyncSong {
    let Some(remote) = remote else {
        return local.clone();
    };

    let mut merged = local.clone();
    merged.added_at = merge_added_at(local.added_at, remote.added_at);
    merged
}

fn merge_added_at(local: i64, remote: i64) -> i64 {
    match (local > 0, remote > 0) {
        (true, true) => local.min(remote),
        (true, false) => local,
        (false, true) => remote,
        (false, false) => 0,
    }
}

fn order_merged_playlists(
    merged: &HashMap<String, SyncPlaylist>,
    local: &[SyncPlaylist],
    remote: &[SyncPlaylist],
    last_sync_time: i64,
) -> Vec<SyncPlaylist> {
    // 优先使用有修改的一方的顺序
    let local_modified = local.iter().any(|p| p.modified_at > last_sync_time);
    let remote_modified = remote.iter().any(|p| p.modified_at > last_sync_time);

    let (primary, secondary) = if remote_modified && !local_modified {
        (remote, local)
    } else {
        (local, remote)
    };

    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    // 按主序列排
    for p in primary {
        if merged.contains_key(&p.id) && seen.insert(p.id.clone()) {
            ordered.push(merged[&p.id].clone());
        }
    }
    // 补充副序列中未出现的
    for p in secondary {
        if merged.contains_key(&p.id) && seen.insert(p.id.clone()) {
            ordered.push(merged[&p.id].clone());
        }
    }
    // 其余
    for (id, p) in merged {
        if seen.insert(id.clone()) {
            ordered.push(p.clone());
        }
    }

    ordered
}

// ===== 收藏歌单合并 =====

fn merge_favorite_playlists(
    local: &[SyncFavoritePlaylist],
    remote: &[SyncFavoritePlaylist],
    _last_sync_time: i64,
) -> Vec<SyncFavoritePlaylist> {
    let mut by_key: HashMap<String, SyncFavoritePlaylist> = HashMap::new();

    for fp in local.iter().chain(remote.iter()) {
        let key = fp.group_key();
        by_key.entry(key)
            .and_modify(|existing| {
                *existing = merge_single_favorite(existing, fp);
            })
            .or_insert_with(|| fp.clone());
    }

    let mut result: Vec<SyncFavoritePlaylist> = by_key.into_values().collect();
    result.sort_by(|a, b| b.added_time.cmp(&a.added_time));
    result
}

fn merge_single_favorite(a: &SyncFavoritePlaylist, b: &SyncFavoritePlaylist) -> SyncFavoritePlaylist {
    // 较新的为基础
    let (newer, older) = if a.modified_at >= b.modified_at { (a, b) } else { (b, a) };

    // 删除状态：较新的说了算
    if newer.is_deleted && older.is_deleted {
        let mut result = newer.clone();
        result.added_time = newer.added_time.max(older.added_time);
        result.modified_at = newer.modified_at.max(older.modified_at);
        return result;
    }

    let mut result = newer.clone();
    result.added_time = newer.added_time.max(older.added_time);
    result.modified_at = newer.modified_at.max(older.modified_at);
    result.track_count = newer.track_count.max(older.track_count);

    if !result.is_deleted {
        // 合并歌曲
        let existing_keys: HashSet<String> = result.songs.iter().map(|s| s.identity().stable_key()).collect();
        for song in &older.songs {
            if !existing_keys.contains(&song.identity().stable_key()) {
                result.songs.push(song.clone());
            }
        }
    }

    if result.sort_order == 0 {
        result.sort_order = older.sort_order;
    }

    result
}

// ===== 最近播放合并 =====

fn merge_recent_plays(
    local: &[SyncRecentPlay],
    remote: &[SyncRecentPlay],
    deletions: &[SyncRecentPlayDeletion],
) -> Vec<SyncRecentPlay> {
    let deletion_keys: HashMap<String, i64> = deletions.iter()
        .map(|d| (d.identity().stable_key(), d.deleted_at))
        .collect();

    let mut all: Vec<SyncRecentPlay> = local.iter().chain(remote.iter()).cloned().collect();
    all.sort_by(|a, b| b.played_at.cmp(&a.played_at));

    // 去重 + 过滤已删除
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for rp in all {
        let key = rp.song.identity().stable_key();

        // 过滤：如果有删除记录且删除时间晚于播放时间
        if let Some(&del_time) = deletion_keys.get(&key) {
            if del_time > rp.played_at {
                continue;
            }
        }

        if seen.insert(key) {
            result.push(rp);
        }
    }

    result.truncate(MAX_RECENT_PLAYS);
    result
}

// ===== 删除记录合并 =====

fn merge_recent_play_deletions(
    local: &[SyncRecentPlayDeletion],
    remote: &[SyncRecentPlayDeletion],
) -> Vec<SyncRecentPlayDeletion> {
    let mut by_key: HashMap<String, SyncRecentPlayDeletion> = HashMap::new();

    for d in local.iter().chain(remote.iter()) {
        let key = d.identity().stable_key();
        by_key.entry(key)
            .and_modify(|existing| {
                if d.deleted_at > existing.deleted_at {
                    *existing = d.clone();
                }
            })
            .or_insert_with(|| d.clone());
    }

    let mut result: Vec<SyncRecentPlayDeletion> = by_key.into_values().collect();
    result.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    result.truncate(MAX_DELETIONS);
    result
}

// ===== 同步日志合并 =====

fn merge_sync_log(local: &[SyncLogEntry], remote: &[SyncLogEntry]) -> Vec<SyncLogEntry> {
    let mut seen = HashSet::new();
    let mut all: Vec<SyncLogEntry> = Vec::new();

    for entry in local.iter().chain(remote.iter()) {
        // 按时间戳去重
        if seen.insert(entry.timestamp) {
            all.push(entry.clone());
        }
    }

    all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    all.truncate(MAX_SYNC_LOG);
    all
}

/// 检查合并后的数据是否与远程有变化（决定是否需要上传）
pub fn has_data_changed(remote: &SyncData, merged: &SyncData) -> bool {
    // 歌单数量或 ID 不同
    if remote.playlists.len() != merged.playlists.len() {
        return true;
    }

    let remote_ids: Vec<&str> = remote.playlists.iter().map(|p| p.id.as_str()).collect();
    let merged_ids: Vec<&str> = merged.playlists.iter().map(|p| p.id.as_str()).collect();
    if remote_ids != merged_ids {
        return true;
    }

    // 歌单内容比对
    for (rp, mp) in remote.playlists.iter().zip(merged.playlists.iter()) {
        if rp.name != mp.name
            || rp.song_order_version != mp.song_order_version
            || rp.songs.len() != mp.songs.len()
        {
            return true;
        }
        if rp.songs.iter().zip(&mp.songs).any(|(remote_song, merged_song)| {
            remote_song.identity().stable_key() != merged_song.identity().stable_key()
                || remote_song.added_at != merged_song.added_at
        }) {
            return true;
        }
    }

    // 收藏歌单
    if remote.favorite_playlists.len() != merged.favorite_playlists.len() {
        return true;
    }

    // 最近播放（只比前 50 条）
    let r_recent: Vec<String> = remote.recent_plays.iter().take(50).map(|r| r.song.identity().stable_key()).collect();
    let m_recent: Vec<String> = merged.recent_plays.iter().take(50).map(|r| r.song.identity().stable_key()).collect();
    if r_recent != m_recent {
        return true;
    }

    false
}

// ===== 播放统计合并 — CRDT 风格 counter-shard 策略 =====

fn merge_counter_shards(local: &[SyncPlaybackCounterShard], remote: &[SyncPlaybackCounterShard]) -> Vec<SyncPlaybackCounterShard> {
    let mut by_device: HashMap<String, SyncPlaybackCounterShard> = HashMap::new();

    for shard in local.iter().chain(remote.iter()) {
        let entry = by_device.entry(shard.device_id.clone()).or_insert_with(|| shard.clone());
        if shard.last_played_at > entry.last_played_at {
            *entry = shard.clone();
        }
    }

    by_device.into_values().collect()
}

fn merge_playback_stats(local: &[SyncTrackStat], remote: &[SyncTrackStat]) -> Vec<SyncTrackStat> {
    let mut by_key: HashMap<String, SyncTrackStat> = HashMap::new();

    for stat in local.iter() {
        by_key.insert(stat.identity_key.clone(), stat.clone());
    }

    for stat in remote.iter() {
        if let Some(entry) = by_key.get_mut(&stat.identity_key) {
            entry.counter_shards = merge_counter_shards(&entry.counter_shards, &stat.counter_shards);
            entry.last_played_at = entry.last_played_at.max(stat.last_played_at);
            entry.first_played_at = if entry.first_played_at == 0 { stat.first_played_at } else { entry.first_played_at.min(stat.first_played_at) };
            let total: (i64, i32) = entry.counter_shards.iter().fold((0, 0), |(ms, c), s| (ms + s.total_listen_ms, c + s.play_count));
            entry.total_listen_ms = entry.counter_base_listen_ms + total.0;
            entry.play_count = entry.counter_base_play_count + total.1;
        } else {
            by_key.insert(stat.identity_key.clone(), stat.clone());
        }
    }

    by_key.into_values().collect()
}

fn merge_stat_buckets(local: &[SyncPlaybackStatBucket], remote: &[SyncPlaybackStatBucket]) -> Vec<SyncPlaybackStatBucket> {
    let mut by_key: HashMap<(i64, String), SyncPlaybackStatBucket> = HashMap::new();

    for bucket in local.iter() {
        by_key.insert((bucket.day_start_at, bucket.identity_key.clone()), bucket.clone());
    }

    for bucket in remote.iter() {
        let key = (bucket.day_start_at, bucket.identity_key.clone());
        let entry = by_key.entry(key).or_insert_with(|| bucket.clone());
        entry.counter_shards = merge_counter_shards(&entry.counter_shards, &bucket.counter_shards);
        entry.last_played_at = entry.last_played_at.max(bucket.last_played_at);
        entry.first_played_at = if entry.first_played_at == 0 { bucket.first_played_at } else { entry.first_played_at.min(bucket.first_played_at) };
        let total: (i64, i32) = entry.counter_shards.iter().fold((0, 0), |(ms, c), s| (ms + s.total_listen_ms, c + s.play_count));
        entry.total_listen_ms = entry.counter_base_listen_ms + total.0;
        entry.play_count = entry.counter_base_play_count + total.1;
    }

    by_key.into_values().collect()
}

fn merge_playlist_song_deletions(local: &[SyncPlaylistSongDeletion], remote: &[SyncPlaylistSongDeletion]) -> Vec<SyncPlaylistSongDeletion> {
    let mut by_key: HashMap<String, SyncPlaylistSongDeletion> = HashMap::new();

    for d in local.iter().chain(remote.iter()) {
        let key = d.identity();
        let entry = by_key.entry(key).or_insert_with(|| d.clone());
        if d.deleted_at > entry.deleted_at {
            *entry = d.clone();
        }
        // union membership tokens
        let existing_tokens: HashSet<String> = entry.removed_membership_tokens.iter()
            .map(|t| format!("{}:{}", t.device_id, t.counter)).collect();
        for t in &d.removed_membership_tokens {
            let tk = format!("{}:{}", t.device_id, t.counter);
            if !existing_tokens.contains(&tk) {
                entry.removed_membership_tokens.push(t.clone());
            }
        }
    }

    let mut result: Vec<SyncPlaylistSongDeletion> = by_key.into_values().collect();
    result.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
    result.truncate(MAX_DELETIONS);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_remote_added_at_when_desktop_local_lacks_it() {
        let local_song = song("42", 0);
        let remote_song = song("42", 500);
        let base_key = remote_song.identity().stable_key();
        let mut base_snapshot = HashMap::new();
        base_snapshot.insert("1".to_string(), HashSet::from([base_key]));

        let merged = three_way_merge(
            &sync_data(vec![playlist(vec![local_song])]),
            &sync_data(vec![playlist(vec![remote_song])]),
            100,
            &base_snapshot,
        );

        assert_eq!(merged.playlists[0].songs[0].added_at, 500);
    }

    #[test]
    fn merge_keeps_existing_song_from_becoming_newer_than_remote_copy() {
        let local_song = song("42", 10_000);
        let remote_song = song("42", 500);
        let base_key = remote_song.identity().stable_key();
        let mut base_snapshot = HashMap::new();
        base_snapshot.insert("1".to_string(), HashSet::from([base_key]));

        let merged = three_way_merge(
            &sync_data(vec![playlist(vec![local_song])]),
            &sync_data(vec![playlist(vec![remote_song])]),
            100,
            &base_snapshot,
        );

        assert_eq!(merged.playlists[0].songs[0].added_at, 500);
    }

    #[test]
    fn data_change_detection_includes_playlist_order_metadata() {
        let remote = sync_data(vec![playlist(vec![song("42", 10_000)])]);
        let merged = sync_data(vec![playlist(vec![song("42", 500)])]);

        assert!(has_data_changed(&remote, &merged));
    }

    fn sync_data(playlists: Vec<SyncPlaylist>) -> SyncData {
        SyncData {
            playlists,
            ..Default::default()
        }
    }

    fn playlist(songs: Vec<SyncSong>) -> SyncPlaylist {
        SyncPlaylist {
            id: "1".into(),
            name: "Playlist".into(),
            songs,
            created_at: 10,
            modified_at: 20,
            is_deleted: false,
            song_order_version: DISPLAY_ORDER_SONG_ORDER_VERSION,
        }
    }

    fn song(id: &str, added_at: i64) -> SyncSong {
        SyncSong {
            id: id.into(),
            name: "Song".into(),
            album: "netease".into(),
            added_at,
            ..Default::default()
        }
    }
}
