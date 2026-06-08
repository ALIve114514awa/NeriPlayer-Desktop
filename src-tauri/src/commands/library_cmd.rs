use serde::Serialize;
use tauri::{AppHandle, Emitter};
use crate::error::{AppError, AppResult};
use crate::state::TrackInfo;
use crate::library::{scanner, playlist::PlaylistStore};
use crate::sync::models::SyncFavoritePlaylist;

#[tauri::command]
pub async fn scan_music_directory(
    dir: String,
    name_template: Option<String>,
) -> AppResult<Vec<TrackInfo>> {
    tokio::task::spawn_blocking(move || scanner::scan_directory(&dir, name_template.as_deref()))
        .await
        .map_err(|e| AppError::Other(e.to_string()))?
}

// 播放列表路径
fn playlists_path() -> std::path::PathBuf {
    let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    path.push("NeriPlayer");
    path.push("playlists.json");
    path
}

#[derive(Serialize)]
pub struct PlaylistInfo {
    pub id: i64,
    pub name: String,
    pub track_count: usize,
    pub modified_at: u64,
    pub cover_url: Option<String>,
}

#[tauri::command]
pub async fn list_playlists() -> AppResult<Vec<PlaylistInfo>> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);

    // 自动清理重复歌单（同名只保留歌曲最多的）
    let mut needs_save = false;
    let mut seen_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut to_remove = Vec::new();
    for (i, pl) in store.playlists.iter().enumerate() {
        let name = pl.name.trim().to_string();
        if let Some(&prev_idx) = seen_names.get(&name) {
            if pl.tracks.len() > store.playlists[prev_idx].tracks.len() {
                to_remove.push(prev_idx);
                seen_names.insert(name, i);
            } else {
                to_remove.push(i);
            }
            needs_save = true;
        } else {
            seen_names.insert(name, i);
        }
    }
    if needs_save {
        to_remove.sort_unstable();
        to_remove.dedup();
        for &idx in to_remove.iter().rev() {
            store.playlists.remove(idx);
        }
        let _ = store.save(&path);
    }

    let mut list: Vec<PlaylistInfo> = store.playlists.iter().map(|p| {
        let cover = p.tracks.iter().rev().find_map(|t| {
            t.cover_url
                .as_ref()
                .filter(|url| !url.trim().is_empty())
                .cloned()
        });
        let mut seen = std::collections::HashSet::new();
        let unique_count = p.tracks.iter()
            .filter(|t| !t.id.is_empty() && seen.insert(t.id.clone()))
            .count();
        PlaylistInfo {
            id: p.id,
            name: p.name.clone(),
            track_count: unique_count,
            modified_at: p.modified_at,
            cover_url: cover,
        }
    }).collect();
    list.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(list)
}

#[tauri::command]
pub async fn create_playlist(app: AppHandle, name: String) -> AppResult<PlaylistInfo> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let pl = store.create(name);
    let info = PlaylistInfo {
        id: pl.id,
        name: pl.name.clone(),
        track_count: 0,
        modified_at: pl.modified_at,
        cover_url: None,
    };
    store.save(&path)?;
    let _ = app.emit("playlists-changed", ());
    Ok(info)
}

#[tauri::command]
pub async fn delete_playlist(app: AppHandle, id: i64) -> AppResult<bool> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let deleted = store.delete(id);
    if deleted {
        store.save(&path)?;
        let _ = app.emit("playlists-changed", ());
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn rename_playlist(app: AppHandle, id: i64, name: String) -> AppResult<bool> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    if let Some(pl) = store.playlists.iter_mut().find(|p| p.id == id) {
        pl.name = name;
        pl.modified_at = chrono::Utc::now().timestamp_millis() as u64;
        store.save(&path)?;
        let _ = app.emit("playlists-changed", ());
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub async fn get_playlist_tracks(id: i64) -> AppResult<Vec<TrackInfo>> {
    let path = playlists_path();
    let store = PlaylistStore::load(&path);
    let pl = store.playlists.iter().find(|p| p.id == id)
        .ok_or_else(|| AppError::NotFound("Playlist not found".into()))?;
    let mut seen = std::collections::HashSet::new();
    let tracks: Vec<TrackInfo> = pl.tracks.iter()
        .filter(|t| !t.id.is_empty() && seen.insert(t.id.clone()))
        .cloned()
        .collect();
    Ok(tracks)
}

#[tauri::command]
pub async fn add_to_playlist(app: AppHandle, playlist_id: i64, track: TrackInfo) -> AppResult<()> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let pl = store.playlists.iter_mut().find(|p| p.id == playlist_id)
        .ok_or_else(|| AppError::NotFound("Playlist not found".into()))?;

    if !pl.tracks.iter().any(|t| t.id == track.id) {
        pl.tracks.push(track);
        pl.modified_at = chrono::Utc::now().timestamp_millis() as u64;
    }
    store.save(&path)?;
    let _ = app.emit("playlists-changed", ());
    Ok(())
}


#[tauri::command]
pub async fn add_tracks_to_playlist(app: AppHandle, playlist_id: i64, tracks: Vec<TrackInfo>) -> AppResult<usize> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let pl = store.playlists.iter_mut().find(|p| p.id == playlist_id)
        .ok_or_else(|| AppError::NotFound("Playlist not found".into()))?;

    let mut added = 0_usize;
    for track in tracks {
        if track.id.is_empty() || pl.tracks.iter().any(|t| t.id == track.id) {
            continue;
        }
        pl.tracks.push(track);
        added += 1;
    }

    if added > 0 {
        pl.modified_at = chrono::Utc::now().timestamp_millis() as u64;
        store.save(&path)?;
        let _ = app.emit("playlists-changed", ());
    }
    Ok(added)
}

#[tauri::command]
pub async fn remove_from_playlist(app: AppHandle, playlist_id: i64, track_id: String) -> AppResult<()> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let pl = store.playlists.iter_mut().find(|p| p.id == playlist_id)
        .ok_or_else(|| AppError::NotFound("Playlist not found".into()))?;
    pl.tracks.retain(|t| t.id != track_id);
    pl.modified_at = chrono::Utc::now().timestamp_millis() as u64;
    store.save(&path)?;
    let _ = app.emit("playlists-changed", ());
    Ok(())
}


#[tauri::command]
pub async fn remove_tracks_from_playlist(app: AppHandle, playlist_id: i64, track_ids: Vec<String>) -> AppResult<usize> {
    let path = playlists_path();
    let mut store = PlaylistStore::load(&path);
    let pl = store.playlists.iter_mut().find(|p| p.id == playlist_id)
        .ok_or_else(|| AppError::NotFound("Playlist not found".into()))?;

    let ids: std::collections::HashSet<String> = track_ids.into_iter().collect();
    if ids.is_empty() {
        return Ok(0);
    }

    let before = pl.tracks.len();
    pl.tracks.retain(|t| !ids.contains(&t.id));
    let removed = before.saturating_sub(pl.tracks.len());

    if removed > 0 {
        pl.modified_at = chrono::Utc::now().timestamp_millis() as u64;
        store.save(&path)?;
        let _ = app.emit("playlists-changed", ());
    }
    Ok(removed)
}

/// 获取收藏歌单列表
#[tauri::command]
pub async fn list_favorite_playlists() -> AppResult<Vec<SyncFavoritePlaylist>> {
    Ok(crate::sync::manager::load_favorite_playlists())
}
