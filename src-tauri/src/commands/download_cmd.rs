use crate::error::{AppError, AppResult};
use crate::state::AppState;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub cover_url: Option<String>,
    pub source: String,
    pub file_path: String,
    pub file_size: u64,
    pub downloaded_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadManifestValidation {
    pub tracks: Vec<DownloadedTrack>,
    pub removed_count: usize,
}

/// 清理文件名中的非法字符
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 根据模板渲染下载文件名（不含扩展名）
/// 支持占位符：{title}, {artist}, {album}, {source}
fn render_download_filename(
    title: &str,
    artist: &str,
    album: &str,
    source: &str,
    template: Option<&str>,
) -> String {
    let default_template = "{artist} - {title}";
    let tpl = template
        .filter(|t| !t.is_empty())
        .unwrap_or(default_template);
    let rendered = tpl
        .replace("{title}", title)
        .replace("{artist}", artist)
        .replace("{album}", album)
        .replace("{source}", source);
    let sanitized = sanitize_filename(&rendered);
    if sanitized.is_empty() {
        sanitize_filename(title)
    } else {
        sanitized
    }
}
fn ext_from_content_type(content_type: &str) -> &str {
    if content_type.contains("mp4") || content_type.contains("m4a") || content_type.contains("aac")
    {
        "m4a"
    } else if content_type.contains("ogg") || content_type.contains("opus") {
        "ogg"
    } else if content_type.contains("webm") {
        "webm"
    } else if content_type.contains("flac") {
        "flac"
    } else if content_type.contains("wav") {
        "wav"
    } else {
        "mp3"
    }
}

/// 获取下载目录，不存在则创建
/// 如果提供了 custom_dir 且有效，则使用自定义目录；否则 fallback 到默认目录
fn downloads_dir(app: &AppHandle, custom_dir: Option<&str>) -> AppResult<PathBuf> {
    let dir = if let Some(cd) = custom_dir {
        if !cd.is_empty() {
            let p = PathBuf::from(cd);
            // 尝试创建目录（如果不存在）
            if !p.exists() {
                if std::fs::create_dir_all(&p).is_ok() {
                    return Ok(p);
                }
                // 创建失败，fallback 到默认
            } else {
                return Ok(p);
            }
        }
        // 空字符串或无效路径，fallback
        default_downloads_dir(app)?
    } else {
        default_downloads_dir(app)?
    };
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| AppError::Io(e))?;
    }
    Ok(dir)
}

/// 默认下载目录
fn default_downloads_dir(app: &AppHandle) -> AppResult<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Other(e.to_string()))?
        .join("downloads");
    Ok(dir)
}

/// 验证并设置下载目录
#[tauri::command]
pub async fn set_download_dir(path: String) -> AppResult<String> {
    let p = PathBuf::from(&path);
    // 确保目录存在
    if !p.exists() {
        std::fs::create_dir_all(&p)
            .map_err(|e| AppError::Other(format!("Cannot create directory: {}", e)))?;
    }
    // 验证可写：创建临时文件再删除
    let test_file = p.join(".neri_write_test");
    std::fs::write(&test_file, b"test")
        .map_err(|_| AppError::Other("Directory is not writable".into()))?;
    let _ = std::fs::remove_file(&test_file);
    // 返回规范化路径
    let canonical = p.canonicalize().unwrap_or(p).to_string_lossy().to_string();
    // 移除 Windows UNC 前缀 \\?\
    let canonical = canonical
        .strip_prefix(r"\\?\")
        .unwrap_or(&canonical)
        .to_string();
    Ok(canonical)
}

/// 获取默认下载目录路径
#[tauri::command]
pub async fn get_default_download_dir(app: AppHandle) -> AppResult<String> {
    let dir = default_downloads_dir(&app)?;
    Ok(dir.to_string_lossy().to_string())
}

/// manifest.json 路径（始终存储在默认下载目录，与自定义目录无关）
fn manifest_path(app: &AppHandle) -> AppResult<PathBuf> {
    let dir = default_downloads_dir(app)?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| AppError::Io(e))?;
    }
    Ok(dir.join("manifest.json"))
}

/// 读取 manifest
fn read_manifest(app: &AppHandle) -> AppResult<Vec<DownloadedTrack>> {
    let path = manifest_path(app)?;
    if !path.exists() {
        return Ok(vec![]);
    }
    let data = std::fs::read_to_string(&path)?;
    let tracks: Vec<DownloadedTrack> = serde_json::from_str(&data).unwrap_or_default();
    Ok(tracks)
}

/// 写入 manifest
fn write_manifest(app: &AppHandle, tracks: &[DownloadedTrack]) -> AppResult<()> {
    let path = manifest_path(app)?;
    let json = serde_json::to_string_pretty(tracks)?;
    std::fs::write(&path, json)?;
    Ok(())
}

