// 三方合并算法：对齐 Android GitHubSyncManager.performThreeWayMerge
use std::collections::{BTreeMap, HashMap, HashSet};

use super::models::*;

const MAX_RECENT_PLAYS: usize = 500;
const MAX_DELETIONS: usize = 500;
const MAX_SYNC_LOG: usize = 100;
// 统计裁剪上限, 与 docs/SYNC-MODEL-CONTRACT.md §4 一致
// 无上限时 playbackStatBuckets 按「天 × 曲目」无限增长, 备份最终撑爆传输通道
const MAX_PLAYBACK_STATS: usize = 2_000;
const MAX_STAT_BUCKETS: usize = 8_000;
const STAT_BUCKET_RETENTION_DAYS: i64 = 400;
const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

pub fn three_way_merge(
    local: &SyncData,
    remote: &SyncData,
    last_sync_time: i64,
    base_snapshot: &HashMap<String, HashSet<String>>,
) -> SyncData {
    let local = local.normalized_for_sync();
    let remote = remote.normalized_for_sync();
    let playlist_deletions = merge_playlist_song_deletions(
        &local.playlist_song_deletions,
        &remote.playlist_song_deletions,
    );
    let playlists = merge_playlists(
        &local.playlists,
        &remote.playlists,
        last_sync_time,
        base_snapshot,
        &playlist_deletions,
    );
    let playlist_deletions = prune_playlist_song_deletions(&playlist_deletions, &playlists);
    let favorites = merge_favorite_playlists(&local.favorite_playlists, &remote.favorite_playlists);
    let recent_deletions = merge_recent_play_deletions(
        &local.recent_play_deletions,
        &remote.recent_play_deletions,
    );
    let recent = merge_recent_plays(&local.recent_plays, &remote.recent_plays, &recent_deletions);
    let recent_deletions = prune_recent_play_deletions(&recent_deletions, &recent);
    let playback_stats_cleared_at = local
        .playback_stats_cleared_at
        .max(remote.playback_stats_cleared_at)
        .max(0);

    let stat_buckets = merge_stat_buckets(
        &local.playback_stat_buckets,
        &remote.playback_stat_buckets,
        playback_stats_cleared_at,
    );
    let playback_stats = merge_playback_stats(
        &local.playback_stats,
        &remote.playback_stats,
        playback_stats_cleared_at,
    );
    // 先用未裁剪的全量分桶抬升聚合值, 再裁剪, 保证「总 >= 年」永远成立
    let playback_stats = lift_stats_to_bucket_totals(&playback_stats, &stat_buckets);

    SyncData {
        version: "2.0".into(),
        device_id: local.device_id.clone(),
        device_name: local.device_name.clone(),
        last_modified: chrono::Utc::now().timestamp_millis(),
        playlists,
        favorite_playlists: favorites,
        recent_plays: recent,
        sync_log: merge_sync_log(&local.sync_log, &remote.sync_log),
        recent_play_deletions: recent_deletions,
        playback_stats: trim_playback_stats(playback_stats),
        playback_stats_cleared_at,
        playback_stat_buckets: trim_stat_buckets(stat_buckets),
        playlist_song_deletions: playlist_deletions,
    }
}

/// 聚合统计不得小于同曲目日分桶之和
///
/// Android 的「总」读聚合值, 「日/周/月/年」读日分桶求和; 两者用了不同的合并代数
/// (聚合取 max, 分桶按天取 max 后再求和), 于是会出现「年 > 总」。
/// 这里只做单调抬升: 结果只增不减, 因此与对端的 max 合并天然收敛, 不会产生回声。
fn lift_stats_to_bucket_totals(
    stats: &[SyncTrackStat],
    buckets: &[SyncPlaybackStatBucket],
) -> Vec<SyncTrackStat> {
    if buckets.is_empty() {
        return stats.to_vec();
    }
    let mut totals: HashMap<&str, (i64, i64)> = HashMap::new();
    for bucket in buckets {
        let entry = totals.entry(bucket.identity_key.as_str()).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(bucket.total_listen_ms.max(0));
        entry.1 = entry.1.saturating_add(i64::from(bucket.play_count.max(0)));
    }

    stats
        .iter()
        .map(|stat| {
            let Some((listen_ms, play_count)) = totals.get(stat.identity_key.as_str()) else {
                return stat.clone();
            };
            let mut lifted = stat.clone();
            lifted.total_listen_ms = lifted.total_listen_ms.max(*listen_ms);
            lifted.play_count = lifted
                .play_count
                .max(i32::try_from(*play_count).unwrap_or(i32::MAX));
            lifted
        })
        .collect()
}

/// 聚合统计裁剪: 保留最近播放的条目, 排序稳定以保证幂等
fn trim_playback_stats(mut stats: Vec<SyncTrackStat>) -> Vec<SyncTrackStat> {
    if stats.len() <= MAX_PLAYBACK_STATS {
        return stats;
    }
    stats.sort_by(|left, right| {
        right
            .last_played_at
            .cmp(&left.last_played_at)
            .then_with(|| left.identity_key.cmp(&right.identity_key))
    });
    stats.truncate(MAX_PLAYBACK_STATS);
    stats
}

/// 日分桶裁剪: 保留窗口锚定在数据集内最新的一天(而非墙钟), 保证双端结果一致且幂等
fn trim_stat_buckets(buckets: Vec<SyncPlaybackStatBucket>) -> Vec<SyncPlaybackStatBucket> {
    let newest_day = buckets
        .iter()
        .map(|bucket| bucket.day_start_at)
        .max()
        .unwrap_or(0);
    let mut kept: Vec<SyncPlaybackStatBucket> = if newest_day > 0 {
        let cutoff = newest_day.saturating_sub(STAT_BUCKET_RETENTION_DAYS * MILLIS_PER_DAY);
        buckets
            .into_iter()
            .filter(|bucket| bucket.day_start_at >= cutoff)
            .collect()
    } else {
        buckets
    };

    if kept.len() > MAX_STAT_BUCKETS {
        kept.sort_by(|left, right| {
            right
                .day_start_at
                .cmp(&left.day_start_at)
                .then_with(|| right.play_count.cmp(&left.play_count))
                .then_with(|| left.identity_key.cmp(&right.identity_key))
        });
        kept.truncate(MAX_STAT_BUCKETS);
    }
    kept
}

fn merge_playlists(
    local: &[SyncPlaylist],
    remote: &[SyncPlaylist],
    last_sync_time: i64,
    base_snapshot: &HashMap<String, HashSet<String>>,
    deletions: &[SyncPlaylistSongDeletion],
) -> Vec<SyncPlaylist> {
    let local_map: HashMap<&str, &SyncPlaylist> = local
        .iter()
        .map(|playlist| (playlist.id.as_str(), playlist))
        .collect();
    let remote_map: HashMap<&str, &SyncPlaylist> = remote
        .iter()
        .map(|playlist| (playlist.id.as_str(), playlist))
        .collect();
    let all_ids: HashSet<&str> = local_map
        .keys()
        .chain(remote_map.keys())
        .copied()
        .collect();
    let mut merged = BTreeMap::new();

    for id in all_ids {
        let result = match (local_map.get(id).copied(), remote_map.get(id).copied()) {
            (Some(local_playlist), None) => apply_deletions_to_playlist(local_playlist, deletions),
            (None, Some(remote_playlist)) => apply_deletions_to_playlist(remote_playlist, deletions),
            (Some(local_playlist), Some(remote_playlist)) => {
                if local_playlist.is_deleted || remote_playlist.is_deleted {
                    merge_deleted_playlist(local_playlist, remote_playlist)
                } else {
                    merge_single_playlist(
                        local_playlist,
                        remote_playlist,
                        last_sync_time,
                        base_snapshot.get(id).cloned().unwrap_or_default(),
                        deletions,
                    )
                }
            }
            (None, None) => continue,
        };
        merged.insert(id.to_string(), result);
    }

    order_merged_playlists(&merged, local, remote, last_sync_time)
        .into_iter()
        .map(|playlist| playlist.normalized_for_display_order())
        .collect()
}

fn apply_deletions_to_playlist(
    playlist: &SyncPlaylist,
    deletions: &[SyncPlaylistSongDeletion],
) -> SyncPlaylist {
    if playlist.is_deleted {
        return playlist.clone();
    }
    let mut merged = playlist.clone();
    merged.songs = apply_playlist_song_deletions(&playlist.id, &playlist.songs, deletions);
    merged
}

fn merge_deleted_playlist(local: &SyncPlaylist, remote: &SyncPlaylist) -> SyncPlaylist {
    SyncPlaylist {
        id: local.id.clone(),
        name: if !local.name.trim().is_empty() {
            local.name.clone()
        } else {
            remote.name.clone()
        },
        songs: Vec::new(),
        created_at: min_positive(local.created_at, remote.created_at),
        modified_at: local.modified_at.max(remote.modified_at),
        is_deleted: true,
        song_order_version: DISPLAY_ORDER_SONG_ORDER_VERSION,
    }
}

