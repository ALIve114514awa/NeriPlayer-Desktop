use crate::error::AppResult;
use crate::lyrics::manager::LyricsManager;
use crate::lyrics::parser::{self, LyricLine};
use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub async fn parse_lrc_content(content: String) -> AppResult<Vec<LyricLine>> {
    Ok(parser::parse_lrc(&content))
}

#[tauri::command]
pub async fn load_lyrics_file(path: String) -> AppResult<Vec<LyricLine>> {
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| crate::error::AppError::Other(format!("Read lyrics: {}", e)))?;
    Ok(parser::parse_lrc(&content))
}

/// 多源歌词获取
#[tauri::command]
pub async fn fetch_lyrics(
    title: String,
    artist: String,
    duration_secs: u64,
    audio_path: Option<String>,
    netease_id: Option<u64>,
    qq_song_mid: Option<String>,
    state: State<'_, AppState>,
) -> AppResult<Vec<LyricLine>> {
    let manager = LyricsManager::new(&state.http());
    manager
        .fetch_lyrics(
            &title,
            &artist,
            duration_secs,
            audio_path.as_deref(),
            netease_id,
            qq_song_mid.as_deref(),
        )
        .await
}