fn validate_manifest_files(app: &AppHandle) -> AppResult<DownloadManifestValidation> {
    let manifest = read_manifest(app)?;
    let mut valid = Vec::with_capacity(manifest.len());
    let mut removed_count = 0_usize;
    let mut changed = false;

    for mut track in manifest {
        let path = std::path::Path::new(&track.file_path);
        if path.is_file() {
            if let Ok(meta) = std::fs::metadata(path) {
                let actual_size = meta.len();
                if actual_size > 0 && actual_size != track.file_size {
                    track.file_size = actual_size;
                    changed = true;
                }
            }
            valid.push(track);
        } else {
            removed_count += 1;
            changed = true;
        }
    }

    if changed {
        write_manifest(app, &valid)?;
    }

    Ok(DownloadManifestValidation {
        tracks: valid,
        removed_count,
    })
}

fn emit_download_progress(
    app: &AppHandle,
    track_id: &str,
    status: &str,
    message: Option<&str>,
    file_size: Option<u64>,
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
) {
    let mut payload = serde_json::json!({
        "trackId": track_id,
        "status": status,
    });
    if let Some(msg) = message {
        payload["message"] = serde_json::Value::String(msg.to_string());
    }
    if let Some(size) = file_size {
        payload["fileSize"] = serde_json::json!(size);
    }
    if let Some(downloaded) = downloaded_bytes {
        payload["downloadedBytes"] = serde_json::json!(downloaded);
    }
    if let Some(total) = total_bytes {
        payload["totalBytes"] = serde_json::json!(total);
    }
    let _ = app.emit("download-progress", payload);
}

async fn perform_download(
    app: AppHandle,
    client: reqwest::Client,
    cancel_flag: Arc<AtomicBool>,
    url: String,
    track_id: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u64,
    cover_url: Option<String>,
    source: String,
    download_dir: Option<String>,
    name_template: Option<String>,
) -> AppResult<DownloadedTrack> {
    // 检查是否已下载
    let existing = read_manifest(&app)?;
    if existing.iter().any(|t| t.id == track_id) {
        emit_download_progress(&app, &track_id, "already_exists", None, None, None, None);
        return Err(AppError::Other("Track already downloaded".into()));
    }

    // 根据 URL 域名动态设置 Referer（复用 player_cmd 逻辑）
    let referer = if url.contains("bilibili.com") || url.contains("bilivideo.") {
        "https://www.bilibili.com"
    } else if url.contains("youtube.com") || url.contains("googlevideo.com") {
        "https://music.youtube.com"
    } else {
        "https://music.163.com"
    };

    let resp = client
        .get(&url)
        .header("Referer", referer)
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(AppError::Api(format!("HTTP {}", resp.status())));
    }

    // 从 Content-Type 推断扩展名
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/mpeg")
        .to_string();
    let total_bytes = resp.content_length();
    let ext = ext_from_content_type(&content_type);

    // 构造文件名：使用模板
    let base_name =
        render_download_filename(&title, &artist, &album, &source, name_template.as_deref());
    let filename = format!("{}.{}", base_name, ext);

    let dir = downloads_dir(&app, download_dir.as_deref())?;
    let file_path = dir.join(&filename);
    let mut file = tokio::fs::File::create(&file_path).await?;

    let mut file_size = 0_u64;
    let mut stream = resp.bytes_stream();
    let mut last_emit_at = Instant::now() - Duration::from_millis(500);
    let mut last_emitted_bytes = 0_u64;
    emit_download_progress(
        &app,
        &track_id,
        "downloading",
        None,
        None,
        Some(0),
        total_bytes,
    );
    while let Some(chunk) = stream.next().await {
        if cancel_flag.load(Ordering::Relaxed) {
            let _ = tokio::fs::remove_file(&file_path).await;
            return Err(AppError::Other("Download cancelled".into()));
        }
        let chunk = chunk?;
        if chunk.is_empty() {
            continue;
        }
        file.write_all(&chunk).await?;
        file_size += chunk.len() as u64;

        let should_emit = total_bytes.map(|total| file_size >= total).unwrap_or(false)
            || file_size.saturating_sub(last_emitted_bytes) >= 256 * 1024
            || last_emit_at.elapsed() >= Duration::from_millis(200);

        if should_emit {
            emit_download_progress(
                &app,
                &track_id,
                "downloading",
                None,
                None,
                Some(file_size),
                total_bytes,
            );
            last_emit_at = Instant::now();
            last_emitted_bytes = file_size;
        }
    }
    file.flush().await?;

    if cancel_flag.load(Ordering::Relaxed) {
        let _ = tokio::fs::remove_file(&file_path).await;
        return Err(AppError::Other("Download cancelled".into()));
    }

    if file_size == 0 {
        let _ = tokio::fs::remove_file(&file_path).await;
        return Err(AppError::Audio("Empty audio data received".into()));
    }

    // 构造记录
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let track = DownloadedTrack {
        id: track_id.clone(),
        title,
        artist,
        album,
        duration_ms,
        cover_url,
        source,
        file_path: file_path.to_string_lossy().to_string(),
        file_size,
        downloaded_at: now,
    };

    let mut manifest = read_manifest(&app)?;
    manifest.push(track.clone());
    write_manifest(&app, &manifest)?;

    emit_download_progress(
        &app,
        &track_id,
        "complete",
        None,
        Some(file_size),
        Some(file_size),
        total_bytes.or(Some(file_size)),
    );
    Ok(track)
}