fn merge_single_playlist(
    local: &SyncPlaylist,
    remote: &SyncPlaylist,
    last_sync_time: i64,
    base_songs: HashSet<String>,
    deletions: &[SyncPlaylistSongDeletion],
) -> SyncPlaylist {
    let local_changed = local.modified_at > last_sync_time;
    let remote_changed = remote.modified_at > last_sync_time;
    let name = if local.name == remote.name {
        local.name.clone()
    } else if remote_changed && !local_changed {
        remote.name.clone()
    } else if local_changed && !remote_changed {
        local.name.clone()
    } else if remote.modified_at > local.modified_at {
        remote.name.clone()
    } else {
        local.name.clone()
    };

    SyncPlaylist {
        id: local.id.clone(),
        name,
        songs: merge_songs(
            &local.songs,
            &remote.songs,
            local.modified_at,
            remote.modified_at,
            local_changed,
            remote_changed,
            last_sync_time,
            local.id == "-1001" || remote.id == "-1001",
            &base_songs,
            &local.id,
            deletions,
        ),
        created_at: min_positive(local.created_at, remote.created_at),
        modified_at: local.modified_at.max(remote.modified_at),
        is_deleted: false,
        song_order_version: DISPLAY_ORDER_SONG_ORDER_VERSION,
    }
}

fn order_merged_playlists(
    merged: &BTreeMap<String, SyncPlaylist>,
    local: &[SyncPlaylist],
    remote: &[SyncPlaylist],
    last_sync_time: i64,
) -> Vec<SyncPlaylist> {
    let local_changed = local.iter().any(|playlist| playlist.modified_at > last_sync_time);
    let remote_changed = remote.iter().any(|playlist| playlist.modified_at > last_sync_time);
    let (primary, secondary) = if remote_changed && !local_changed {
        (remote, local)
    } else {
        (local, remote)
    };
    let mut ordered_ids = Vec::new();
    let mut seen = HashSet::new();
    for playlist in primary.iter().chain(secondary.iter()) {
        if merged.contains_key(&playlist.id) && seen.insert(playlist.id.clone()) {
            ordered_ids.push(playlist.id.clone());
        }
    }
    for id in merged.keys() {
        if seen.insert(id.clone()) {
            ordered_ids.push(id.clone());
        }
    }
    ordered_ids
        .into_iter()
        .filter_map(|id| merged.get(&id).cloned())
        .collect()
}

// 播放/下载编排函数的参数都是相互独立的运行时上下文，聚成结构体只是换个地方堆字段
#[allow(clippy::too_many_arguments)]
fn merge_songs(
    local: &[SyncSong],
    remote: &[SyncSong],
    local_modified_at: i64,
    remote_modified_at: i64,
    local_changed: bool,
    remote_changed: bool,
    last_sync_time: i64,
    is_favorites: bool,
    base_songs: &HashSet<String>,
    playlist_id: &str,
    deletions: &[SyncPlaylistSongDeletion],
) -> Vec<SyncSong> {
    let local_is_empty = local.is_empty();
    let remote_is_empty = remote.is_empty();
    let local_has_membership_tokens = has_membership_tokens(local);
    let remote_has_membership_tokens = has_membership_tokens(remote);
    let prefer_remote_favorites =
        is_favorites && local_is_empty && !remote_is_empty && last_sync_time <= 0;

    let mut merged = if prefer_remote_favorites {
        deduplicate_songs(remote)
    } else if local_is_empty && remote_is_empty {
        Vec::new()
    } else if local_is_empty {
        if remote_has_membership_tokens {
            deduplicate_songs(remote)
        } else if local_changed && local_modified_at >= remote_modified_at {
            Vec::new()
        } else {
            deduplicate_songs(remote)
        }
    } else if remote_is_empty {
        if local_has_membership_tokens {
            deduplicate_songs(local)
        } else if remote_changed && remote_modified_at > local_modified_at {
            Vec::new()
        } else {
            deduplicate_songs(local)
        }
    } else if remote_changed && !local_changed {
        merge_membership_tokens_into_primary(remote, local)
    } else if local_changed && !remote_changed {
        merge_membership_tokens_into_primary(local, remote)
    } else if local_changed && local_modified_at > remote_modified_at {
        merge_concurrent_changes(local, remote)
    } else if local_changed && remote_modified_at > local_modified_at {
        merge_concurrent_changes(remote, local)
    } else {
        merge_songs_with_deterministic_payload(local, remote)
    };

    if !base_songs.is_empty() {
        merged.retain(|song| {
            if !song.sync_membership_tokens.is_empty() {
                return true;
            }
            let local_has = contains_matching_song(local, song);
            let remote_has = contains_matching_song(remote, song);
            if local_has == remote_has {
                return true;
            }
            !song_matches_base(song, base_songs)
        });
    }

    apply_playlist_song_deletions(playlist_id, &merged, deletions)
}

fn deduplicate_songs(songs: &[SyncSong]) -> Vec<SyncSong> {
    let mut accumulator = SongMergeAccumulator::new(false);
    for song in songs {
        accumulator.add_if_absent(song);
    }
    accumulator.into_songs()
}

fn merge_songs_with_deterministic_payload(
    local: &[SyncSong],
    remote: &[SyncSong],
) -> Vec<SyncSong> {
    let mut accumulator = SongMergeAccumulator::new(true);
    for song in local.iter().chain(remote) {
        accumulator.add_if_absent(song);
    }
    accumulator.into_songs()
}

fn merge_membership_tokens_into_primary(
    primary: &[SyncSong],
    secondary: &[SyncSong],
) -> Vec<SyncSong> {
    let mut accumulator = SongMergeAccumulator::new(false);
    for song in primary {
        accumulator.add_if_absent(song);
    }
    for song in secondary {
        accumulator.merge_matching_membership_tokens(song);
    }
    accumulator.into_songs()
}

fn merge_concurrent_changes(primary: &[SyncSong], secondary: &[SyncSong]) -> Vec<SyncSong> {
    let mut accumulator = SongMergeAccumulator::new(false);
    for song in primary.iter().chain(secondary) {
        accumulator.add_if_absent(song);
    }
    accumulator.into_songs()
}

fn has_membership_tokens(songs: &[SyncSong]) -> bool {
    songs
        .iter()
        .any(|song| !song.sync_membership_tokens.is_empty())
}

#[derive(Debug, Clone)]
struct SongMergeEntry {
    song: SyncSong,
    aliases: Vec<SyncSong>,
}

struct SongMergeAccumulator {
    entries: Vec<SongMergeEntry>,
    resolve_payload_deterministically: bool,
}

impl SongMergeAccumulator {
    fn new(resolve_payload_deterministically: bool) -> Self {
        Self {
            entries: Vec::new(),
            resolve_payload_deterministically,
        }
    }

    fn add_if_absent(&mut self, song: &SyncSong) {
        let normalized = song.normalized_for_sync();
        let matching_indices = self.matching_indices(&normalized);
        if matching_indices.is_empty() {
            self.entries.push(SongMergeEntry {
                aliases: vec![normalized.clone()],
                song: normalized,
            });
            return;
        }
        self.merge_matching_components(&matching_indices, normalized);
    }

    fn merge_matching_membership_tokens(&mut self, song: &SyncSong) {
        let normalized = song.normalized_for_sync();
        let matching_indices = self.matching_indices(&normalized);
        if !matching_indices.is_empty() {
            self.merge_matching_components(&matching_indices, normalized);
        }
    }

    fn matching_indices(&self, song: &SyncSong) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .aliases
                    .iter()
                    .any(|alias| songs_match(alias, song))
                    .then_some(index)
            })
            .collect()
    }

    fn merge_matching_components(&mut self, matching_indices: &[usize], other: SyncSong) {
        let primary_index = matching_indices[0];
        let mut payload_candidates: Vec<SyncSong> = matching_indices
            .iter()
            .map(|index| self.entries[*index].song.clone())
            .collect();
        payload_candidates.push(other.clone());

        let selected = if self.resolve_payload_deterministically {
            payload_candidates
                .iter()
                .max_by(|left, right| {
                    left.sync_metadata_version
                        .cmp(&right.sync_metadata_version)
                        .then_with(|| canonical_payload_key(left).cmp(&canonical_payload_key(right)))
                })
                .cloned()
                .unwrap_or_else(|| self.entries[primary_index].song.clone())
        } else {
            self.entries[primary_index].song.clone()
        };
        let mut resolved = resolve_selected_sync_payload(&selected, &payload_candidates);
        resolved.added_at = resolve_primary_added_at(selected.added_at, &payload_candidates);
        resolved.sync_metadata_version = CURRENT_SYNC_METADATA_VERSION;
        resolved.sync_membership_tokens = normalize_sync_causal_tokens(
            &payload_candidates
                .iter()
                .flat_map(|song| song.sync_membership_tokens.iter().cloned())
                .collect::<Vec<_>>(),
        );

        let mut aliases = self.entries[primary_index].aliases.clone();
        for index in matching_indices.iter().copied().skip(1) {
            for alias in &self.entries[index].aliases {
                if !aliases.iter().any(|known| same_song_payload(known, alias)) {
                    aliases.push(alias.clone());
                }
            }
        }
        if !aliases
            .iter()
            .any(|known| same_song_payload(known, &other))
        {
            aliases.push(other);
        }

        self.entries[primary_index] = SongMergeEntry {
            song: resolved,
            aliases,
        };
        for index in matching_indices.iter().copied().skip(1).rev() {
            self.entries.remove(index);
        }
    }

    fn into_songs(self) -> Vec<SyncSong> {
        self.entries.into_iter().map(|entry| entry.song).collect()
    }
}

