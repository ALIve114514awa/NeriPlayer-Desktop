use serde::Serialize;
use tauri::State;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Serialize)]
pub struct PlayerStateDto {
    pub is_playing: bool,
    pub volume: f32,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub current_track: Option<crate::state::TrackInfo>,
    pub repeat_mode: crate::state::RepeatMode,
    pub shuffle: bool,
}

#[tauri::command]
pub async fn play_file(path: String, state: State<'_, AppState>) -> AppResult<u64> {
    let mut player = state.player.lock();
    player.play_file(&path)
}

/// 从 URL 下载音频并播放（网易云 / B站 / YouTube 流式播放）
#[tauri::command]
pub async fn play_url(url: String, duration_hint_ms: u64, state: State<'_, AppState>) -> AppResult<u64> {
    eprintln!("[play_url] start: url_len={}, hint={}ms", url.len(), duration_hint_ms);

    // 根据 URL 域名动态设置 Referer
    let referer = if url.contains("bilibili.com") || url.contains("bilivideo.") {
        "https://www.bilibili.com"
    } else if url.contains("youtube.com") || url.contains("googlevideo.com") {
        "https://music.youtube.com"
    } else {
        "https://music.163.com"
    };

    let resp = state.http().get(&url)
        .header("Referer", referer)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .send().await
        .map_err(|e| {
            eprintln!("[play_url] HTTP send error: {}", e);
            AppError::Network(e)
        })?;

    let status = resp.status();
    eprintln!("[play_url] HTTP status: {}", status);

    if !status.is_success() {
        return Err(AppError::Api(format!("HTTP {}: stream fetch failed", status)));
    }

    let bytes = resp.bytes().await
        .map_err(|e| {
            eprintln!("[play_url] body read error: {}", e);
            AppError::Network(e)
        })?;

    eprintln!("[play_url] downloaded {} bytes", bytes.len());

    if bytes.is_empty() {
        return Err(AppError::Audio("Empty audio data received".into()));
    }

    let data = bytes.to_vec();
    let mut player = state.player.lock();
    player.play_bytes(data, duration_hint_ms)
}

#[tauri::command]
pub async fn pause(state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().pause();
    Ok(())
}

#[tauri::command]
pub async fn resume(state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().resume();
    Ok(())
}

#[tauri::command]
pub async fn toggle_play_pause(state: State<'_, AppState>) -> AppResult<bool> {
    let mut player = state.player.lock();
    if player.is_playing {
        player.pause();
    } else {
        player.resume();
    }
    Ok(player.is_playing)
}

#[tauri::command]
pub async fn set_volume(level: f32, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().set_volume(level);
    Ok(())
}

#[tauri::command]
pub async fn seek(position_ms: u64, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().seek_to(position_ms)
}

#[tauri::command]
pub async fn stop(state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().stop();
    Ok(())
}

#[tauri::command]
pub async fn set_speed(speed: f32, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().set_speed(speed);
    Ok(())
}

#[tauri::command]
pub async fn set_loudness_gain(gain_mb: i32, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().set_loudness_gain(gain_mb);
    Ok(())
}

#[tauri::command]
pub async fn set_equalizer(enabled: bool, band_levels_mb: Vec<i32>, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().set_equalizer(enabled, &band_levels_mb);
    Ok(())
}

#[tauri::command]
pub async fn reset_audio_effects(state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().reset_effects();
    Ok(())
}

#[tauri::command]
pub async fn pause_with_fade(duration_ms: u32, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().pause_with_fade(duration_ms);
    Ok(())
}

#[tauri::command]
pub async fn resume_with_fade(duration_ms: u32, state: State<'_, AppState>) -> AppResult<()> {
    state.player.lock().resume_with_fade(duration_ms);
    Ok(())
}

#[tauri::command]
pub async fn crossfade_url(
    url: String,
    duration_hint_ms: u64,
    fade_out_ms: u32,
    fade_in_ms: u32,
    state: State<'_, AppState>,
) -> AppResult<u64> {
    // 根据 URL 域名动态设置 Referer
    let referer = if url.contains("bilibili.com") || url.contains("bilivideo.") {
        "https://www.bilibili.com"
    } else if url.contains("youtube.com") || url.contains("googlevideo.com") {
        "https://music.youtube.com"
    } else {
        "https://music.163.com"
    };

    let resp = state.http().get(&url)
        .header("Referer", referer)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .send().await
        .map_err(|e| AppError::Network(e))?;

    if !resp.status().is_success() {
        return Err(AppError::Api(format!("HTTP {}: stream fetch failed", resp.status())));
    }

    let bytes = resp.bytes().await
        .map_err(|e| AppError::Network(e))?;

    if bytes.is_empty() {
        return Err(AppError::Audio("Empty audio data received".into()));
    }

    let data = bytes.to_vec();
    let mut player = state.player.lock();
    player.crossfade_bytes(data, duration_hint_ms, fade_out_ms, fade_in_ms)
}

#[tauri::command]
pub async fn crossfade_file(
    path: String,
    fade_out_ms: u32,
    fade_in_ms: u32,
    state: State<'_, AppState>,
) -> AppResult<u64> {
    let mut player = state.player.lock();
    player.crossfade_file(&path, fade_out_ms, fade_in_ms)
}

#[tauri::command]
pub async fn get_player_state(state: State<'_, AppState>) -> AppResult<PlayerStateDto> {
    let player = state.player.lock();
    let queue = state.queue.lock();
    Ok(PlayerStateDto {
        is_playing: player.is_playing,
        volume: player.volume,
        position_ms: player.position_ms(),
        duration_ms: player.duration_ms,
        current_track: queue.current().cloned(),
        repeat_mode: queue.repeat_mode,
        shuffle: queue.shuffle,
    })
}

#[tauri::command]
pub async fn next_track(state: State<'_, AppState>) -> AppResult<Option<crate::state::TrackInfo>> {
    let mut queue = state.queue.lock();
    let track = queue.next().cloned();
    if let Some(ref t) = track {
        let mut player = state.player.lock();
        player.play_file(&t.url)?;
    }
    Ok(track)
}

#[tauri::command]
pub async fn prev_track(state: State<'_, AppState>) -> AppResult<Option<crate::state::TrackInfo>> {
    let mut queue = state.queue.lock();
    let track = queue.prev().cloned();
    if let Some(ref t) = track {
        let mut player = state.player.lock();
        player.play_file(&t.url)?;
    }
    Ok(track)
}

#[tauri::command]
pub async fn set_queue(tracks: Vec<crate::state::TrackInfo>, start_index: usize, state: State<'_, AppState>) -> AppResult<()> {
    let mut queue = state.queue.lock();
    queue.set_tracks(tracks, start_index);
    if let Some(track) = queue.current().cloned() {
        let mut player = state.player.lock();
        player.play_file(&track.url)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn toggle_shuffle(state: State<'_, AppState>) -> AppResult<bool> {
    let mut queue = state.queue.lock();
    queue.toggle_shuffle();
    Ok(queue.shuffle)
}

#[tauri::command]
pub async fn cycle_repeat(state: State<'_, AppState>) -> AppResult<crate::state::RepeatMode> {
    let mut queue = state.queue.lock();
    Ok(queue.cycle_repeat())
}
