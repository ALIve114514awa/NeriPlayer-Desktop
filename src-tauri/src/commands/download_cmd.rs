use crate::error::{AppError, AppResult};
use crate::lyrics::manager::LyricsManager;
use crate::lyrics::parser::LyricLine;
use crate::settings::store::DEFAULT_DOWNLOAD_NAME_TEMPLATE;
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

/// 流式下载的空闲超时：超过该时长收不到任何数据视为连接假死。
/// 半开 TCP 下 `stream.next()` 会永久阻塞——任务卡死且因注册表判
/// "downloading" 无法重试，必须有兜底
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 文件名 stem 的最大字节数（UTF-8）：为扩展名与 " (n)" 后缀留余量，
/// 避免超出各文件系统 255 字节单文件名 / Windows MAX_PATH 限制
const MAX_FILENAME_STEM_BYTES: usize = 180;

/// Windows 设备保留名：即使带扩展名（如 CON.mp3）也会创建失败或写入设备
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 按字节数截断，绝不切断多字节字符
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// 清理文件名中的非法字符
///
/// 覆盖：路径分隔/Windows 非法字符、控制字符（0x00-0x1F 等）、结尾的
/// `.`/空格（Windows 非法）、Windows 设备保留名（加 `_` 前缀）、
/// 180 字节截断（扩展名由调用方追加）
fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            _ => c,
        })
        .collect();
    let cleaned = truncate_at_char_boundary(cleaned.trim(), MAX_FILENAME_STEM_BYTES)
        .trim_end_matches(['.', ' '])
        .to_string();
    // 保留名按首个 `.` 之前的 stem 判定（CON 与 CON.tail 都保留给设备）
    let stem = cleaned.split('.').next().unwrap_or("");
    let reserved = WINDOWS_RESERVED_NAMES
        .iter()
        .any(|name| stem.eq_ignore_ascii_case(name));
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
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
    let tpl = template
        .filter(|t| !t.is_empty())
        .unwrap_or(DEFAULT_DOWNLOAD_NAME_TEMPLATE);
    let rendered = tpl
        .replace("{title}", title)
        .replace("{artist}", artist)
        .replace("{album}", album)
        .replace("{source}", source)
        .replace("%title%", title)
        .replace("%artist%", artist)
        .replace("%album%", album)
        .replace("%source%", source);
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

fn ext_from_image_content_type(content_type: &str) -> &str {
    if content_type.contains("png") {
        "png"
    } else if content_type.contains("webp") {
        "webp"
    } else if content_type.contains("gif") {
        "gif"
    } else {
        "jpg"
    }
}

fn make_lrc_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let centis = (ms % 1000) / 10;
    format!("{:02}:{:02}.{:02}", minutes, seconds, centis)
}