fn songs_match(left: &SyncSong, right: &SyncSong) -> bool {
    if left
        .sync_membership_tokens
        .iter()
        .any(|token| right.sync_membership_tokens.contains(token))
    {
        return true;
    }
    if left.identity() == right.identity() {
        return true;
    }
    if left
        .identity_keys()
        .iter()
        .any(|key| right.identity_keys().iter().any(|other| key == other))
    {
        return true;
    }
    if channel_audio_key(left).is_some_and(|key| Some(key) == channel_audio_key(right)) {
        return true;
    }
    let same_id = !left.id.is_empty() && left.id != "0" && left.id == right.id;
    same_id
        && normalize_text(&left.name) == normalize_text(&right.name)
        && normalize_text(&left.artist) == normalize_text(&right.artist)
        && source_hints_compatible(left, right)
}

fn source_hints_compatible(left: &SyncSong, right: &SyncSong) -> bool {
    match (source_hint(left), source_hint(right)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn normalize_text(value: &str) -> String {
    value.trim().to_lowercase()
}

fn channel_audio_key(song: &SyncSong) -> Option<String> {
    let channel = song
        .channel_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_lowercase();
    let audio = song
        .audio_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let sub_audio = song.sub_audio_id.as_deref().unwrap_or_default().trim();
    Some(format!("{}|{}|{}", channel, audio, sub_audio))
}

fn source_hint(song: &SyncSong) -> Option<String> {
    if let Some(channel) = song
        .channel_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(channel.to_lowercase());
    }
    let album = song.album.trim().to_lowercase();
    if album.starts_with("netease") {
        Some("netease".into())
    } else if album.starts_with("bilibili") {
        Some("bilibili".into())
    } else if song.media_uri.to_lowercase().contains("youtube") {
        Some("youtube".into())
    } else {
        None
    }
}

fn canonical_payload_key(song: &SyncSong) -> String {
    let values = [
        song.id.clone(),
        song.name.clone(),
        song.artist.clone(),
        song.album.clone(),
        song.album_id.clone(),
        song.duration_ms.to_string(),
        song.cover_url.clone(),
        song.media_uri.clone(),
        song.added_at.to_string(),
        song.matched_lyric.clone().unwrap_or_default(),
        song.matched_translated_lyric.clone().unwrap_or_default(),
        song.matched_lyric_source.clone().unwrap_or_default(),
        song.matched_song_id.clone().unwrap_or_default(),
        song.user_lyric_offset_ms.to_string(),
        song.custom_cover_url.clone().unwrap_or_default(),
        song.custom_name.clone().unwrap_or_default(),
        song.custom_artist.clone().unwrap_or_default(),
        song.original_name.clone().unwrap_or_default(),
        song.original_artist.clone().unwrap_or_default(),
        song.original_cover_url.clone().unwrap_or_default(),
        song.original_lyric.clone().unwrap_or_default(),
        song.original_translated_lyric.clone().unwrap_or_default(),
        song.channel_id.clone().unwrap_or_default(),
        song.audio_id.clone().unwrap_or_default(),
        song.sub_audio_id.clone().unwrap_or_default(),
        song.playlist_context_id.clone().unwrap_or_default(),
        song.sync_metadata_version.to_string(),
    ];
    values
        .iter()
        .map(|value| format!("{}:{}", value.encode_utf16().count(), value))
        .collect::<Vec<_>>()
        .concat()
}

fn resolve_selected_sync_payload(selected: &SyncSong, candidates: &[SyncSong]) -> SyncSong {
    if selected.sync_metadata_version >= CURRENT_SYNC_METADATA_VERSION {
        return selected.clone();
    }

    if let Some(current) = candidates
        .iter()
        .filter(|song| song.sync_metadata_version >= CURRENT_SYNC_METADATA_VERSION)
        .max_by_key(|song| canonical_payload_key(song))
    {
        return current.clone();
    }

    let mut resolved = selected.clone();
    fill_missing_sync_metadata(&mut resolved, candidates);
    resolved
}

fn fill_missing_sync_metadata(target: &mut SyncSong, candidates: &[SyncSong]) {
    if target.id.is_empty() || target.id == "0" {
        target.id = first_non_empty(candidates, |song| {
            (song.id != "0").then_some(song.id.as_str())
        })
        .unwrap_or_default()
        .to_string();
    }
    if target.name.is_empty() {
        target.name = first_non_empty(candidates, |song| Some(song.name.as_str()))
            .unwrap_or_default()
            .to_string();
    }
    if target.artist.is_empty() {
        target.artist = first_non_empty(candidates, |song| Some(song.artist.as_str()))
            .unwrap_or_default()
            .to_string();
    }
    if target.album.is_empty() {
        target.album = first_non_empty(candidates, |song| Some(song.album.as_str()))
            .unwrap_or_default()
            .to_string();
    }
    if target.album_id.is_empty() {
        target.album_id = first_non_empty(candidates, |song| Some(song.album_id.as_str()))
            .unwrap_or_default()
            .to_string();
    }
    if target.duration_ms <= 0 {
        target.duration_ms = candidates
            .iter()
            .map(|song| song.duration_ms)
            .find(|duration| *duration > 0)
            .unwrap_or_default();
    }
    if target.cover_url.is_empty() {
        target.cover_url = first_non_empty(candidates, |song| Some(song.cover_url.as_str()))
            .unwrap_or_default()
            .to_string();
    }
    if target.media_uri.is_empty() {
        target.media_uri = first_non_empty(candidates, |song| Some(song.media_uri.as_str()))
            .unwrap_or_default()
            .to_string();
    }

    fill_missing_option(&mut target.matched_lyric, candidates, |song| {
        song.matched_lyric.as_deref()
    });
    fill_missing_option(
        &mut target.matched_translated_lyric,
        candidates,
        |song| song.matched_translated_lyric.as_deref(),
    );
    fill_missing_option(&mut target.matched_lyric_source, candidates, |song| {
        song.matched_lyric_source.as_deref()
    });
    fill_missing_option(&mut target.matched_song_id, candidates, |song| {
        song.matched_song_id.as_deref()
    });
    fill_missing_option(&mut target.custom_cover_url, candidates, |song| {
        song.custom_cover_url.as_deref()
    });
    fill_missing_option(&mut target.custom_name, candidates, |song| {
        song.custom_name.as_deref()
    });
    fill_missing_option(&mut target.custom_artist, candidates, |song| {
        song.custom_artist.as_deref()
    });
    fill_missing_option(&mut target.original_name, candidates, |song| {
        song.original_name.as_deref()
    });
    fill_missing_option(&mut target.original_artist, candidates, |song| {
        song.original_artist.as_deref()
    });
    fill_missing_option(&mut target.original_cover_url, candidates, |song| {
        song.original_cover_url.as_deref()
    });
    fill_missing_option(&mut target.original_lyric, candidates, |song| {
        song.original_lyric.as_deref()
    });
    fill_missing_option(
        &mut target.original_translated_lyric,
        candidates,
        |song| song.original_translated_lyric.as_deref(),
    );
    fill_missing_option(&mut target.channel_id, candidates, |song| {
        song.channel_id.as_deref()
    });
    fill_missing_option(&mut target.audio_id, candidates, |song| {
        song.audio_id.as_deref()
    });
    fill_missing_option(&mut target.sub_audio_id, candidates, |song| {
        song.sub_audio_id.as_deref()
    });
    fill_missing_option(&mut target.playlist_context_id, candidates, |song| {
        song.playlist_context_id.as_deref()
    });
    if target.user_lyric_offset_ms == 0 {
        target.user_lyric_offset_ms = candidates
            .iter()
            .map(|song| song.user_lyric_offset_ms)
            .find(|offset| *offset != 0)
            .unwrap_or_default();
    }
}

fn fill_missing_option<F>(
    target: &mut Option<String>,
    candidates: &[SyncSong],
    selector: F,
) where
    F: for<'a> Fn(&'a SyncSong) -> Option<&'a str>,
{
    if target.as_deref().is_some_and(|value| !value.trim().is_empty()) {
        return;
    }
    *target = first_non_empty(candidates, selector).map(String::from);
}

