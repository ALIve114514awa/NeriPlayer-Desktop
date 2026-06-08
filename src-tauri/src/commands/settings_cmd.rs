use crate::api::bilibili::client::BiliClient;
use crate::api::netease::client::NeteaseClient;
use crate::api::qq::client::QqMusicClient;
use crate::api::youtube::client::YouTubeClient;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use serde::Serialize;
use tauri::{Manager, State};

#[tauri::command]
pub async fn get_app_data_dir(app: tauri::AppHandle) -> AppResult<String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok(dir.to_string_lossy().to_string())
}

/// 获取网易云歌曲播放 URL
#[derive(Serialize)]
pub struct SongUrlResult {
    pub url: Option<String>,
    pub bitrate: u64,
    pub format: String,
}

#[tauri::command]
pub async fn get_netease_song_url(
    song_id: u64,
    quality: String,
    state: State<'_, AppState>,
) -> AppResult<SongUrlResult> {
    let client = NeteaseClient::new(&state.http());
    let result = client.get_song_url(song_id, &quality).await?;
    Ok(SongUrlResult {
        url: result.url,
        bitrate: result.br,
        format: result.r#type,
    })
}

#[tauri::command]
pub async fn get_qq_song_url(
    song_mid: String,
    quality: String,
    state: State<'_, AppState>,
) -> AppResult<SongUrlResult> {
    let client = QqMusicClient::new(&state.http());
    let result = client.get_song_url(&song_mid, &quality).await?;
    Ok(SongUrlResult {
        url: result.url,
        bitrate: result.bitrate,
        format: result.format,
    })
}

/// 获取B站音频流 URL
#[derive(Serialize)]
pub struct BiliAudioResult {
    pub url: String,
    pub bandwidth: u64,
    pub codecs: String,
}

#[tauri::command]
pub async fn get_bili_audio_url(
    bvid: String,
    avid: Option<u64>,
    cid: Option<u64>,
    state: State<'_, AppState>,
) -> AppResult<BiliAudioResult> {
    let client = BiliClient::new(&state.http());

    // 确定 bvid 和 cid
    let (real_bvid, real_cid) = if let Some(aid) = avid {
        // 通过 avid 获取视频信息（同步歌曲场景）
        let info = client.get_video_info_by_avid(aid).await?;
        let c = cid.unwrap_or(info.cid);
        (info.bvid, c)
    } else {
        let info = client.get_video_info(&bvid).await?;
        let c = cid.unwrap_or(info.cid);
        (bvid, c)
    };

    let streams = client.get_audio_url(&real_bvid, real_cid).await?;
    let best = streams
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Api("No audio stream found".into()))?;
    Ok(BiliAudioResult {
        url: best.url,
        bandwidth: best.bandwidth,
        codecs: best.codecs,
    })
}

/// 获取 YouTube 音频流 URL
#[derive(Serialize)]
pub struct YtAudioResult {
    pub url: String,
    pub bitrate: u64,
    pub mime_type: String,
    pub content_length: u64,
}

#[tauri::command]
pub async fn get_youtube_audio_url(
    video_id: String,
    state: State<'_, AppState>,
) -> AppResult<Vec<YtAudioResult>> {
    let client = YouTubeClient::new(&state.http());
    let streams = client.get_streams(&video_id).await?;
    Ok(streams
        .into_iter()
        .map(|s| YtAudioResult {
            url: s.url,
            bitrate: s.bitrate,
            mime_type: s.mime_type,
            content_length: s.content_length,
        })
        .collect())
}

/// 将字节数据保存到本地文件（供前端封面保存等场景使用）
#[tauri::command]
pub async fn save_file_bytes(path: String, data: Vec<u8>) -> AppResult<()> {
    std::fs::write(&path, &data).map_err(|e| AppError::Other(e.to_string()))
}

/// 设置绕过代理（前端保存设置后通知后端重建 HTTP Client）
#[tauri::command]
pub async fn set_bypass_proxy(bypass: bool, state: State<'_, AppState>) -> AppResult<()> {
    state.rebuild_http(bypass);
    Ok(())
}

/// 获取构建信息
#[derive(Serialize)]
pub struct BuildInfo {
    pub build_uuid: String,
    pub build_timestamp: String,
    pub version: String,
}

#[tauri::command]
pub async fn get_build_info(app: tauri::AppHandle) -> AppResult<BuildInfo> {
    let version = app.package_info().version.to_string();
    Ok(BuildInfo {
        build_uuid: env!("BUILD_UUID").to_string(),
        build_timestamp: env!("BUILD_TIMESTAMP").to_string(),
        version,
    })
}