/// 下载音频文件并保存到本地
#[tauri::command]
pub async fn download_track(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    track_id: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: u64,
    cover_url: Option<String>,
    source: String,
    download_dir: Option<String>,
    name_template: Option<String>,
) -> AppResult<()> {
    {
        let mut tasks = state.download_tasks.lock();
        if let Some(existing) = tasks.get(&track_id) {
            if !existing.handle.is_finished() {
                return Err(AppError::Other("Track is already downloading".into()));
            }
        }
        tasks.retain(|_, control| !control.handle.is_finished());
    }

    emit_download_progress(&app, &track_id, "start", None, None, None, None);

    let app_handle = app.clone();
    let task_track_id = track_id.clone();
    let client = state.http();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let task_cancel_flag = cancel_flag.clone();
    let handle = tokio::spawn(async move {
        let result = perform_download(
            app_handle.clone(),
            client,
            task_cancel_flag,
            url,
            task_track_id.clone(),
            title,
            artist,
            album,
            duration_ms,
            cover_url,
            source,
            download_dir,
            name_template,
        )
        .await;

        if let Err(err) = result {
            let message = err.to_string();
            if message.to_lowercase().contains("cancelled")
                || message.to_lowercase().contains("canceled")
            {
                emit_download_progress(
                    &app_handle,
                    &task_track_id,
                    "cancelled",
                    None,
                    None,
                    None,
                    None,
                );
            } else if !message.contains("Track already downloaded") {
                emit_download_progress(
                    &app_handle,
                    &task_track_id,
                    "error",
                    Some(&message),
                    None,
                    None,
                    None,
                );
            }
        }

        app_handle
            .state::<AppState>()
            .download_tasks
            .lock()
            .remove(&task_track_id);
    });

    state.download_tasks.lock().insert(
        track_id,
        crate::state::DownloadTaskControl {
            cancel_flag,
            handle,
        },
    );
    Ok(())
}

/// 列出所有已下载的曲目
#[tauri::command]
pub async fn list_downloads(app: AppHandle) -> AppResult<Vec<DownloadedTrack>> {
    Ok(validate_manifest_files(&app)?.tracks)
}

/// 校验下载清单，自动移除磁盘文件已不存在的记录
#[tauri::command]
pub async fn validate_downloads(app: AppHandle) -> AppResult<DownloadManifestValidation> {
    validate_manifest_files(&app)
}

/// 删除已下载的曲目（文件 + manifest 记录）
#[tauri::command]
pub async fn delete_download(app: AppHandle, track_id: String) -> AppResult<()> {
    let mut manifest = read_manifest(&app)?;

    // 查找并移除
    let idx = manifest.iter().position(|t| t.id == track_id);
    if let Some(i) = idx {
        let track = manifest.remove(i);
        // 删除磁盘文件（忽略错误，文件可能已被手动删除）
        let _ = std::fs::remove_file(&track.file_path);
        write_manifest(&app, &manifest)?;
    } else {
        return Err(AppError::NotFound("Download not found".into()));
    }

    Ok(())
}

/// 取消单个下载任务
#[tauri::command]
pub async fn cancel_download(
    app: AppHandle,
    state: State<'_, AppState>,
    track_id: String,
) -> AppResult<bool> {
    let tasks = state.download_tasks.lock();
    if let Some(control) = tasks.get(&track_id) {
        control.cancel_flag.store(true, Ordering::Relaxed);
        Ok(true)
    } else {
        let _ = app;
        Ok(false)
    }
}

/// 取消全部下载任务
#[tauri::command]
pub async fn cancel_all_downloads(app: AppHandle, state: State<'_, AppState>) -> AppResult<usize> {
    let tasks = state.download_tasks.lock();
    let cancelled = tasks.len();
    for (_track_id, control) in tasks.iter() {
        control.cancel_flag.store(true, Ordering::Relaxed);
    }
    let _ = app;
    Ok(cancelled)
}

/// 在系统文件管理器中显示文件
#[tauri::command]
pub async fn reveal_file(path: String) -> AppResult<()> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(AppError::NotFound("File not found".into()));
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| AppError::Other(e.to_string()))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| AppError::Other(e.to_string()))?;
    }
    #[cfg(target_os = "linux")]
    {
        // xdg-open on parent directory
        if let Some(parent) = p.parent() {
            std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| AppError::Other(e.to_string()))?;
        }
    }
    Ok(())
}