fn first_non_empty<'a, F>(candidates: &'a [SyncSong], selector: F) -> Option<&'a str>
where
    F: Fn(&'a SyncSong) -> Option<&'a str>,
{
    candidates
        .iter()
        .filter_map(selector)
        .find(|value| !value.trim().is_empty())
}

fn same_song_payload(left: &SyncSong, right: &SyncSong) -> bool {
    canonical_payload_key(left) == canonical_payload_key(right)
        && normalize_sync_causal_tokens(&left.sync_membership_tokens)
            == normalize_sync_causal_tokens(&right.sync_membership_tokens)
}

fn contains_matching_song(songs: &[SyncSong], target: &SyncSong) -> bool {
    songs.iter().any(|song| songs_match(song, target))
}

fn song_matches_base(song: &SyncSong, base_songs: &HashSet<String>) -> bool {
    song.identity_keys().iter().any(|key| base_songs.contains(key))
}

fn resolve_primary_added_at(selected: i64, candidates: &[SyncSong]) -> i64 {
    if selected > 0 {
        return selected;
    }
    candidates
        .iter()
        .map(|song| song.added_at)
        .max()
        .unwrap_or_default()
        .max(0)
}

fn apply_playlist_song_deletions(
    playlist_id: &str,
    songs: &[SyncSong],
    deletions: &[SyncPlaylistSongDeletion],
) -> Vec<SyncSong> {
    let relevant: Vec<&SyncPlaylistSongDeletion> = deletions
        .iter()
        .filter(|deletion| deletion.playlist_id == playlist_id)
        .collect();
    if relevant.is_empty() {
        return songs.to_vec();
    }
    let causal_tokens: HashSet<SyncCausalToken> = relevant
        .iter()
        .flat_map(|deletion| deletion.removed_membership_tokens.iter().cloned())
        .collect();
    songs
        .iter()
        .filter_map(|song| {
            let identity_deletions: Vec<&SyncPlaylistSongDeletion> = relevant
                .iter()
                .copied()
                .filter(|deletion| deletion.matches_song(playlist_id, song))
                .collect();
            let song_tokens = normalize_sync_causal_tokens(&song.sync_membership_tokens);
            if song_tokens.is_empty() {
                let latest = identity_deletions
                    .iter()
                    .max_by(|left, right| deletion_cmp(left, right));
                return match latest {
                    Some(deletion) if song.added_at <= deletion.deleted_at => None,
                    _ => Some(song.clone()),
                };
            }
            let remaining: Vec<SyncCausalToken> = song_tokens
                .into_iter()
                .filter(|token| !causal_tokens.contains(token))
                .collect();
            if remaining.is_empty() {
                None
            } else {
                let mut surviving = song.clone();
                surviving.sync_membership_tokens = remaining;
                Some(surviving)
            }
        })
        .collect()
}

fn merge_favorite_playlists(
    local: &[SyncFavoritePlaylist],
    remote: &[SyncFavoritePlaylist],
) -> Vec<SyncFavoritePlaylist> {
    let mut groups: BTreeMap<String, Vec<SyncFavoritePlaylist>> = BTreeMap::new();
    for favorite in local.iter().chain(remote.iter()) {
        groups
            .entry(favorite.group_key())
            .or_default()
            .push(favorite.normalized_for_sync());
    }
    let mut result: Vec<SyncFavoritePlaylist> = groups
        .into_values()
        .map(|snapshots| snapshots.into_iter().reduce(|left, right| merge_single_favorite(&left, &right)).unwrap())
        .collect();
    result.sort_by(|left, right| {
        right
            .sort_order
            .cmp(&left.sort_order)
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| right.added_time.cmp(&left.added_time))
            .then_with(|| left.group_key().cmp(&right.group_key()))
    });
    result
}

fn merge_single_favorite(left: &SyncFavoritePlaylist, right: &SyncFavoritePlaylist) -> SyncFavoritePlaylist {
    let left = left.normalized_for_sync();
    let right = right.normalized_for_sync();
    let newer = if right.modified_at > left.modified_at { &right } else { &left };
    let older = if std::ptr::eq(newer, &left) { &right } else { &left };
    if left.is_deleted != right.is_deleted {
        if left.modified_at == right.modified_at {
            let mut result = newer.clone();
            if result.is_deleted {
                result.songs.clear();
                result.track_count = 0;
            } else {
                result.songs = deduplicate_songs(&[left.songs.clone(), right.songs.clone()].concat());
                result.track_count = left.track_count.max(right.track_count).max(result.songs.len() as i32);
            }
            return result;
        }
        if newer.is_deleted {
            let mut result = newer.clone();
            result.songs.clear();
            result.track_count = 0;
            result.sort_order = left.sort_order.max(right.sort_order);
            return result;
        }
        let mut result = newer.clone();
        result.songs = deduplicate_songs(&[left.songs.clone(), right.songs.clone()].concat());
        result.track_count = left.track_count.max(right.track_count).max(result.songs.len() as i32);
        if result.sort_order == 0 { result.sort_order = older.sort_order; }
        return result;
    }
    if newer.is_deleted {
        let mut result = newer.clone();
        result.songs.clear();
        result.track_count = 0;
        result.added_time = left.added_time.max(right.added_time);
        result.sort_order = left.sort_order.max(right.sort_order);
        return result;
    }
    let mut result = newer.clone();
    result.cover_url = if !newer.cover_url.is_empty() {
        newer.cover_url.clone()
    } else {
        older.cover_url.clone()
    };
    result.songs = deduplicate_songs(&[left.songs.clone(), right.songs.clone()].concat());
    result.track_count = left.track_count.max(right.track_count).max(result.songs.len() as i32);
    result.added_time = left.added_time.max(right.added_time);
    result.modified_at = left.modified_at.max(right.modified_at);
    if result.sort_order == 0 { result.sort_order = older.sort_order; }
    result.is_deleted = false;
    result
}

fn merge_recent_plays(
    local: &[SyncRecentPlay],
    remote: &[SyncRecentPlay],
    deletions: &[SyncRecentPlayDeletion],
) -> Vec<SyncRecentPlay> {
    let mut all: Vec<SyncRecentPlay> = local.iter().chain(remote.iter()).cloned().collect();
    all.sort_by(|left, right| {
        right
            .played_at
            .cmp(&left.played_at)
            .then_with(|| right.device_id.cmp(&left.device_id))
            .then_with(|| recent_song_key(left).cmp(&recent_song_key(right)))
    });
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for mut recent in all {
        recent.song = recent.song.normalized_for_sync();
        let key = recent_song_key(&recent);
        if !seen.insert(key.clone()) || recent_is_deleted(&recent.song, recent.played_at, deletions) {
            continue;
        }
        result.push(recent);
    }
    result.truncate(MAX_RECENT_PLAYS);
    result
}

fn recent_song_key(recent: &SyncRecentPlay) -> String {
    recent.song.identity().stable_key()
}

fn recent_is_deleted(song: &SyncSong, played_at: i64, deletions: &[SyncRecentPlayDeletion]) -> bool {
    deletions.iter().any(|deletion| {
        let matches = deletion
            .identity_keys()
            .iter()
            .any(|key| song.identity_keys().iter().any(|song_key| song_key == key));
        matches && deletion.deleted_at > played_at
    })
}

fn merge_recent_play_deletions(
    local: &[SyncRecentPlayDeletion],
    remote: &[SyncRecentPlayDeletion],
) -> Vec<SyncRecentPlayDeletion> {
    let mut groups: BTreeMap<String, Vec<SyncRecentPlayDeletion>> = BTreeMap::new();
    for deletion in local.iter().chain(remote.iter()) {
        groups
            .entry(deletion.identity().stable_key())
            .or_default()
            .push(deletion.clone());
    }
    let mut result: Vec<SyncRecentPlayDeletion> = groups
        .into_values()
        .filter_map(|snapshots| snapshots.into_iter().max_by(recent_deletion_cmp))
        .collect();
    result.sort_by(|left, right| {
        right
            .deleted_at
            .cmp(&left.deleted_at)
            .then_with(|| right.device_id.cmp(&left.device_id))
            .then_with(|| left.identity().stable_key().cmp(&right.identity().stable_key()))
    });
    result.truncate(MAX_DELETIONS);
    result
}

fn recent_deletion_cmp(left: &SyncRecentPlayDeletion, right: &SyncRecentPlayDeletion) -> std::cmp::Ordering {
    left.deleted_at
        .cmp(&right.deleted_at)
        .then_with(|| left.device_id.cmp(&right.device_id))
}

fn prune_recent_play_deletions(
    deletions: &[SyncRecentPlayDeletion],
    recent: &[SyncRecentPlay],
) -> Vec<SyncRecentPlayDeletion> {
    let mut result: Vec<SyncRecentPlayDeletion> = deletions
        .iter()
        .filter(|deletion| {
            recent.iter().all(|played| {
                let matches = deletion
                    .identity_keys()
                    .iter()
                    .any(|key| played.song.identity_keys().iter().any(|song_key| song_key == key));
                !matches || played.played_at <= deletion.deleted_at
            })
        })
        .cloned()
        .collect();
    result.sort_by(|left, right| right.deleted_at.cmp(&left.deleted_at).then_with(|| right.device_id.cmp(&left.device_id)));
    result.truncate(MAX_DELETIONS);
    result
}

fn merge_sync_log(local: &[SyncLogEntry], remote: &[SyncLogEntry]) -> Vec<SyncLogEntry> {
    let mut unique = BTreeMap::new();
    for entry in local.iter().chain(remote.iter()) {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            entry.timestamp,
            entry.device_id,
            entry.action,
            entry.playlist_id.as_deref().unwrap_or_default(),
            entry.song_id.as_deref().unwrap_or_default(),
            entry.details.as_deref().unwrap_or_default()
        );
        unique.insert(key, entry.clone());
    }
    let mut result: Vec<SyncLogEntry> = unique.into_values().collect();
    result.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| right.device_id.cmp(&left.device_id))
            .then_with(|| left.action.cmp(&right.action))
    });
    result.truncate(MAX_SYNC_LOG);
    result
}

