// 同步相关命令
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::sync::models::*;
use crate::sync::manager;
use tauri_plugin_store::StoreExt;

// 同步配置存储键
const GITHUB_CONFIG_KEY: &str = "githubSync";
const WEBDAV_CONFIG_KEY: &str = "webdavSync";
const SYNC_STORE: &str = "sync-config.json";

fn load_github_config(app: &AppHandle) -> GitHubSyncConfig {
    app.store(SYNC_STORE).ok()
        .and_then(|s| s.get(GITHUB_CONFIG_KEY))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn save_github_config(app: &AppHandle, config: &GitHubSyncConfig) {
    if let Ok(s) = app.store(SYNC_STORE) {
        let _ = s.set(GITHUB_CONFIG_KEY, serde_json::to_value(config).unwrap_or_default());
    }
}

fn load_webdav_config(app: &AppHandle) -> WebDavSyncConfig {
    app.store(SYNC_STORE).ok()
        .and_then(|s| s.get(WEBDAV_CONFIG_KEY))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn save_webdav_config(app: &AppHandle, config: &WebDavSyncConfig) {
    if let Ok(s) = app.store(SYNC_STORE) {
        let _ = s.set(WEBDAV_CONFIG_KEY, serde_json::to_value(config).unwrap_or_default());
    }
}

/// 获取 GitHub 同步配置（不含 token 明文）
#[tauri::command]
pub async fn get_github_sync_config(app: AppHandle) -> AppResult<Value> {
    let config = load_github_config(&app);
    Ok(serde_json::json!({
        "configured": !config.token.is_empty(),
        "owner": config.owner,
        "repo": config.repo,
        "autoSync": config.auto_sync,
        "lastSyncTime": config.last_sync_time,
        "dataSaver": config.data_saver,
        "silentFailures": config.silent_failures,
        "historyUpdateMode": config.history_update_mode,
    }))
}

/// Phase 1: 验证 GitHub token，返回用户名
#[tauri::command]
pub async fn validate_github_token(
    app: AppHandle,
    state: State<'_, AppState>,
    token: String,
) -> AppResult<Value> {
    let api = crate::sync::github_api::GitHubApiClient::new(&state.http(), &token);
    let username = api.validate_token().await?;

    // 暂存 token（还没配置完，只保存 token 和 owner）
    let mut config = load_github_config(&app);
    config.token = token;
    config.owner = username.clone();
    save_github_config(&app, &config);

    Ok(serde_json::json!({
        "success": true,
        "username": username,
    }))
}

/// Phase 2: 创建新仓库
#[tauri::command]
pub async fn create_github_repo(
    app: AppHandle,
    state: State<'_, AppState>,
    repo_name: String,
) -> AppResult<Value> {
    let config = load_github_config(&app);
    if config.token.is_empty() {
        return Err(AppError::Api("Token not validated yet".into()));
    }

    let api = crate::sync::github_api::GitHubApiClient::new(&state.http(), &config.token);
    api.create_repository(&repo_name).await?;

    let updated = GitHubSyncConfig {
        token: config.token,
        owner: config.owner.clone(),
        repo: repo_name.clone(),
        auto_sync: true,
        data_saver: true,  // 默认开启省流
        ..Default::default()
    };
    save_github_config(&app, &updated);

    Ok(serde_json::json!({
        "success": true,
        "owner": config.owner,
        "repo": repo_name,
    }))
}

/// Phase 2 (alternative): 使用已有仓库
#[tauri::command]
pub async fn use_existing_github_repo(
    app: AppHandle,
    state: State<'_, AppState>,
    owner: String,
    repo: String,
) -> AppResult<Value> {
    let config = load_github_config(&app);
    if config.token.is_empty() {
        return Err(AppError::Api("Token not validated yet".into()));
    }

    let api = crate::sync::github_api::GitHubApiClient::new(&state.http(), &config.token);
    let _branch = api.check_repository(&owner, &repo).await?;

    let updated = GitHubSyncConfig {
        token: config.token,
        owner: owner.clone(),
        repo: repo.clone(),
        auto_sync: true,
        data_saver: true,  // 默认开启省流
        ..Default::default()
    };
    save_github_config(&app, &updated);

    Ok(serde_json::json!({
        "success": true,
        "owner": owner,
        "repo": repo,
    }))
}

/// 配置 GitHub 同步（保留兼容，内部调用两阶段）
#[tauri::command]
pub async fn configure_github_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    token: String,
    repo: String,
) -> AppResult<Value> {
    let api = crate::sync::github_api::GitHubApiClient::new(&state.http(), &token);
    let owner = api.validate_token().await?;

    if let Err(_) = api.check_repository(&owner, &repo).await {
        api.create_repository(&repo).await?;
    }

    let config = GitHubSyncConfig {
        token,
        owner: owner.clone(),
        repo: repo.clone(),
        auto_sync: true,
        data_saver: true,  // 默认开启省流
        ..Default::default()
    };
    save_github_config(&app, &config);

    Ok(serde_json::json!({
        "success": true,
        "owner": owner,
        "repo": repo,
    }))
}

/// 执行 GitHub 同步
#[tauri::command]
pub async fn sync_github(app: AppHandle, state: State<'_, AppState>) -> AppResult<SyncResult> {
    let mut config = load_github_config(&app);
    if config.token.is_empty() {
        return Err(AppError::Api("GitHub sync not configured".into()));
    }

    let local_data = manager::build_local_sync_data(&app);
    let result = manager::sync_github(&state.http(), &mut config, &local_data).await?;
    save_github_config(&app, &config);
    // 通知前端歌单数据可能已变更
    let _ = app.emit("playlists-changed", ());
    Ok(result)
}

/// 断开 GitHub 同步
#[tauri::command]
pub async fn disconnect_github_sync(app: AppHandle) -> AppResult<()> {
    save_github_config(&app, &GitHubSyncConfig::default());
    Ok(())
}

/// 获取 WebDAV 同步配置
#[tauri::command]
pub async fn get_webdav_sync_config(app: AppHandle) -> AppResult<Value> {
    let config = load_webdav_config(&app);
    Ok(serde_json::json!({
        "configured": !config.server_url.is_empty(),
        "serverUrl": config.server_url,
        "basePath": config.base_path,
        "autoSync": config.auto_sync,
        "lastSyncTime": config.last_sync_time,
    }))
}

/// 配置 WebDAV 同步
#[tauri::command]
pub async fn configure_webdav_sync(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    username: String,
    password: String,
    base_path: Option<String>,
) -> AppResult<Value> {
    let bp = base_path.unwrap_or_default();
    let api = crate::sync::webdav_api::WebDavApiClient::new(
        &state.http(), &server_url, &username, &password, &bp,
    );
    api.validate_connection().await?;

    let config = WebDavSyncConfig {
        server_url: server_url.clone(),
        username,
        password,
        base_path: bp,
        auto_sync: true,
        ..Default::default()
    };
    save_webdav_config(&app, &config);

    Ok(serde_json::json!({
        "success": true,
        "serverUrl": server_url,
    }))
}

/// 执行 WebDAV 同步
#[tauri::command]
pub async fn sync_webdav(app: AppHandle, state: State<'_, AppState>) -> AppResult<SyncResult> {
    let mut config = load_webdav_config(&app);
    if config.server_url.is_empty() {
        return Err(AppError::Api("WebDAV sync not configured".into()));
    }

    let local_data = manager::build_local_sync_data(&app);
    let result = manager::sync_webdav(&state.http(), &mut config, &local_data).await?;
    save_webdav_config(&app, &config);
    let _ = app.emit("playlists-changed", ());
    Ok(result)
}

/// 更新 GitHub 同步子设置（不影响 token/owner/repo）
#[tauri::command]
pub async fn update_github_sync_settings(
    app: AppHandle,
    auto_sync: Option<bool>,
    data_saver: Option<bool>,
    silent_failures: Option<bool>,
    history_update_mode: Option<String>,
) -> AppResult<()> {
    let mut config = load_github_config(&app);
    if config.token.is_empty() {
        return Err(AppError::Api("GitHub sync not configured".into()));
    }
    if let Some(v) = auto_sync { config.auto_sync = v; }
    if let Some(v) = data_saver { config.data_saver = v; }
    if let Some(v) = silent_failures { config.silent_failures = v; }
    if let Some(v) = history_update_mode { config.history_update_mode = v; }
    save_github_config(&app, &config);
    Ok(())
}

/// 更新 WebDAV 同步子设置
#[tauri::command]
pub async fn update_webdav_sync_settings(
    app: AppHandle,
    auto_sync: Option<bool>,
) -> AppResult<()> {
    let mut config = load_webdav_config(&app);
    if config.server_url.is_empty() {
        return Err(AppError::Api("WebDAV sync not configured".into()));
    }
    if let Some(v) = auto_sync { config.auto_sync = v; }
    save_webdav_config(&app, &config);
    Ok(())
}

/// 清除应用缓存（音频/图片缓存目录 + app_data_dir 下的临时缓存子目录）
#[tauri::command]
pub async fn clear_app_cache(app: AppHandle) -> AppResult<Value> {
    let mut cleared: u64 = 0;
    let mut failed: u64 = 0;

    // 1. 清理 app_cache_dir
    if let Ok(cache_dir) = app.path().app_cache_dir() {
        let (c, f) = clear_directory_contents(&cache_dir);
        cleared += c;
        failed += f;
    }

    // 2. 清理 app_data_dir 下的缓存子目录（covers, temp 等）
    if let Ok(data_dir) = app.path().app_data_dir() {
        for sub in &["covers", "temp", "cache", "thumbnails"] {
            let sub_dir = data_dir.join(sub);
            if sub_dir.exists() && sub_dir.is_dir() {
                let (c, f) = clear_directory_contents(&sub_dir);
                cleared += c;
                failed += f;
            }
        }
    }

    log::info!("clear_app_cache: cleared {} bytes, {} failures", cleared, failed);
    Ok(serde_json::json!({ "clearedBytes": cleared, "failedCount": failed }))
}

/// 清除目录下所有内容，返回 (cleared_bytes, failed_count)
fn clear_directory_contents(dir: &std::path::Path) -> (u64, u64) {
    let mut cleared: u64 = 0;
    let mut failed: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if std::fs::remove_file(&path).is_ok() {
                    cleared += size;
                } else {
                    failed += 1;
                }
            } else if path.is_dir() {
                if let Ok(size) = dir_size(&path) { cleared += size; }
                if std::fs::remove_dir_all(&path).is_err() {
                    failed += 1;
                }
            }
        }
    }
    (cleared, failed)
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            total += dir_size(&entry.path())?;
        }
    }
    Ok(total)
}

