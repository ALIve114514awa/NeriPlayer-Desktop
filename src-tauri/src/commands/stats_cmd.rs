// 播放统计命令：与 Android 播放统计页对齐
use tauri::{AppHandle, State};

use crate::error::AppResult;
use crate::state::AppState;
use crate::stats::{self, PlaybackSession, PlaybackStatsSummary, StatsPeriod};
use crate::sync::manager;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 前端上报的一次收听增量
///
/// 身份键由后端从 TrackInfo 派生，前端不需要重复实现各平台的哈希规则，
/// 也就不会出现两端算出不同 key 导致统计分裂。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSessionInput {
    pub track: crate::state::TrackInfo,
    #[serde(default)]
    pub listened_ms: i64,
    #[serde(default)]
    pub play_count_increment: i32,
}

fn to_session(input: &PlaybackSessionInput) -> PlaybackSession {
    let track = &input.track;
    PlaybackSession {
        identity_key: manager::playback_stats_identity_key_pub(track),
        id: track.id.clone(),
        name: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        album_id: String::new(),
        cover_url: track.cover_url.clone(),
        media_uri: None,
        duration_ms: track.duration_ms as i64,
        listened_ms: input.listened_ms,
        play_count_increment: input.play_count_increment,
    }
}

/// 上报一次收听增量（由前端 PlaybackStatsTracker 产出）
#[tauri::command]
pub async fn record_playback_session(
    session: PlaybackSessionInput,
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let device_id = manager::get_or_create_device_id_pub(&app);
    let mut store = state.stats.lock();
    store.record(&to_session(&session), &device_id, now_ms());
    stats::save(&store);
    Ok(())
}

/// 批量上报，退出前一次性 flush 用
#[tauri::command]
pub async fn record_playback_sessions(
    sessions: Vec<PlaybackSessionInput>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    let device_id = manager::get_or_create_device_id_pub(&app);
    let played_at = now_ms();
    let mut store = state.stats.lock();
    for session in &sessions {
        store.record(&to_session(session), &device_id, played_at);
    }
    stats::save(&store);
    Ok(())
}

#[tauri::command]
pub async fn get_playback_stats(
    period: StatsPeriod,
    state: State<'_, AppState>,
) -> AppResult<PlaybackStatsSummary> {
    let store = state.stats.lock();
    Ok(store.summarize(period, now_ms()))
}

/// 一次性返回全部区间，避免切换 tab 时来回 IPC
#[tauri::command]
pub async fn get_playback_stats_overview(
    state: State<'_, AppState>,
) -> AppResult<Vec<PlaybackStatsSummary>> {
    let now = now_ms();
    let store = state.stats.lock();
    Ok([
        StatsPeriod::Day,
        StatsPeriod::Week,
        StatsPeriod::Month,
        StatsPeriod::Year,
        StatsPeriod::All,
    ]
    .into_iter()
    .map(|period| store.summarize(period, now))
    .collect())
}

#[tauri::command]
pub async fn clear_playback_stats(state: State<'_, AppState>) -> AppResult<()> {
    let mut store = state.stats.lock();
    store.clear(now_ms());
    stats::save(&store);
    Ok(())
}

#[tauri::command]
pub async fn remove_playback_stats(
    identity_keys: Vec<String>,
    state: State<'_, AppState>,
) -> AppResult<()> {
    let mut store = state.stats.lock();
    store.remove_tracks(&identity_keys);
    stats::save(&store);
    Ok(())
}

/// 计算曲目的统计身份键，供前端上报时使用
#[tauri::command]
pub async fn playback_stats_identity_key(
    track: crate::state::TrackInfo,
) -> AppResult<String> {
    Ok(manager::playback_stats_identity_key_pub(&track))
}