fn merge_playlist_song_deletions(
    local: &[SyncPlaylistSongDeletion],
    remote: &[SyncPlaylistSongDeletion],
) -> Vec<SyncPlaylistSongDeletion> {
    let mut groups: BTreeMap<String, Vec<SyncPlaylistSongDeletion>> = BTreeMap::new();
    for deletion in local.iter().chain(remote.iter()) {
        let mut normalized = deletion.clone();
        normalized.removed_membership_tokens = normalize_sync_causal_tokens(
            &deletion.removed_membership_tokens,
        );
        groups
            .entry(normalized.identity())
            .or_default()
            .push(normalized);
    }
    let mut result = Vec::new();
    for snapshots in groups.into_values() {
        let legacy = snapshots
            .iter()
            .filter(|deletion| deletion.removed_membership_tokens.is_empty())
            .max_by(|left, right| deletion_cmp(left, right))
            .cloned();
        let causal_snapshots: Vec<&SyncPlaylistSongDeletion> = snapshots
            .iter()
            .filter(|deletion| !deletion.removed_membership_tokens.is_empty())
            .collect();
        let causal = causal_snapshots.iter().max_by(|left, right| deletion_cmp(left, right)).map(|deletion| {
            let mut merged = (*deletion).clone();
            let tokens: Vec<SyncCausalToken> = causal_snapshots
                .iter()
                .flat_map(|snapshot| snapshot.removed_membership_tokens.iter().cloned())
                .collect();
            merged.removed_membership_tokens = normalize_sync_causal_tokens(&tokens);
            merged
        });
        if let Some(legacy) = legacy { result.push(legacy); }
        if let Some(causal) = causal { result.push(causal); }
    }
    result.sort_by(deletion_order_cmp);
    result.truncate(MAX_DELETIONS);
    result
}

fn deletion_cmp(left: &SyncPlaylistSongDeletion, right: &SyncPlaylistSongDeletion) -> std::cmp::Ordering {
    left.deleted_at
        .cmp(&right.deleted_at)
        .then_with(|| left.device_id.cmp(&right.device_id))
}

fn deletion_order_cmp(left: &SyncPlaylistSongDeletion, right: &SyncPlaylistSongDeletion) -> std::cmp::Ordering {
    right
        .deleted_at
        .cmp(&left.deleted_at)
        .then_with(|| right.device_id.cmp(&left.device_id))
        .then_with(|| left.identity().cmp(&right.identity()))
        .then_with(|| left.removed_membership_tokens.is_empty().cmp(&right.removed_membership_tokens.is_empty()))
}

fn prune_playlist_song_deletions(
    deletions: &[SyncPlaylistSongDeletion],
    playlists: &[SyncPlaylist],
) -> Vec<SyncPlaylistSongDeletion> {
    let mut result: Vec<SyncPlaylistSongDeletion> = deletions
        .iter()
        .filter(|deletion| {
            if !deletion.removed_membership_tokens.is_empty() {
                return true;
            }
            playlists.iter().all(|playlist| {
                playlist.id != deletion.playlist_id
                    || playlist.is_deleted
                    || playlist.songs.iter().all(|song| {
                        !deletion.matches_song(&playlist.id, song) || song.added_at <= deletion.deleted_at
                    })
            })
        })
        .cloned()
        .collect();
    result.sort_by(deletion_order_cmp);
    result.truncate(MAX_DELETIONS);
    result
}

#[derive(Default)]
struct MergedCounters {
    total_listen_ms: i64,
    play_count: i32,
    first_played_at: i64,
    last_played_at: i64,
    base_listen_ms: i64,
    base_play_count: i32,
    shards: Vec<SyncPlaybackCounterShard>,
}

fn merge_counter_shards(
    local: &[SyncPlaybackCounterShard],
    remote: &[SyncPlaybackCounterShard],
) -> Vec<SyncPlaybackCounterShard> {
    let mut grouped: BTreeMap<(String, i64), SyncPlaybackCounterShard> = BTreeMap::new();
    for raw in local.iter().chain(remote.iter()) {
        if raw.device_id.trim().is_empty() {
            continue;
        }
        let mut shard = raw.clone();
        shard.epoch_started_at = shard.epoch_started_at.max(0);
        shard.total_listen_ms = shard.total_listen_ms.max(0);
        shard.play_count = shard.play_count.max(0);
        shard.last_played_at = shard.last_played_at.max(0);
        shard.first_played_at = if shard.first_played_at <= 0 || shard.first_played_at > shard.last_played_at {
            shard.last_played_at
        } else {
            shard.first_played_at
        };
        let key = (shard.device_id.clone(), shard.epoch_started_at);
        grouped
            .entry(key)
            .and_modify(|existing| {
                existing.total_listen_ms = existing.total_listen_ms.max(shard.total_listen_ms);
                existing.play_count = existing.play_count.max(shard.play_count);
                existing.first_played_at = min_positive(existing.first_played_at, shard.first_played_at);
                existing.last_played_at = existing.last_played_at.max(shard.last_played_at);
            })
            .or_insert(shard);
    }
    grouped.into_values().collect()
}

fn normalize_stat_after_clear(stat: &SyncTrackStat, cleared_at: i64) -> Option<SyncTrackStat> {
    if cleared_at > 0 && stat.last_played_at < cleared_at {
        return None;
    }
    let mut normalized = stat.clone();
    normalized.total_listen_ms = normalized.total_listen_ms.max(0);
    normalized.play_count = normalized.play_count.max(0);
    normalized.counter_shards = merge_counter_shards(&stat.counter_shards, &[]);
    if cleared_at > 0 {
        normalized.counter_shards.retain(|shard| shard.last_played_at >= cleared_at);
        normalized.counter_shards = merge_counter_shards(&normalized.counter_shards, &[]);
        if !normalized.counter_shards.is_empty() {
            normalized.counter_base_listen_ms = 0;
            normalized.counter_base_play_count = 0;
        }
        normalized.first_played_at = normalized
            .first_played_at
            .max(cleared_at)
            .min(normalized.last_played_at.max(cleared_at));
    }
    Some(normalized)
}

fn normalize_bucket_after_clear(bucket: &SyncPlaybackStatBucket, cleared_at: i64) -> Option<SyncPlaybackStatBucket> {
    if cleared_at > 0 && bucket.last_played_at < cleared_at {
        return None;
    }
    let mut normalized = bucket.clone();
    normalized.total_listen_ms = normalized.total_listen_ms.max(0);
    normalized.play_count = normalized.play_count.max(0);
    normalized.counter_shards = merge_counter_shards(&bucket.counter_shards, &[]);
    if cleared_at > 0 {
        normalized.counter_shards.retain(|shard| shard.last_played_at >= cleared_at);
        normalized.counter_shards = merge_counter_shards(&normalized.counter_shards, &[]);
        if !normalized.counter_shards.is_empty() {
            normalized.counter_base_listen_ms = 0;
            normalized.counter_base_play_count = 0;
        }
        normalized.first_played_at = normalized
            .first_played_at
            .max(cleared_at)
            .min(normalized.last_played_at.max(cleared_at));
    }
    Some(normalized)
}

/// 参与计数合并的一侧快照
///
/// 聚合统计与日分桶字段完全同构，用同一个快照类型描述，避免把 14 个
/// 平铺参数在调用点排错顺序 —— 这里一旦错位是静默的数据损坏。
struct CounterSide<'a> {
    total_listen_ms: i64,
    play_count: i32,
    first_played_at: i64,
    last_played_at: i64,
    base_listen_ms: i64,
    base_play_count: i32,
    shards: &'a [SyncPlaybackCounterShard],
}

impl CounterSide<'_> {
    /// 分片机制上线前的历史存量：没有分片且 base 为 0 时，总量本身就是 base
    fn effective_base_listen_ms(&self) -> i64 {
        if self.shards.is_empty() && self.base_listen_ms == 0 {
            self.total_listen_ms.max(0)
        } else {
            self.base_listen_ms.max(0)
        }
    }

    fn effective_base_play_count(&self) -> i32 {
        if self.shards.is_empty() && self.base_play_count == 0 {
            self.play_count.max(0)
        } else {
            self.base_play_count.max(0)
        }
    }
}

fn merge_counter_values(local: CounterSide<'_>, remote: CounterSide<'_>) -> MergedCounters {
    let shards = merge_counter_shards(local.shards, remote.shards);
    let base_total = local
        .effective_base_listen_ms()
        .max(remote.effective_base_listen_ms());
    let base_count = local
        .effective_base_play_count()
        .max(remote.effective_base_play_count());
    let sharded_total = base_total.saturating_add(shards.iter().map(|shard| shard.total_listen_ms).sum::<i64>());
    let sharded_count = base_count.saturating_add(shards.iter().map(|shard| shard.play_count).sum::<i32>());
    MergedCounters {
        total_listen_ms: sharded_total
            .max(local.total_listen_ms)
            .max(remote.total_listen_ms)
            .max(0),
        play_count: sharded_count
            .max(local.play_count)
            .max(remote.play_count)
            .max(0),
        first_played_at: min_positive(
            min_positive(local.first_played_at, remote.first_played_at),
            shards.iter().map(|shard| shard.first_played_at).fold(0, min_positive),
        ),
        last_played_at: local
            .last_played_at
            .max(remote.last_played_at)
            .max(shards.iter().map(|shard| shard.last_played_at).max().unwrap_or(0)),
        base_listen_ms: base_total,
        base_play_count: base_count,
        shards,
    }
}