/// 导出播放列表为 JSON（Android BackupData 兼容格式）
#[tauri::command]
pub async fn export_playlists(app: AppHandle) -> AppResult<Value> {
    use crate::library::playlist::PlaylistStore;
    let playlists_path = {
        let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        path.push("NeriPlayer");
        path.push("playlists.json");
        path
    };
    let store = PlaylistStore::load(&playlists_path);

    // 转换为 SyncPlaylist 格式（Android 兼容）
    let sync_playlists: Vec<crate::sync::models::SyncPlaylist> = store.playlists.iter().map(|pl| {
        crate::sync::models::SyncPlaylist {
            id: pl.id.to_string(),
            name: pl.name.clone(),
            songs: pl.tracks.iter().map(|t| crate::sync::manager::track_to_sync_song_pub(t)).collect(),
            created_at: pl.modified_at as i64,
            modified_at: pl.modified_at as i64,
            is_deleted: false,
        }
    }).collect();

    let backup_data = serde_json::json!({
        "version": "2.0",
        "timestamp": chrono::Utc::now().timestamp_millis(),
        "exportDate": chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string(),
        "playlists": sync_playlists,
    });

    let json_data = serde_json::to_string_pretty(&backup_data)
        .map_err(|e| AppError::Other(format!("Serialize failed: {}", e)))?;

    use tauri_plugin_dialog::DialogExt;
    let path = app.dialog().file()
        .set_file_name("neriplayer-playlists.json")
        .add_filter("JSON", &["json"])
        .blocking_save_file();

    match path {
        Some(p) => {
            std::fs::write(p.as_path().unwrap(), &json_data)
                .map_err(|e| AppError::Other(format!("Write failed: {}", e)))?;
            Ok(serde_json::json!({ "success": true, "count": store.playlists.len() }))
        }
        None => Ok(serde_json::json!({ "success": false, "reason": "cancelled" })),
    }
}