fn build_lrc_text(lines: &[LyricLine]) -> Option<String> {
    let mut out = String::new();
    for line in lines {
        let text = line.text.trim();
        if text.is_empty() {
            continue;
        }
        out.push('[');
        out.push_str(&make_lrc_timestamp(line.start_ms));
        out.push(']');
        out.push_str(text);
        out.push('\n');
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn build_translation_lrc_text(lines: &[LyricLine]) -> Option<String> {
    let mut out = String::new();
    for line in lines {
        let Some(translated) = line.translation.as_deref() else {
            continue;
        };
        let text = translated.trim();
        if text.is_empty() {
            continue;
        }
        out.push('[');
        out.push_str(&make_lrc_timestamp(line.start_ms));
        out.push(']');
        out.push_str(text);
        out.push('\n');
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_netease_id(track_id: &str) -> Option<u64> {
    track_id
        .strip_prefix("netease:")
        .and_then(|id| id.parse::<u64>().ok())
}

fn extract_qq_song_mid(track_id: &str) -> Option<&str> {
    track_id.strip_prefix("qq:").filter(|id| !id.is_empty())
}

async fn write_download_sidecars(
    client: &reqwest::Client,
    file_path: &std::path::Path,
    track_id: &str,
    title: &str,
    artist: &str,
    duration_ms: u64,
    cover_url: Option<&str>,
) -> AppResult<()> {
    let base_path = file_path.with_extension("");
    let mut written_files: Vec<PathBuf> = Vec::new();

    // 歌词 sidecar
    let lyrics_manager = LyricsManager::new(client);
    let youtube_video_id = track_id.strip_prefix("youtube:").filter(|id| !id.is_empty());
    let lyrics = lyrics_manager
        .fetch_lyrics(
            title,
            artist,
            duration_ms / 1000,
            None,
            extract_netease_id(track_id),
            extract_qq_song_mid(track_id),
            youtube_video_id,
        )
        .await
        .unwrap_or_default();

    if let Some(lrc_text) = build_lrc_text(&lyrics) {
        let lrc_path = base_path.with_extension("lrc");
        std::fs::write(&lrc_path, lrc_text)?;
        written_files.push(lrc_path);
    }

    if let Some(tlrc_text) = build_translation_lrc_text(&lyrics) {
        let tlrc_path = base_path.with_extension("tlrc");
        std::fs::write(&tlrc_path, tlrc_text)?;
        written_files.push(tlrc_path);
    }

    // 封面 sidecar
    if let Some(raw_cover_url) = cover_url {
        let normalized_cover_url = if raw_cover_url.starts_with("//") {
            format!("https:{}", raw_cover_url)
        } else {
            raw_cover_url.to_string()
        };
        if !normalized_cover_url.trim().is_empty() {
            if let Ok(resp) = client
                .get(&normalized_cover_url)
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
                .send()
                .await
            {
                if resp.status().is_success() {
                    let content_type = resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("image/jpeg")
                        .to_lowercase();
                    if content_type.starts_with("image/") {
                        if let Ok(bytes) = resp.bytes().await {
                            if !bytes.is_empty() {
                                let ext = ext_from_image_content_type(&content_type);
                                let cover_path = base_path.with_extension(ext);
                                std::fs::write(&cover_path, &bytes)?;
                                written_files.push(cover_path);
                            }
                        }
                    }
                }
            }
        }
    }

    if written_files.is_empty() {
        return Ok(());
    }
    Ok(())
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
        std::fs::create_dir_all(&dir).map_err(AppError::Io)?;
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
        std::fs::create_dir_all(&dir).map_err(AppError::Io)?;
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

/// 写入 manifest（原子写：半截 manifest 会让全部下载记录判损坏丢失）
fn write_manifest(app: &AppHandle, tracks: &[DownloadedTrack]) -> AppResult<()> {
    let path = manifest_path(app)?;
    let json = serde_json::to_string_pretty(tracks)?;
    crate::fsutil::atomic_write(&path, json)?;
    Ok(())
}

fn candidate_download_sidecars(audio_path: &std::path::Path) -> Vec<PathBuf> {
    let Some(parent) = audio_path.parent() else {
        return Vec::new();
    };
    let Some(stem) = audio_path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };

    [
        "lrc",
        "tlrc",
        "txt",
        "translated.lrc",
        "translation.lrc",
        "jpg",
        "jpeg",
        "png",
        "webp",
        "gif",
    ]
    .into_iter()
    .map(|suffix| parent.join(format!("{}.{}", stem, suffix)))
    .collect()
}

fn remove_download_artifacts(file_path: &str) {
    let audio_path = std::path::Path::new(file_path);
    // 先删同名 sidecar，再删主音频；全部忽略错误，避免手动删除/占用时影响 manifest 清理。
    for sidecar in candidate_download_sidecars(audio_path) {
        if sidecar.is_file() {
            let _ = std::fs::remove_file(sidecar);
        }
    }
    let _ = std::fs::remove_file(audio_path);
}

/// 清扫遗留的 `*.part` 半截下载文件
///
/// 只清理最后修改超过 1 小时的：下载中的 .part 由任务自身负责删除，
/// 且流有 30s 空闲超时——停滞任务会在分钟级内自行失败并清掉自己的
/// .part；能活过 1 小时的必然是崩溃/断电遗留
fn sweep_stale_part_files(dirs: impl IntoIterator<Item = PathBuf>) -> usize {
    const STALE_AFTER: Duration = Duration::from_secs(3_600);
    let mut removed = 0_usize;
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for dir in dirs {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("part") || !path.is_file() {
                continue;
            }
            let stale = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.elapsed().ok())
                .is_some_and(|age| age >= STALE_AFTER);
            if stale && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

fn validate_manifest_files(app: &AppHandle) -> AppResult<DownloadManifestValidation> {
    let manifest = read_manifest(app)?;

    // 顺带清扫崩溃/断电遗留的 .part：默认下载目录 + manifest 涉及的自定义目录
    let mut sweep_dirs: Vec<PathBuf> = manifest
        .iter()
        .filter_map(|track| {
            std::path::Path::new(&track.file_path)
                .parent()
                .map(|parent| parent.to_path_buf())
        })
        .collect();
    if let Ok(default_dir) = default_downloads_dir(app) {
        sweep_dirs.push(default_dir);
    }
    let swept = sweep_stale_part_files(sweep_dirs);
    if swept > 0 {
        log::info!(target: "download", "swept {} stale .part files", swept);
    }

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
            // 主音频已丢失时，也顺手清理同名歌词/封面 sidecar，避免残留。
            remove_download_artifacts(&track.file_path);
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

// 播放/下载编排函数的参数都是相互独立的运行时上下文，聚成结构体只是换个地方堆字段
#[allow(clippy::too_many_arguments)]
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
    } else if url.contains("qqmusic.qq.com") || url.contains("y.qq.com") {
        "https://y.qq.com"
    } else {
        "https://music.163.com"
    };

    // YouTube googlevideo CDN 校验拉流 UA 与直链 `c=` 客户端一致, 不一致 403;
    // 故 googlevideo 直链按客户端选匹配 UA, 其它平台用桌面 Chrome UA (对齐 player_cmd)
    let user_agent = if url.contains("googlevideo.com") || url.contains("youtube.com") {
        crate::api::youtube::playback::stream_user_agent_for_url(&url)
    } else {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    };

    let resp = client
        .get(&url)
        .header("Referer", referer)
        .header("User-Agent", user_agent)
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

    let dir = downloads_dir(&app, download_dir.as_deref())?;
    // 同名碰撞：本曲目已下载会在函数入口提前返回，走到这里时目标文件若已
    // 存在则必属于其它曲目（或历史遗留）——直接覆盖会互删数据、manifest
    // 双记录指向同一文件。追加 " (2)"/" (3)" 后缀避让
    let mut file_path = dir.join(format!("{}.{}", base_name, ext));
    let mut collision_suffix = 2_u32;
    while file_path.exists() {
        file_path = dir.join(format!("{} ({}).{}", base_name, collision_suffix, ext));
        collision_suffix += 1;
        if collision_suffix > 1_000 {
            return Err(AppError::Other("Too many filename collisions".into()));
        }
    }
    // 先写 `<final>.part` 临时文件，完整收尾后才 rename 为最终名：
    // 失败/取消/崩溃只会留下可识别清扫的 .part，不会产生半截"成品"文件
    let part_path = {
        let mut name = file_path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".part");
        file_path.with_file_name(name)
    };
    let mut file = tokio::fs::File::create(&part_path).await?;

    let mut stream = resp.bytes_stream();
    emit_download_progress(
        &app,
        &track_id,
        "downloading",
        None,
        None,
        Some(0),
        total_bytes,
    );
    // 集中收敛写入阶段的所有失败出口，统一在块外清理 .part
    let stream_outcome: AppResult<u64> = async {
        let mut file_size = 0_u64;
        let mut last_emit_at = Instant::now() - Duration::from_millis(500);
        let mut last_emitted_bytes = 0_u64;
        loop {
            // 空闲超时兜底：半开 TCP 下 next() 会永久挂起（见 STREAM_STALL_TIMEOUT）
            let next = match tokio::time::timeout(STREAM_STALL_TIMEOUT, stream.next()).await {
                Ok(item) => item,
                Err(_) => {
                    return Err(AppError::Other(
                        "Download stalled: no data received for 30s".into(),
                    ));
                }
            };
            let Some(chunk) = next else { break };
            if cancel_flag.load(Ordering::Relaxed) {
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
            return Err(AppError::Other("Download cancelled".into()));
        }
        if file_size == 0 {
            return Err(AppError::Audio("Empty audio data received".into()));
        }
        Ok(file_size)
    }
    .await;

    // rename 前必须关句柄（Windows 上打开中的文件无法作为 rename 目标源）
    drop(file);
    let file_size = match stream_outcome {
        Ok(size) => size,
        Err(error) => {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(error);
        }
    };
    if let Err(error) = tokio::fs::rename(&part_path, &file_path).await {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(AppError::Io(error));
    }

    // 下载 sidecar（歌词/翻译歌词/封面），失败不影响主音频
    if let Err(e) = write_download_sidecars(
        &client,
        &file_path,
        &track_id,
        &title,
        &artist,
        duration_ms,
        cover_url.as_deref(),
    )
    .await
    {
        log::warn!(
            target: "download",
            "write sidecars failed: track_id={}, title={}, error={}",
            track_id, title, e
        );
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
// Tauri 命令签名由 IPC 契约决定：参数必须平铺，改成结构体会同时改掉前端调用点
#[allow(clippy::too_many_arguments)]
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
        // 删除磁盘文件 + 同名 sidecar（忽略错误，文件可能已被手动删除）
        remove_download_artifacts(&track.file_path);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 非法字符、控制字符替换为下划线，键盘可见字符原样保留
    #[test]
    fn sanitize_replaces_illegal_and_control_chars() {
        assert_eq!(sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_filename("bad\u{0}name\u{1f}\ttail"), "bad_name__tail");
        assert_eq!(sanitize_filename("正常 - 歌名 (Live)"), "正常 - 歌名 (Live)");
    }

    /// Windows 不允许结尾的点与空格
    #[test]
    fn sanitize_strips_trailing_dots_and_spaces() {
        assert_eq!(sanitize_filename("track name. . ."), "track name");
        assert_eq!(sanitize_filename("  spaced  "), "spaced");
    }

    /// Windows 设备保留名（含带扩展名的 stem）必须加前缀，大小写不敏感
    #[test]
    fn sanitize_prefixes_windows_reserved_names() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con"), "_con");
        assert_eq!(sanitize_filename("Com1"), "_Com1");
        assert_eq!(sanitize_filename("NUL.mp3"), "_NUL.mp3");
        assert_eq!(sanitize_filename("LPT9.flac"), "_LPT9.flac");
        // 相似但不保留的名字不受影响
        assert_eq!(sanitize_filename("CONCERT"), "CONCERT");
        assert_eq!(sanitize_filename("COM10"), "COM10");
    }

    /// stem 超长时按字节截断到 180，且绝不切断多字节字符
    #[test]
    fn sanitize_truncates_stem_to_180_bytes_at_char_boundary() {
        let long_ascii = "a".repeat(400);
        assert_eq!(sanitize_filename(&long_ascii).len(), MAX_FILENAME_STEM_BYTES);

        // 中文 3 字节/字：180/3=60 字整除；用 61+ 字验证边界处理
        let long_cjk = "歌".repeat(100);
        let out = sanitize_filename(&long_cjk);
        assert!(out.len() <= MAX_FILENAME_STEM_BYTES);
        assert!(out.chars().all(|c| c == '歌'), "不得出现被切断的乱码字符");

        // 2 字节字符错位对齐：179 字节处落在字符中间也要安全回退
        let mixed = format!("{}é", "a".repeat(179));
        let out = sanitize_filename(&mixed);
        assert!(out.len() <= MAX_FILENAME_STEM_BYTES);
        assert!(out.is_char_boundary(out.len()));
    }

    /// 超 1 小时的 .part 会被清扫，新鲜 .part 与正常文件保留
    #[test]
    fn stale_part_files_are_swept_but_fresh_ones_kept() {
        let dir = std::env::temp_dir().join(format!("neri-part-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("old.mp3.part");
        let fresh = dir.join("new.mp3.part");
        let audio = dir.join("keep.mp3");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&audio, b"x").unwrap();
        // 把 stale 的 mtime 拨回 2 小时前
        let old_time = std::time::SystemTime::now() - Duration::from_secs(7_200);
        let file = std::fs::OpenOptions::new().write(true).open(&stale).unwrap();
        file.set_modified(old_time).unwrap();
        drop(file);

        let removed = sweep_stale_part_files([dir.clone()]);

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(audio.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