fn merge_playback_stats(
    local: &[SyncTrackStat],
    remote: &[SyncTrackStat],
    cleared_at: i64,
) -> Vec<SyncTrackStat> {
    let mut grouped: BTreeMap<String, SyncTrackStat> = BTreeMap::new();
    for stat in local.iter().chain(remote.iter()) {
        let Some(stat) = normalize_stat_after_clear(stat, cleared_at) else { continue };
        if let Some(existing) = grouped.remove(&stat.identity_key) {
            let newer = if stat.last_played_at >= existing.last_played_at { &stat } else { &existing };
            let counters = merge_counter_values(
                CounterSide {
                    total_listen_ms: existing.total_listen_ms,
                    play_count: existing.play_count,
                    first_played_at: existing.first_played_at,
                    last_played_at: existing.last_played_at,
                    base_listen_ms: existing.counter_base_listen_ms,
                    base_play_count: existing.counter_base_play_count,
                    shards: &existing.counter_shards,
                },
                CounterSide {
                    total_listen_ms: stat.total_listen_ms,
                    play_count: stat.play_count,
                    first_played_at: stat.first_played_at,
                    last_played_at: stat.last_played_at,
                    base_listen_ms: stat.counter_base_listen_ms,
                    base_play_count: stat.counter_base_play_count,
                    shards: &stat.counter_shards,
                },
            );
            let mut merged = newer.clone();
            merged.total_listen_ms = counters.total_listen_ms;
            merged.play_count = counters.play_count;
            merged.first_played_at = counters.first_played_at;
            merged.last_played_at = counters.last_played_at;
            merged.counter_base_listen_ms = counters.base_listen_ms;
            merged.counter_base_play_count = counters.base_play_count;
            merged.counter_shards = counters.shards;
            grouped.insert(stat.identity_key.clone(), merged);
        } else {
            grouped.insert(stat.identity_key.clone(), stat);
        }
    }
    grouped.into_values().collect()
}

fn merge_stat_buckets(
    local: &[SyncPlaybackStatBucket],
    remote: &[SyncPlaybackStatBucket],
    cleared_at: i64,
) -> Vec<SyncPlaybackStatBucket> {
    let mut grouped: BTreeMap<(i64, String), SyncPlaybackStatBucket> = BTreeMap::new();
    for bucket in local.iter().chain(remote.iter()) {
        let Some(bucket) = normalize_bucket_after_clear(bucket, cleared_at) else { continue };
        let key = (bucket.day_start_at, bucket.identity_key.clone());
        if let Some(existing) = grouped.remove(&key) {
            let newer = if bucket.last_played_at >= existing.last_played_at { &bucket } else { &existing };
            let counters = merge_counter_values(
                CounterSide {
                    total_listen_ms: existing.total_listen_ms,
                    play_count: existing.play_count,
                    first_played_at: existing.first_played_at,
                    last_played_at: existing.last_played_at,
                    base_listen_ms: existing.counter_base_listen_ms,
                    base_play_count: existing.counter_base_play_count,
                    shards: &existing.counter_shards,
                },
                CounterSide {
                    total_listen_ms: bucket.total_listen_ms,
                    play_count: bucket.play_count,
                    first_played_at: bucket.first_played_at,
                    last_played_at: bucket.last_played_at,
                    base_listen_ms: bucket.counter_base_listen_ms,
                    base_play_count: bucket.counter_base_play_count,
                    shards: &bucket.counter_shards,
                },
            );
            let mut merged = newer.clone();
            merged.total_listen_ms = counters.total_listen_ms;
            merged.play_count = counters.play_count;
            merged.first_played_at = counters.first_played_at;
            merged.last_played_at = counters.last_played_at;
            merged.counter_base_listen_ms = counters.base_listen_ms;
            merged.counter_base_play_count = counters.base_play_count;
            merged.counter_shards = counters.shards;
            grouped.insert(key, merged);
        } else {
            grouped.insert(key, bucket);
        }
    }
    grouped.into_values().collect()
}

/// 按稳定键排序后逐条比较
///
/// 直接 zip 比较会把「仅顺序不同」误判为有变更, 触发无意义上传;
/// 对端合并后顺序又可能变回去, 形成永不收敛的回声。
fn pairs_by_key<'a, T, K, F>(left: &'a [T], right: &'a [T], key: F) -> Option<Vec<(&'a T, &'a T)>>
where
    K: Ord,
    F: Fn(&T) -> K,
{
    if left.len() != right.len() {
        return None;
    }
    let mut left: Vec<&T> = left.iter().collect();
    let mut right: Vec<&T> = right.iter().collect();
    left.sort_by_key(|item| key(item));
    right.sort_by_key(|item| key(item));
    Some(left.into_iter().zip(right).collect())
}

pub fn has_data_changed(remote: &SyncData, merged: &SyncData) -> bool {
    let remote = remote.normalized_for_sync();
    let merged = merged.normalized_for_sync();
    match pairs_by_key(&remote.playlists, &merged.playlists, |playlist| {
        playlist.id.clone()
    }) {
        Some(pairs) => {
            if pairs.iter().any(|(left, right)| !same_playlist(left, right)) {
                return true;
            }
        }
        None => return true,
    }
    match pairs_by_key(
        &remote.favorite_playlists,
        &merged.favorite_playlists,
        SyncFavoritePlaylist::group_key,
    ) {
        Some(pairs) => {
            if pairs.iter().any(|(left, right)| !same_favorite(left, right)) {
                return true;
            }
        }
        None => return true,
    }
    match pairs_by_key(&remote.recent_plays, &merged.recent_plays, |play| {
        (play.song.identity().stable_key(), play.played_at)
    }) {
        Some(pairs) => {
            if pairs.iter().any(|(left, right)| {
                left.song_id != right.song_id
                    || left.played_at != right.played_at
                    || left.device_id != right.device_id
                    || !same_song(&left.song, &right.song)
            }) {
                return true;
            }
        }
        None => return true,
    }
    match pairs_by_key(
        &remote.recent_play_deletions,
        &merged.recent_play_deletions,
        |deletion| deletion.identity().stable_key(),
    ) {
        Some(pairs) => {
            if pairs.iter().any(|(left, right)| {
                left.identity() != right.identity()
                    || left.deleted_at != right.deleted_at
                    || left.device_id != right.device_id
            }) {
                return true;
            }
        }
        None => return true,
    }
    match pairs_by_key(
        &remote.playlist_song_deletions,
        &merged.playlist_song_deletions,
        |deletion| {
            (
                deletion.identity(),
                deletion.removed_membership_tokens.is_empty(),
            )
        },
    ) {
        Some(pairs) => {
            if pairs.iter().any(|(left, right)| {
                left.identity() != right.identity()
                    || left.deleted_at != right.deleted_at
                    || left.device_id != right.device_id
                    || normalize_sync_causal_tokens(&left.removed_membership_tokens)
                        != normalize_sync_causal_tokens(&right.removed_membership_tokens)
            }) {
                return true;
            }
        }
        None => return true,
    }
    if remote.playback_stats_cleared_at != merged.playback_stats_cleared_at
        || !same_stats(&remote.playback_stats, &merged.playback_stats)
        || !same_buckets(&remote.playback_stat_buckets, &merged.playback_stat_buckets)
    {
        return true;
    }
    false
}

fn same_playlist(left: &SyncPlaylist, right: &SyncPlaylist) -> bool {
    // 对齐 Android SyncDataChangeDetector: 仅比较内容, 不比较 created_at/modified_at
    // 时间戳单调增长, 若纳入比较会把"仅时间戳变化"误判为改动, 导致同步回流/回声上传
    left.id == right.id
        && left.name == right.name
        && left.is_deleted == right.is_deleted
        && left.song_order_version == right.song_order_version
        && left.songs.len() == right.songs.len()
        && left.songs.iter().zip(&right.songs).all(|(a, b)| same_song(a, b))
}

fn same_favorite(left: &SyncFavoritePlaylist, right: &SyncFavoritePlaylist) -> bool {
    // 对齐 Android SyncDataChangeDetector.favorite:
    // 比较 isDeleted/modifiedAt/sortOrder/trackCount/song identity+metadata
    // 不比较 name/cover/source 展示字段与 addedTime, 避免仅展示刷新触发回声上传
    left.id == right.id
        && left.source == right.source
        && left.modified_at == right.modified_at
        && left.is_deleted == right.is_deleted
        && left.sort_order == right.sort_order
        && left.track_count == right.track_count
        && left.songs.len() == right.songs.len()
        && left.songs.iter().zip(&right.songs).all(|(a, b)| same_song(a, b))
}

fn same_song(left: &SyncSong, right: &SyncSong) -> bool {
    left.identity_keys() == right.identity_keys()
        && left.name == right.name
        && left.artist == right.artist
        && left.album == right.album
        && left.album_id == right.album_id
        && left.duration_ms == right.duration_ms
        && left.cover_url == right.cover_url
        && left.media_uri == right.media_uri
        && left.added_at == right.added_at
        && left.matched_lyric == right.matched_lyric
        && left.matched_translated_lyric == right.matched_translated_lyric
        && left.matched_lyric_source == right.matched_lyric_source
        && left.matched_song_id == right.matched_song_id
        && left.user_lyric_offset_ms == right.user_lyric_offset_ms
        && left.custom_cover_url == right.custom_cover_url
        && left.custom_name == right.custom_name
        && left.custom_artist == right.custom_artist
        && left.original_name == right.original_name
        && left.original_artist == right.original_artist
        && left.original_cover_url == right.original_cover_url
        && left.original_lyric == right.original_lyric
        && left.original_translated_lyric == right.original_translated_lyric
        && left.channel_id == right.channel_id
        && left.audio_id == right.audio_id
        && left.sub_audio_id == right.sub_audio_id
        && left.playlist_context_id == right.playlist_context_id
        && left.sync_metadata_version == right.sync_metadata_version
        && normalize_sync_causal_tokens(&left.sync_membership_tokens)
            == normalize_sync_causal_tokens(&right.sync_membership_tokens)
}