/// 导入播放列表 JSON（兼容 Android BackupData 和 Desktop 两种格式）
#[tauri::command]
pub async fn import_playlists(app: AppHandle) -> AppResult<Value> {
    use crate::library::playlist::{PlaylistStore, Playlist};
    use crate::sync::models::{SyncPlaylist, SyncData};
    use crate::sync::manager::save_synced_playlists;
    use tauri_plugin_dialog::DialogExt;

    let path = app.dialog().file()
        .add_filter("JSON", &["json"])
        .blocking_pick_file();

    match path {
        Some(p) => {
            let data = std::fs::read_to_string(p.as_path().unwrap())
                .map_err(|e| AppError::Other(format!("Read failed: {}", e)))?;

            let parsed: serde_json::Value = serde_json::from_str(&data)
                .map_err(|e| AppError::Other(format!("Parse failed: {}", e)))?;

            // 检测格式：Android BackupData 有 "playlists" 顶层数组（每项有 "songs"）
            //           Desktop 格式是直接的 Playlist 数组（每项有 "tracks"）
            let count;

            if parsed.is_object() && parsed.get("playlists").is_some() {
                // Android BackupData 格式：{ version, playlists: [SyncPlaylist] }
                let sync_playlists: Vec<SyncPlaylist> = serde_json::from_value(
                    parsed["playlists"].clone()
                ).map_err(|e| AppError::Other(format!("Parse Android playlists: {}", e)))?;
                count = sync_playlists.len();

                // 通过 save_synced_playlists 转换并回写（复用已有的去重 + 转换逻辑）
                let sync_data = SyncData {
                    playlists: sync_playlists,
                    ..Default::default()
                };
                save_synced_playlists(&sync_data);
            } else if parsed.is_array() {
                // 尝试 Desktop 格式
                if let Ok(imported) = serde_json::from_value::<Vec<Playlist>>(parsed.clone()) {
                    count = imported.len();
                    let playlists_path = {
                        let mut path = dirs_next::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                        path.push("NeriPlayer");
                        path.push("playlists.json");
                        path
                    };
                    let mut store = PlaylistStore::load(&playlists_path);
                    for pl in imported {
                        if !store.playlists.iter().any(|p| p.name == pl.name) {
                            store.playlists.push(pl);
                        }
                    }
                    store.save(&playlists_path)?;
                } else {
                    // 可能是 SyncPlaylist 数组（无外层包装）
                    let sync_playlists: Vec<SyncPlaylist> = serde_json::from_value(parsed)
                        .map_err(|e| AppError::Other(format!("Parse playlists array: {}", e)))?;
                    count = sync_playlists.len();
                    let sync_data = SyncData {
                        playlists: sync_playlists,
                        ..Default::default()
                    };
                    save_synced_playlists(&sync_data);
                }
            } else {
                return Err(AppError::Other("Unrecognized playlist format".into()));
            }

            let _ = app.emit("playlists-changed", ());
            Ok(serde_json::json!({ "success": true, "imported": count }))
        }
        None => Ok(serde_json::json!({ "success": false, "reason": "cancelled" })),
    }
}

/// 断开 WebDAV 同步
#[tauri::command]
pub async fn disconnect_webdav_sync(app: AppHandle) -> AppResult<()> {
    save_webdav_config(&app, &WebDavSyncConfig::default());
    Ok(())
}