fn same_stats(left: &[SyncTrackStat], right: &[SyncTrackStat]) -> bool {
    if left.len() != right.len() { return false; }
    let mut left = left.iter().collect::<Vec<_>>();
    let mut right = right.iter().collect::<Vec<_>>();
    left.sort_by(|a, b| a.identity_key.cmp(&b.identity_key));
    right.sort_by(|a, b| a.identity_key.cmp(&b.identity_key));
    left.iter().zip(right).all(|(a, b)| {
        a.identity_key == b.identity_key
            && a.name == b.name
            && a.artist == b.artist
            && a.album == b.album
            && a.total_listen_ms == b.total_listen_ms
            && a.play_count == b.play_count
            && a.last_played_at == b.last_played_at
            && a.first_played_at == b.first_played_at
            && a.cover_url == b.cover_url
            && a.duration_ms == b.duration_ms
            && a.media_uri == b.media_uri
            && a.id == b.id
            && a.album_id == b.album_id
            && a.counter_base_listen_ms == b.counter_base_listen_ms
            && a.counter_base_play_count == b.counter_base_play_count
            && merge_counter_shards(&a.counter_shards, &[]) == merge_counter_shards(&b.counter_shards, &[])
    })
}

fn same_buckets(left: &[SyncPlaybackStatBucket], right: &[SyncPlaybackStatBucket]) -> bool {
    if left.len() != right.len() { return false; }
    let mut left = left.iter().collect::<Vec<_>>();
    let mut right = right.iter().collect::<Vec<_>>();
    left.sort_by(|a, b| a.day_start_at.cmp(&b.day_start_at).then_with(|| a.identity_key.cmp(&b.identity_key)));
    right.sort_by(|a, b| a.day_start_at.cmp(&b.day_start_at).then_with(|| a.identity_key.cmp(&b.identity_key)));
    left.iter().zip(right).all(|(a, b)| {
        a.day_start_at == b.day_start_at
            && a.identity_key == b.identity_key
            && a.name == b.name
            && a.artist == b.artist
            && a.album == b.album
            && a.total_listen_ms == b.total_listen_ms
            && a.play_count == b.play_count
            && a.last_played_at == b.last_played_at
            && a.first_played_at == b.first_played_at
            && a.cover_url == b.cover_url
            && a.duration_ms == b.duration_ms
            && a.media_uri == b.media_uri
            && a.id == b.id
            && a.album_id == b.album_id
            && a.counter_base_listen_ms == b.counter_base_listen_ms
            && a.counter_base_play_count == b.counter_base_play_count
            && merge_counter_shards(&a.counter_shards, &[]) == merge_counter_shards(&b.counter_shards, &[])
    })
}

fn min_positive(left: i64, right: i64) -> i64 {
    match (left > 0, right > 0) {
        (false, false) => 0,
        (false, true) => right,
        (true, false) => left,
        (true, true) => left.min(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_phone_reorder_keeps_remote_added_at_values_and_order() {
        let mut local = playlist(vec![song("1", 300), song("2", 200), song("3", 100)]);
        local.modified_at = 100;
        let mut remote = playlist(vec![song("3", 900), song("1", 899), song("2", 898)]);
        remote.modified_at = 300;

        let merged = three_way_merge(
            &sync_data(vec![local]),
            &sync_data(vec![remote]),
            200,
            &HashMap::new(),
        );

        assert_eq!(
            merged.playlists[0]
                .songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["3", "1", "2"]
        );
        assert_eq!(
            merged.playlists[0]
                .songs
                .iter()
                .map(|song| song.added_at)
                .collect::<Vec<_>>(),
            vec![900, 899, 898]
        );
    }

    #[test]
    fn local_desktop_reorder_keeps_local_added_at_values_and_order() {
        let mut local = playlist(vec![song("2", 900), song("3", 899), song("1", 898)]);
        local.modified_at = 300;
        let mut remote = playlist(vec![song("1", 300), song("2", 200), song("3", 100)]);
        remote.modified_at = 100;

        let merged = three_way_merge(
            &sync_data(vec![local]),
            &sync_data(vec![remote]),
            200,
            &HashMap::new(),
        );

        assert_eq!(
            merged.playlists[0]
                .songs
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "3", "1"]
        );
        assert_eq!(
            merged.playlists[0]
                .songs
                .iter()
                .map(|song| song.added_at)
                .collect::<Vec<_>>(),
            vec![900, 899, 898]
        );
    }

    #[test]
    fn missing_created_at_does_not_replace_valid_timestamp() {
        let mut local = playlist(Vec::new());
        local.created_at = 100;
        let mut remote = local.clone();
        remote.created_at = 0;

        let merged = three_way_merge(
            &sync_data(vec![local]),
            &sync_data(vec![remote]),
            0,
            &HashMap::new(),
        );

        assert_eq!(merged.playlists[0].created_at, 100);
    }

    #[test]
    fn merge_preserves_remote_added_at_when_desktop_local_lacks_it() {
        let local_song = song("42", 0);
        let remote_song = song("42", 500);
        let base_key = remote_song.identity().stable_key();
        let base_snapshot = HashMap::from([("1".to_string(), HashSet::from([base_key]))]);
        let merged = three_way_merge(
            &sync_data(vec![playlist(vec![local_song])]),
            &sync_data(vec![playlist(vec![remote_song])]),
            100,
            &base_snapshot,
        );
        assert_eq!(merged.playlists[0].songs[0].added_at, 500);
    }

    #[test]
    fn local_deleted_song_is_not_resurrected_from_remote() {
        let deleted_song = song("42", 500);
        let base_snapshot = HashMap::from([(
            "1".to_string(),
            HashSet::from([deleted_song.identity().stable_key()]),
        )]);
        let merged = three_way_merge(
            &sync_data(vec![playlist(Vec::new())]),
            &sync_data(vec![playlist(vec![deleted_song])]),
            100,
            &base_snapshot,
        );
        assert!(merged.playlists[0].songs.is_empty());
    }

    #[test]
    fn remote_deleted_song_is_not_resurrected_from_local() {
        let deleted_song = song("42", 500);
        let base_snapshot = HashMap::from([(
            "1".to_string(),
            HashSet::from([deleted_song.identity().stable_key()]),
        )]);
        let merged = three_way_merge(
            &sync_data(vec![playlist(vec![deleted_song])]),
            &sync_data(vec![playlist(Vec::new())]),
            100,
            &base_snapshot,
        );
        assert!(merged.playlists[0].songs.is_empty());
    }

    #[test]
    fn playlist_deletion_tombstone_is_preserved() {
        let active = playlist(Vec::new());
        let mut deleted = active.clone();
        deleted.is_deleted = true;
        deleted.modified_at = 200;
        let merged = three_way_merge(
            &sync_data(vec![active]),
            &sync_data(vec![deleted]),
            100,
            &HashMap::new(),
        );
        assert!(merged.playlists[0].is_deleted);
        assert!(merged.playlists[0].songs.is_empty());
    }

    #[test]
    fn playlist_song_deletion_is_applied() {
        let target = song("42", 100);
        let deletion = SyncPlaylistSongDeletion {
            playlist_id: "1".into(),
            song_id: target.id.clone(),
            album: target.album.clone(),
            deleted_at: 200,
            device_id: "desktop".into(),
            ..Default::default()
        };
        let mut local = sync_data(vec![playlist(vec![target])]);
        local.playlist_song_deletions = vec![deletion];
        let merged = three_way_merge(&local, &SyncData::default(), 0, &HashMap::new());
        assert!(merged.playlists[0].songs.is_empty());
    }

    #[test]
    fn local_primary_does_not_clear_remote_lyrics() {
        let mut local = song("42", 500);
        local.matched_lyric = Some(String::new());
        local.sync_metadata_version = LEGACY_SYNC_METADATA_VERSION;
        let mut remote = song("42", 500);
        remote.matched_lyric = Some("[00:01.00]cloud lyric".into());
        remote.matched_translated_lyric = Some("[00:01.00]translation".into());
        remote.original_lyric = Some("original cloud lyric".into());
        remote.sync_metadata_version = LEGACY_SYNC_METADATA_VERSION;

        let merged = merge_songs(
            &[local],
            &[remote],
            300,
            200,
            true,
            true,
            100,
            false,
            &HashSet::new(),
            "1",
            &[],
        );

        assert_eq!(
            merged[0].matched_lyric.as_deref(),
            Some("[00:01.00]cloud lyric")
        );
        assert_eq!(
            merged[0].matched_translated_lyric.as_deref(),
            Some("[00:01.00]translation")
        );
        assert_eq!(
            merged[0].original_lyric.as_deref(),
            Some("original cloud lyric")
        );
        assert_eq!(
            merged[0].sync_metadata_version,
            CURRENT_SYNC_METADATA_VERSION
        );
    }

    #[test]
    fn legacy_primary_order_keeps_current_rich_metadata() {
        let mut current = song("42", 100);
        current.matched_lyric = Some("[00:01.00]lyric".into());
        current.custom_name = Some("Custom title".into());
        current.original_name = Some("Original".into());
        let mut legacy_primary = current.clone();
        legacy_primary.name = "Custom title".into();
        legacy_primary.added_at = 900;
        legacy_primary.matched_lyric = None;
        legacy_primary.custom_name = None;
        legacy_primary.original_name = None;
        legacy_primary.sync_metadata_version = LEGACY_SYNC_METADATA_VERSION;

        let merged = merge_songs(
            &[current],
            &[legacy_primary],
            100,
            300,
            false,
            true,
            200,
            false,
            &HashSet::new(),
            "1",
            &[],
        );

        assert_eq!(merged[0].added_at, 900);
        assert_eq!(merged[0].name, "Song");
        assert_eq!(merged[0].matched_lyric.as_deref(), Some("[00:01.00]lyric"));
        assert_eq!(merged[0].custom_name.as_deref(), Some("Custom title"));
        assert_eq!(merged[0].sync_metadata_version, CURRENT_SYNC_METADATA_VERSION);
    }

    #[test]
    fn current_primary_can_intentionally_clear_metadata() {
        let mut local = song("42", 100);
        local.matched_lyric = Some("[00:01.00]old lyric".into());
        local.custom_name = Some("Old custom title".into());
        let mut remote = local.clone();
        remote.added_at = 200;
        remote.matched_lyric = None;
        remote.custom_name = None;

        let merged = merge_songs(
            &[local],
            &[remote],
            100,
            300,
            false,
            true,
            200,
            false,
            &HashSet::new(),
            "1",
            &[],
        );

        assert_eq!(merged[0].added_at, 200);
        assert!(merged[0].matched_lyric.is_none());
        assert!(merged[0].custom_name.is_none());
    }

    #[test]
    fn remote_only_change_uses_remote_membership_and_payload() {
        let local_token = SyncCausalToken {
            device_id: "desktop".into(),
            counter: 1,
        };
        let remote_token = SyncCausalToken {
            device_id: "android".into(),
            counter: 1,
        };
        let mut local_match = song("42", 100);
        local_match.name = "Local metadata".into();
        local_match.sync_membership_tokens = vec![local_token.clone()];
        let mut remote_match = local_match.clone();
        remote_match.name = "Remote metadata".into();
        remote_match.sync_membership_tokens = vec![remote_token.clone()];
        let local_only = song("99", 100);

        let merged = merge_songs(
            &[local_match, local_only],
            &[remote_match],
            100,
            200,
            false,
            true,
            150,
            false,
            &HashSet::new(),
            "1",
            &[],
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "Remote metadata");
        assert_eq!(
            merged[0].sync_membership_tokens,
            vec![remote_token, local_token]
        );
    }

    #[test]
    fn bridge_aliases_collapse_the_full_membership_component() {
        let token_a = SyncCausalToken {
            device_id: "a".into(),
            counter: 1,
        };
        let token_b = SyncCausalToken {
            device_id: "b".into(),
            counter: 1,
        };
        let mut first = song("1", 100);
        first.sync_membership_tokens = vec![token_a.clone()];
        let mut second = song("2", 100);
        second.sync_membership_tokens = vec![token_b.clone()];
        let mut bridge = first.clone();
        bridge.sync_membership_tokens = vec![token_b.clone()];

        let merged = deduplicate_songs(&[first, second, bridge]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].sync_membership_tokens, vec![token_a, token_b]);
    }

    #[test]
    fn counter_shards_keep_multiple_epochs_in_stable_order() {
        let left = SyncPlaybackCounterShard { device_id: "d".into(), epoch_started_at: 1, play_count: 1, ..Default::default() };
        let right = SyncPlaybackCounterShard { device_id: "d".into(), epoch_started_at: 2, play_count: 2, ..Default::default() };
        let merged = merge_counter_shards(&[left], &[right]);
        assert_eq!(merged.iter().map(|shard| shard.epoch_started_at).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn data_change_detection_includes_song_metadata_and_deletions() {
        let remote = sync_data(vec![playlist(vec![song("42", 10_000)])]);
        let mut merged = remote.clone();
        merged.playlists[0].songs[0].name = "Changed".into();
        assert!(has_data_changed(&remote, &merged));
    }

    #[test]
    fn playlist_timestamp_only_change_is_not_data_change() {
        let remote = sync_data(vec![playlist(vec![song("42", 10_000)])]);
        let mut merged = remote.clone();
        merged.playlists[0].created_at = 999;
        merged.playlists[0].modified_at = 1_000_000;
        assert!(!has_data_changed(&remote, &merged));
    }

    #[test]
    fn favorite_display_only_fields_are_not_data_change() {
        let remote = SyncData {
            favorite_playlists: vec![SyncFavoritePlaylist {
                id: "7".into(),
                name: "Fav".into(),
                cover_url: "https://a".into(),
                track_count: 1,
                source: "netease".into(),
                songs: vec![song("42", 10)],
                added_time: 1,
                modified_at: 2,
                is_deleted: false,
                sort_order: 3,
                browse_id: Some("B".into()),
                playlist_id: Some("P".into()),
                subtitle: Some("old".into()),
            }],
            ..Default::default()
        };
        let mut merged = remote.clone();
        // 仅展示字段变化不应触发回声上传
        merged.favorite_playlists[0].name = "Fav Renamed".into();
        merged.favorite_playlists[0].cover_url = "https://b".into();
        merged.favorite_playlists[0].added_time = 99;
        merged.favorite_playlists[0].browse_id = Some("B2".into());
        merged.favorite_playlists[0].subtitle = Some("new".into());
        assert!(!has_data_changed(&remote, &merged));

        // 内容字段变化仍应检测
        merged.favorite_playlists[0].sort_order = 4;
        assert!(has_data_changed(&remote, &merged));
    }

    #[test]
    fn playlist_reordering_alone_is_not_a_data_change() {
        let first = playlist(vec![song("1", 10)]);
        let second = SyncPlaylist { id: "2".into(), ..playlist(vec![song("2", 20)]) };
        let remote = sync_data(vec![first.clone(), second.clone()]);
        let reordered = sync_data(vec![second, first]);

        assert!(!has_data_changed(&remote, &reordered));
    }

    #[test]
    fn aggregate_stats_never_fall_below_daily_bucket_totals() {
        let stat = SyncTrackStat {
            identity_key: "k".into(),
            total_listen_ms: 900,
            play_count: 876,
            last_played_at: 100,
            ..Default::default()
        };
        let buckets = vec![
            SyncPlaybackStatBucket {
                day_start_at: 0,
                identity_key: "k".into(),
                total_listen_ms: 600,
                play_count: 500,
                last_played_at: 50,
                ..Default::default()
            },
            SyncPlaybackStatBucket {
                day_start_at: MILLIS_PER_DAY,
                identity_key: "k".into(),
                total_listen_ms: 700,
                play_count: 381,
                last_played_at: 100,
                ..Default::default()
            },
        ];

        let lifted = lift_stats_to_bucket_totals(&[stat], &buckets);

        assert_eq!(lifted[0].play_count, 881);
        assert_eq!(lifted[0].total_listen_ms, 1_300);
        // 单调抬升必须幂等, 否则与对端的 max 合并会来回震荡
        let again = lift_stats_to_bucket_totals(&lifted, &buckets);
        assert_eq!(again[0].play_count, 881);
        assert_eq!(again[0].total_listen_ms, 1_300);
    }

    #[test]
    fn stat_bucket_trim_drops_out_of_window_days_and_is_idempotent() {
        let newest = 1_000 * MILLIS_PER_DAY;
        let buckets = vec![
            stat_bucket(newest, "recent"),
            stat_bucket(newest - STAT_BUCKET_RETENTION_DAYS * MILLIS_PER_DAY, "edge"),
            stat_bucket(
                newest - (STAT_BUCKET_RETENTION_DAYS + 1) * MILLIS_PER_DAY,
                "stale",
            ),
        ];

        let trimmed = trim_stat_buckets(buckets);
        let keys: Vec<&str> = trimmed.iter().map(|b| b.identity_key.as_str()).collect();

        assert!(keys.contains(&"recent"));
        assert!(keys.contains(&"edge"));
        assert!(!keys.contains(&"stale"));
        assert_eq!(trim_stat_buckets(trimmed.clone()).len(), trimmed.len());
    }

    #[test]
    fn playback_stats_trim_keeps_most_recent_entries() {
        let stats: Vec<SyncTrackStat> = (0..(MAX_PLAYBACK_STATS + 10))
            .map(|index| SyncTrackStat {
                identity_key: format!("k{index:05}"),
                last_played_at: index as i64,
                ..Default::default()
            })
            .collect();

        let trimmed = trim_playback_stats(stats);

        assert_eq!(trimmed.len(), MAX_PLAYBACK_STATS);
        assert!(trimmed.iter().all(|stat| stat.last_played_at >= 10));
        assert_eq!(trim_playback_stats(trimmed.clone()).len(), MAX_PLAYBACK_STATS);
    }

    fn stat_bucket(day_start_at: i64, identity_key: &str) -> SyncPlaybackStatBucket {
        SyncPlaybackStatBucket {
            day_start_at,
            identity_key: identity_key.into(),
            play_count: 1,
            last_played_at: day_start_at,
            ..Default::default()
        }
    }

    fn sync_data(playlists: Vec<SyncPlaylist>) -> SyncData {
        SyncData { playlists, ..Default::default() }
    }

    fn playlist(songs: Vec<SyncSong>) -> SyncPlaylist {
        SyncPlaylist {
            id: "1".into(),
            name: "Playlist".into(),
            songs,
            created_at: 1,
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
            channel_id: Some("netease".into()),
            audio_id: Some(id.into()),
            added_at,
            sync_metadata_version: CURRENT_SYNC_METADATA_VERSION,
            ..Default::default()
        }
    }
}
