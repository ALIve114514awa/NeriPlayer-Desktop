// YouTube Music 播放客户端
//
// 分层 (对齐 yt-dlp 默认 jsless 客户端 + 桌面防互踢):
// 1. 主路径 ANDROID_VR + visitorData (yt-dlp _DEFAULT_JSLESS_CLIENTS)
//    IOS/ANDROID plain url 在 2026-07 实测会被 googlevideo 限到约 1MB 后 403
//    ANDROID_VR 无此限速, 但缺少 visitorData 时会 LOGIN_REQUIRED bot check
// 2. player API 在已登录时携带用户 Cookie (不附 SAPISID*HASH)
//    mobile player + SAPISIDHASH 会被 Innertube 以 HTTP 400 INVALID_ARGUMENT 拒绝
//    WEB_REMIX 库接口继续用完整 hash; 播放侧故意不走 WEB_REMIX + PO token 完整浏览器模拟
// 3. googlevideo CDN 拉流不附带登录 Cookie (Android buildYouTubeStreamRequestHeaders 同款)
// 4. 仅接受 plain url 或已带 sig 的 cipher; 加密 s= 需 player JS 解签, 当前跳过并回退
// 5. 排序优先 audio/mp4 (AAC): rodio/symphonia 未启 opus, webm/opus 会 unsupported codec
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

use crate::auth::state::YouTubeAuth;
use crate::error::{AppError, AppResult};

use super::client::YtAudioStream;

// visitorData 缓存: ANDROID_VR 等 jsless 客户端依赖它绕过 bot check
const VISITOR_DATA_TTL: Duration = Duration::from_secs(30 * 60);
const WATCH_PAGE_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

struct VisitorDataCache {
    value: String,
    fetched_at: Instant,
}

static VISITOR_DATA_CACHE: Mutex<Option<VisitorDataCache>> = Mutex::new(None);

// 桌面播放端点: 非 WEB_REMIX 客户端统一走 www, 降低与 music 登录会话的关联
const PLAYER_URL_WWW: &str = "https://www.youtube.com/youtubei/v1/player";
const PLAYER_URL_MUSIC: &str = "https://music.youtube.com/youtubei/v1/player";
const ORIGIN_WWW: &str = "https://www.youtube.com";
const ORIGIN_MUSIC: &str = "https://music.youtube.com";

// 公开 InnerTube key (与 WEB 客户端共用, 非密钥; 失效时仍可无 key 调用)
const DEFAULT_PLAYER_API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";

// ANDROID_MUSIC 常用 player params: 请求音频向格式
const ANDROID_MUSIC_PLAYER_PARAMS: &str = "CgIIAdgDAQ==";

// googlevideo CDN 拉流 User-Agent (对齐 Android resolveYouTubeStreamUserAgent).
// CDN 会校验拉流 UA 与 stream URL 中 `c=` 客户端参数一致, 不一致直接 403.
// 因此拉流侧必须按铸造该直链的客户端选择匹配 UA, 而非固定 Chrome UA.
// 注意: 下列 UA 字符串须与 playback_client_profiles() 中对应 client_version 同步.
const STREAM_ANDROID_VR_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const STREAM_IOS_USER_AGENT: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
const STREAM_ANDROID_USER_AGENT: &str =
    "com.google.android.youtube/20.10.38 (Linux; U; Android 15) gzip";
const STREAM_ANDROID_MUSIC_USER_AGENT: &str =
    "com.google.android.apps.youtube.music/8.32.52 (Linux; U; Android 15) gzip";
// WEB / TV 及未知客户端回退到桌面 Chrome UA
const STREAM_WEB_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy)]
enum PlayerHost {
    Www,
    // 预留给将来可选的 music.youtube.com player (默认不用, 防会话耦合)
    #[allow(dead_code)]
    Music,
}

#[derive(Debug, Clone)]
struct PlayerClientProfile {
    client_id: &'static str,
    client_name: &'static str,
    client_version: &'static str,
    user_agent: &'static str,
    platform: &'static str,
    /// 兜底 locale；实际请求以 innertube_locale() 为准，随国际化开关变化
    #[allow(dead_code)]
    hl: &'static str,
    #[allow(dead_code)]
    gl: &'static str,
    host: PlayerHost,
    android_sdk_version: Option<i32>,
    os_name: Option<&'static str>,
    os_version: Option<&'static str>,
    device_make: Option<&'static str>,
    device_model: Option<&'static str>,
    // 部分移动客户端依赖 playerParams 才吐 plain url
    player_params: Option<&'static str>,
    // ANDROID_VR 等 jsless 客户端缺 visitorData 会被 bot check 拦下
    requires_visitor_data: bool,
}

/// 播放客户端顺序 (对齐 yt-dlp 2026-07 默认 jsless 路径):
/// ANDROID_VR (主) -> IOS -> ANDROID -> ANDROID_MUSIC -> TVHTML5
///
/// 2026-07 实测关键结论:
/// - IOS/ANDROID plain url 可解析, 但 googlevideo 约 1MB 后 Range/整文件 403 (无 n/ratebypass)
/// - ANDROID_VR + visitorData 可拿到可完整下载的 plain url (yt-dlp 同款)
/// - ANDROID_VR 无 visitorData -> LOGIN_REQUIRED bot check
/// - 故意不包含 WEB_REMIX player (需 PO token / 完整浏览器模拟, 互踢风险高)
/// - 登录时 player 只附 Cookie, 不附 SAPISID*HASH (mobile + hash = HTTP 400)
fn playback_client_profiles() -> &'static [PlayerClientProfile] {
    &[
        // 主路径: yt-dlp _DEFAULT_JSLESS_CLIENTS; 需 visitorData, 直链可完整下载
        PlayerClientProfile {
            client_id: "28",
            client_name: "ANDROID_VR",
            client_version: "1.65.10",
            user_agent: STREAM_ANDROID_VR_USER_AGENT,
            platform: "MOBILE",
            hl: "en",
            gl: "US",
            host: PlayerHost::Www,
            android_sdk_version: Some(32),
            os_name: Some("Android"),
            os_version: Some("12L"),
            device_make: Some("Oculus"),
            device_model: Some("Quest 3"),
            player_params: None,
            requires_visitor_data: true,
        },
        // 回退: 仍可解析 plain url, 但 CDN 可能限速; 保留作 ANDROID_VR 失败时兜底
        PlayerClientProfile {
            client_id: "5",
            client_name: "IOS",
            client_version: "20.10.4",
            user_agent: STREAM_IOS_USER_AGENT,
            platform: "MOBILE",
            hl: "en",
            gl: "US",
            host: PlayerHost::Www,
            android_sdk_version: None,
            os_name: Some("iOS"),
            os_version: Some("18.3.2"),
            device_make: Some("Apple"),
            device_model: Some("iPhone"),
            player_params: None,
            requires_visitor_data: false,
        },
        PlayerClientProfile {
            client_id: "3",
            client_name: "ANDROID",
            client_version: "20.10.38",
            user_agent: STREAM_ANDROID_USER_AGENT,
            platform: "MOBILE",
            hl: "en",
            gl: "US",
            host: PlayerHost::Www,
            android_sdk_version: Some(35),
            os_name: Some("Android"),
            os_version: Some("15"),
            device_make: Some("Google"),
            device_model: Some("Pixel 8"),
            player_params: None,
            requires_visitor_data: false,
        },
        // 音乐客户端: Cookie-only 时常 LOGIN_REQUIRED; 有完整 mobile OAuth 时更利于 Premium
        PlayerClientProfile {
            client_id: "21",
            client_name: "ANDROID_MUSIC",
            client_version: "8.32.52",
            user_agent: STREAM_ANDROID_MUSIC_USER_AGENT,
            platform: "MOBILE",
            hl: "en",
            gl: "US",
            host: PlayerHost::Www,
            android_sdk_version: Some(35),
            os_name: Some("Android"),
            os_version: Some("15"),
            device_make: Some("Google"),
            device_model: Some("Pixel 8"),
            player_params: Some(ANDROID_MUSIC_PLAYER_PARAMS),
            requires_visitor_data: false,
        },
        // TV 客户端目前常返回 "page needs to be reloaded", 保留为最后兜底
        PlayerClientProfile {
            client_id: "7",
            client_name: "TVHTML5",
            client_version: "7.20250709.16.00",
            user_agent: "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/25.lts.30.1034943-gold (unlike Gecko), Unknown_TV_Unknown_0/Unknown (Unknown, Unknown)",
            platform: "TV",
            hl: "en",
            gl: "US",
            host: PlayerHost::Www,
            android_sdk_version: None,
            os_name: None,
            os_version: None,
            device_make: None,
            device_model: None,
            player_params: None,
            requires_visitor_data: false,
        },
    ]
}

fn player_endpoint(profile: &PlayerClientProfile) -> (&'static str, &'static str) {
    match profile.host {
        PlayerHost::Www => (PLAYER_URL_WWW, ORIGIN_WWW),
        PlayerHost::Music => (PLAYER_URL_MUSIC, ORIGIN_MUSIC),
    }
}

fn build_player_context(profile: &PlayerClientProfile, visitor_data: Option<&str>) -> Value {
    // 播放侧同样跟随国际化开关，否则库列表和可播曲目的区域会不一致
    let locale = super::innertube_locale();
    let mut client = json!({
        "clientName": profile.client_name,
        "clientVersion": profile.client_version,
        "hl": locale.0,
        "gl": locale.1,
        "platform": profile.platform,
        "userAgent": profile.user_agent,
        "utcOffsetMinutes": 0
    });

    if let Some(sdk) = profile.android_sdk_version {
        client["androidSdkVersion"] = json!(sdk);
    }
    if let Some(os_name) = profile.os_name {
        client["osName"] = json!(os_name);
    }
    if let Some(os_version) = profile.os_version {
        client["osVersion"] = json!(os_version);
    }
    if let Some(device_make) = profile.device_make {
        client["deviceMake"] = json!(device_make);
    }
    if let Some(device_model) = profile.device_model {
        client["deviceModel"] = json!(device_model);
    }
    // ANDROID_VR 等 jsless 客户端: visitorData 是绕过 bot check 的关键
    if let Some(vd) = visitor_data.map(str::trim).filter(|s| !s.is_empty()) {
        client["visitorData"] = json!(vd);
    }

    json!({
        "client": client,
        "user": { "lockedSafetyMode": false }
    })
}

fn build_player_body(
    profile: &PlayerClientProfile,
    video_id: &str,
    visitor_data: Option<&str>,
) -> Value {
    let mut body = json!({
        "context": build_player_context(profile, visitor_data),
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
        // 对齐 Android/桌面: 声明 HTML5 偏好, 提高 progressive 直链概率
        "playbackContext": {
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
                "lactMilliseconds": "9",
                "autonavState": "STATE_OFF",
                "autoCaptionsDefaultOn": false,
                "vis": 10
            }
        }
    });

    if let Some(params) = profile.player_params {
        body["params"] = json!(params);
    }

    body
}

/// 从 youtube watch 页提取 visitorData (对齐 yt-dlp webpage bootstrap).
/// ANDROID_VR 无此字段会 LOGIN_REQUIRED; 缓存 30 分钟避免每首歌都打首页.
async fn fetch_visitor_data(http: &Client) -> AppResult<String> {
    if let Ok(guard) = VISITOR_DATA_CACHE.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.fetched_at.elapsed() < VISITOR_DATA_TTL && !cached.value.is_empty() {
                return Ok(cached.value.clone());
            }
        }
    }

    let html = http
        .get("https://www.youtube.com/watch?v=dQw4w9WgXcQ&bpctr=9999999999&has_verified=1")
        .header("User-Agent", WATCH_PAGE_UA)
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| AppError::Api(format!("youtube visitor page network: {e}")))?
        .text()
        .await
        .map_err(|e| AppError::Api(format!("youtube visitor page body: {e}")))?;

    let visitor = extract_visitor_data_from_html(&html).ok_or_else(|| {
        AppError::Api("youtube visitorData missing from watch page".into())
    })?;

    if let Ok(mut guard) = VISITOR_DATA_CACHE.lock() {
        *guard = Some(VisitorDataCache {
            value: visitor.clone(),
            fetched_at: Instant::now(),
        });
    }
    log::info!(
        target: "youtube-playback",
        "visitorData refreshed len={}",
        visitor.len()
    );
    Ok(visitor)
}

fn extract_visitor_data_from_html(html: &str) -> Option<String> {
    // "visitorData":"Cgt..."
    let key = "\"visitorData\":\"";
    let start = html.find(key)? + key.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query_map(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let key = percent_decode(parts.next().unwrap_or(""));
        let value = percent_decode(parts.next().unwrap_or(""));
        if !key.is_empty() {
            map.insert(key, value);
        }
    }
    map
}

/// 从 stream URL 的 `c=` 客户端参数推导拉流 User-Agent.
/// googlevideo CDN 校验拉流 UA 与铸造该直链的客户端一致, 否则 HTTP 403.
/// 对齐 Android resolveYouTubeStreamUserAgent: IOS/ANDROID/... 各用对应 app UA, 其余回退 Web.
pub fn stream_user_agent_for_url(url: &str) -> &'static str {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let client = parse_query_map(query)
        .get("c")
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_default();
    match client.as_str() {
        "IOS" => STREAM_IOS_USER_AGENT,
        "ANDROID" | "ANDROID_TESTSUITE" => STREAM_ANDROID_USER_AGENT,
        "ANDROID_MUSIC" => STREAM_ANDROID_MUSIC_USER_AGENT,
        "ANDROID_VR" => STREAM_ANDROID_VR_USER_AGENT,
        _ => STREAM_WEB_USER_AGENT,
    }
}

fn append_query_param(url: &str, key: &str, value: &str) -> String {
    let encoded_key = urlencoding::encode(key);
    let encoded_value = urlencoding::encode(value);
    if url.contains('?') {
        format!("{url}&{encoded_key}={encoded_value}")
    } else {
        format!("{url}?{encoded_key}={encoded_value}")
    }
}

/// 解析 format 的可播 URL
/// 支持 plain `url`, 以及已带 sig 的 signatureCipher;
/// 若仅有加密 `s` 且无解签器, 则跳过该 format (由其他客户端回退)
pub fn resolve_format_url(format: &Value) -> Option<String> {
    if let Some(url) = format.get("url").and_then(|v| v.as_str()).map(str::trim) {
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }

    let cipher = format
        .get("signatureCipher")
        .and_then(|v| v.as_str())
        .or_else(|| format.get("cipher").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    let params = parse_query_map(cipher);
    let url = params.get("url").map(String::as_str).unwrap_or("").trim();
    if url.is_empty() {
        return None;
    }

    if let Some(signature) = params
        .get("sig")
        .or_else(|| params.get("signature"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let sp = params
            .get("sp")
            .map(String::as_str)
            .unwrap_or("sig")
            .trim();
        return Some(append_query_param(url, sp, signature));
    }

    // 需要 player JS 解签的 `s=` 路径: 当前阶段跳过, 等待其它客户端给出 plain url
    if params.contains_key("s") {
        return None;
    }

    Some(url.to_string())
}

fn collect_format_arrays(resp: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(arr) = resp
        .pointer("/streamingData/adaptiveFormats")
        .and_then(|v| v.as_array())
    {
        out.extend(arr.iter().cloned());
    }
    // progressive formats 作为兜底 (部分 TV/IOS 客户端主要吐这里)
    if let Some(arr) = resp
        .pointer("/streamingData/formats")
        .and_then(|v| v.as_array())
    {
        out.extend(arr.iter().cloned());
    }
    out
}

fn is_audio_like(format: &Value) -> bool {
    let mime = format
        .get("mimeType")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if mime.starts_with("audio/") {
        return true;
    }
    // progressive 可能是 muxed, 仅在无独立 audio 时由调用方兜底
    false
}

fn format_to_stream(format: &Value) -> Option<YtAudioStream> {
    let url = resolve_format_url(format)?;
    Some(YtAudioStream {
        url,
        bitrate: format.get("bitrate").and_then(|v| v.as_u64()).unwrap_or(0),
        mime_type: format
            .get("mimeType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        content_length: format
            .get("contentLength")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| format.get("contentLength").and_then(|v| v.as_u64()))
            .unwrap_or(0),
    })
}

fn extract_audio_streams(resp: &Value) -> Vec<YtAudioStream> {
    let formats = collect_format_arrays(resp);

    let mut streams: Vec<YtAudioStream> = formats
        .iter()
        .filter(|f| is_audio_like(f))
        .filter_map(format_to_stream)
        .collect();

    // 若完全没有 audio/* 直链, 尝试 progressive muxed 里可解析 url 的条目
    // (少数 TV 响应只有 muxed; rodio/symphonia 可解常见容器中的音轨)
    if streams.is_empty() {
        streams = formats
            .iter()
            .filter(|f| {
                f.get("mimeType")
                    .and_then(|m| m.as_str())
                    .map(|m| m.starts_with("video/") || m.starts_with("audio/"))
                    .unwrap_or(false)
            })
            .filter_map(format_to_stream)
            .collect();
    }

    streams.sort_by(|a, b| {
        // 优先 audio/mp4 (AAC, symphonia 可解) > 其它 audio/* > muxed
        // webm/opus 当前未启 symphonia opus feature, 会 unsupported codec
        let a_score = mime_playback_score(&a.mime_type);
        let b_score = mime_playback_score(&b.mime_type);
        b_score
            .cmp(&a_score)
            .then(b.bitrate.cmp(&a.bitrate))
            .then(b.content_length.cmp(&a.content_length))
    });
    streams
}

fn mime_playback_score(mime: &str) -> u8 {
    let base = mime.split(';').next().unwrap_or(mime).trim().to_ascii_lowercase();
    if base.starts_with("audio/mp4") || base == "audio/m4a" || base == "audio/aac" {
        3
    } else if base.starts_with("audio/") {
        2
    } else if base.starts_with("video/") {
        1
    } else {
        0
    }
}

fn playability_summary(resp: &Value) -> (String, String, String) {
    let status = resp
        .pointer("/playabilityStatus/status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reason = resp
        .pointer("/playabilityStatus/reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let subreason = resp
        .pointer("/playabilityStatus/errorScreen/playerErrorMessageRenderer/subreason/simpleText")
        .and_then(|v| v.as_str())
        .or_else(|| {
            resp.pointer(
                "/playabilityStatus/errorScreen/playerErrorMessageRenderer/reason/simpleText",
            )
            .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    (status, reason, subreason)
}

fn playback_http_client() -> AppResult<Client> {
    Client::builder()
        // 默认 UA 仅作兜底; 实际请求按 profile 覆盖
        .user_agent("com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)")
        .no_proxy()
        // 不启用 cookie_store: 登录 Cookie 仅在 player 请求上显式附带,
        // CDN 拉流路径不会被 jar 自动污染
        .cookie_store(false)
        .build()
        .map_err(|e| AppError::Other(format!("build youtube playback client: {e}")))
}

fn build_cookie_header(auth: &YouTubeAuth) -> String {
    auth.cookies
        .iter()
        .filter(|c| !c.name.is_empty() && !c.value.is_empty())
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// player 登录 Cookie 头.
/// 注意: 当前 mobile/TV player 路径不要附带 SAPISID*HASH.
/// 2026-07 实测: IOS/ANDROID + Cookie 正常; 再加 Authorization 会 HTTP 400 INVALID_ARGUMENT.
/// WEB_REMIX 库接口仍使用完整 SAPISID*HASH (见 client.rs / account.rs).
fn player_cookie_header(auth: Option<&YouTubeAuth>) -> Option<String> {
    let auth = auth.filter(|a| a.has_login())?;
    let cookie_header = build_cookie_header(auth);
    if cookie_header.is_empty() {
        None
    } else {
        Some(cookie_header)
    }
}

async fn player_request(
    http: &Client,
    profile: &PlayerClientProfile,
    video_id: &str,
    auth: Option<&YouTubeAuth>,
    visitor_data: Option<&str>,
) -> AppResult<Value> {
    let body = build_player_body(profile, video_id, visitor_data);
    let (endpoint, origin) = player_endpoint(profile);
    let url = format!(
        "{endpoint}?prettyPrint=false&id={}&key={}",
        urlencoding::encode(video_id),
        DEFAULT_PLAYER_API_KEY
    );
    let cookie_header = player_cookie_header(auth);

    let mut req = http
        .post(&url)
        .header("User-Agent", profile.user_agent)
        .header("Content-Type", "application/json")
        .header("Origin", origin)
        .header("Referer", format!("{origin}/"))
        .header("X-YouTube-Client-Name", profile.client_id)
        .header("X-YouTube-Client-Version", profile.client_version)
        .header("X-Goog-Api-Format-Version", "2");

    // visitorData 同时放 body 与 header (yt-dlp generate_api_headers 同款)
    if let Some(vd) = visitor_data.map(str::trim).filter(|s| !s.is_empty()) {
        req = req.header("X-Goog-Visitor-Id", vd);
    }

    // 仅 Cookie: 让服务端识别登录会话/地区偏好; 不发 SAPISIDHASH 以免 mobile player 400
    if let Some(cookie) = cookie_header.as_deref() {
        req = req
            .header("Cookie", cookie)
            .header("X-Goog-AuthUser", "0");
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Api(format!("youtube player network: {e}")))?;

    let status = resp.status();
    let data: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Api(format!("youtube player json: {e}")))?;

    if !status.is_success() {
        let (ps, reason, _) = playability_summary(&data);
        let api_message = data
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(AppError::Api(format!(
            "youtube player http {}: client={} playability={} reason={} api={}",
            status.as_u16(),
            profile.client_name,
            ps,
            reason,
            api_message
        )));
    }
    Ok(data)
}

/// 多客户端解析可播音频流
/// `auth`: 已登录时传入, player 请求带 Cookie 以启用 Premium; CDN 拉流仍不带 Cookie
pub async fn resolve_audio_streams(
    video_id: &str,
    auth: Option<&YouTubeAuth>,
) -> AppResult<Vec<YtAudioStream>> {
    let video_id = video_id.trim();
    if video_id.is_empty() {
        return Err(AppError::Api("empty youtube video id".into()));
    }

    let http = playback_http_client()?;
    let mut errors = Vec::new();
    let logged_in = auth.map(YouTubeAuth::has_login).unwrap_or(false);
    log::info!(
        target: "youtube-playback",
        "resolve streams video_id={} logged_in={}",
        video_id,
        logged_in
    );

    // 任一客户端声明需要 visitorData 时预取; 失败不致命, 由该客户端自行报错回退
    let needs_visitor = playback_client_profiles()
        .iter()
        .any(|p| p.requires_visitor_data);
    let visitor_data = if needs_visitor {
        match fetch_visitor_data(&http).await {
            Ok(vd) => Some(vd),
            Err(err) => {
                log::warn!(
                    target: "youtube-playback",
                    "visitorData fetch failed: {}",
                    err
                );
                errors.push(format!("visitorData:{err}"));
                None
            }
        }
    } else {
        None
    };

    for profile in playback_client_profiles() {
        if profile.requires_visitor_data && visitor_data.is_none() {
            errors.push(format!("{}:missing_visitor_data", profile.client_name));
            continue;
        }
        let profile_visitor = if profile.requires_visitor_data {
            visitor_data.as_deref()
        } else {
            // IOS/ANDROID 不强制 visitor; 有则附带无害
            visitor_data.as_deref()
        };
        match player_request(&http, profile, video_id, auth, profile_visitor).await {
            Ok(resp) => {
                let (status, reason, subreason) = playability_summary(&resp);
                if status != "OK" {
                    log::warn!(
                        target: "youtube-playback",
                        "client={} status={} reason={} sub={} logged_in={}",
                        profile.client_name,
                        status,
                        reason,
                        subreason,
                        logged_in
                    );
                    errors.push(format!(
                        "{}:{}:{}:{}",
                        profile.client_name, status, reason, subreason
                    ));
                    continue;
                }

                let streams = extract_audio_streams(&resp);
                log::info!(
                    target: "youtube-playback",
                    "client={} version={} streams={} logged_in={}",
                    profile.client_name,
                    profile.client_version,
                    streams.len(),
                    logged_in
                );
                if !streams.is_empty() {
                    return Ok(streams);
                }
                errors.push(format!("{}:no_audio_url", profile.client_name));
            }
            Err(err) => {
                log::warn!(
                    target: "youtube-playback",
                    "client={} error={}",
                    profile.client_name,
                    err
                );
                errors.push(format!("{}:{}", profile.client_name, err));
            }
        }
    }

    Err(AppError::Api(format!(
        "YouTube playback failed for {video_id}: {}",
        errors.join(" | ")
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        append_query_param, build_player_body, collect_format_arrays, extract_audio_streams,
        extract_visitor_data_from_html, parse_query_map, playback_client_profiles,
        player_cookie_header, resolve_format_url, stream_user_agent_for_url, PlayerHost,
        STREAM_ANDROID_MUSIC_USER_AGENT, STREAM_ANDROID_USER_AGENT, STREAM_ANDROID_VR_USER_AGENT,
        STREAM_IOS_USER_AGENT, STREAM_WEB_USER_AGENT,
    };
    use serde_json::json;

    #[test]
    fn stream_ua_matches_client_param() {
        // googlevideo 直链 `c=` 客户端参数决定 CDN 允许的拉流 UA
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?c=IOS&id=1"),
            STREAM_IOS_USER_AGENT
        );
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?c=ANDROID&id=1"),
            STREAM_ANDROID_USER_AGENT
        );
        assert_eq!(
            stream_user_agent_for_url(
                "https://rr1.googlevideo.com/videoplayback?c=ANDROID_MUSIC&id=1"
            ),
            STREAM_ANDROID_MUSIC_USER_AGENT
        );
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?c=ANDROID_VR&id=1"),
            STREAM_ANDROID_VR_USER_AGENT
        );
        // 小写 / TVHTML5 / 缺失 c= 均回退 Web UA
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?c=ios&id=1"),
            STREAM_IOS_USER_AGENT
        );
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?c=TVHTML5&id=1"),
            STREAM_WEB_USER_AGENT
        );
        assert_eq!(
            stream_user_agent_for_url("https://rr1.googlevideo.com/videoplayback?id=1"),
            STREAM_WEB_USER_AGENT
        );
    }

    #[test]
    fn resolve_plain_url() {
        let format = json!({ "url": "https://googlevideo.com/videoplayback?id=1" });
        assert_eq!(
            resolve_format_url(&format).as_deref(),
            Some("https://googlevideo.com/videoplayback?id=1")
        );
    }

    #[test]
    fn resolve_signature_cipher_with_sig() {
        let cipher = "url=https%3A%2F%2Fgooglevideo.com%2Fvideoplayback%3Fid%3D1&sig=ABC123&sp=sig";
        let format = json!({ "signatureCipher": cipher });
        let url = resolve_format_url(&format).expect("url");
        assert!(url.starts_with("https://googlevideo.com/videoplayback?id=1"));
        assert!(url.contains("sig=ABC123"));
    }

    #[test]
    fn skip_encrypted_s_without_solver() {
        let cipher = "url=https%3A%2F%2Fgooglevideo.com%2Fvideoplayback&s=ENCRYPTED&sp=sig";
        let format = json!({ "signatureCipher": cipher });
        assert!(resolve_format_url(&format).is_none());
    }

    #[test]
    fn parse_query_and_append() {
        let map = parse_query_map("a=1&b=hello%20world");
        assert_eq!(map.get("b").map(String::as_str), Some("hello world"));
        let url = append_query_param("https://x.test/p", "sig", "a+b");
        assert_eq!(url, "https://x.test/p?sig=a%2Bb");
    }

    #[test]
    fn profiles_prefer_unciphered_clients_without_web_remix() {
        let profiles = playback_client_profiles();
        assert!(profiles.len() >= 4);
        // 主路径 ANDROID_VR (yt-dlp jsless) + visitorData; IOS/ANDROID 作回退
        assert_eq!(profiles[0].client_name, "ANDROID_VR");
        assert!(profiles[0].requires_visitor_data);
        assert!(profiles.iter().any(|p| p.client_name == "IOS"));
        assert!(profiles.iter().any(|p| p.client_name == "ANDROID"));
        assert!(profiles.iter().any(|p| p.client_name == "ANDROID_MUSIC"));
        // IOS/ANDROID 回退版本必须足够新, 否则 Innertube 直接 HTTP 400
        let ios = profiles.iter().find(|p| p.client_name == "IOS").unwrap();
        let android = profiles.iter().find(|p| p.client_name == "ANDROID").unwrap();
        assert!(ios.client_version.starts_with("20."));
        assert!(android.client_version.starts_with("20."));
        // 全部非 Music host, 避免和 WEB_REMIX 登录会话绑定到同一 player 端点
        assert!(profiles
            .iter()
            .all(|p| matches!(p.host, PlayerHost::Www)));
        // 不包含 WEB_REMIX player
        assert!(profiles.iter().all(|p| p.client_name != "WEB_REMIX"));
    }

    #[test]
    fn player_cookie_header_none_without_login() {
        assert!(player_cookie_header(None).is_none());
    }

    #[test]
    fn player_cookie_header_includes_cookie_when_logged_in() {
        use crate::auth::state::{CookieEntry, YouTubeAuth};
        let auth = YouTubeAuth {
            cookies: vec![
                CookieEntry {
                    name: "SAPISID".into(),
                    value: "sap-value".into(),
                    domain: ".youtube.com".into(),
                },
                CookieEntry {
                    name: "SID".into(),
                    value: "sid-value".into(),
                    domain: ".youtube.com".into(),
                },
            ],
            nickname: None,
            avatar_url: None,
        };
        let cookie = player_cookie_header(Some(&auth)).expect("cookie");
        assert!(cookie.contains("SAPISID=sap-value"));
        assert!(cookie.contains("SID=sid-value"));
        // mobile player 不再附带 SAPISIDHASH (见 player_request)
        assert!(!cookie.contains("SAPISIDHASH"));
    }

    #[test]
    fn android_music_body_includes_player_params() {
        let profile = playback_client_profiles()
            .iter()
            .find(|p| p.client_name == "ANDROID_MUSIC")
            .expect("android music profile");
        let body = build_player_body(profile, "dQw4w9WgXcQ", None);
        assert_eq!(body["videoId"], "dQw4w9WgXcQ");
        assert_eq!(body["params"], ANDROID_MUSIC_PLAYER_PARAMS_CONST);
        assert_eq!(
            body["playbackContext"]["contentPlaybackContext"]["html5Preference"],
            "HTML5_PREF_WANTS"
        );
    }

    #[test]
    fn android_vr_body_includes_visitor_data() {
        let profile = playback_client_profiles()
            .iter()
            .find(|p| p.client_name == "ANDROID_VR")
            .expect("android vr profile");
        let body = build_player_body(profile, "dQw4w9WgXcQ", Some("CgtVisitorTest"));
        assert_eq!(body["context"]["client"]["visitorData"], "CgtVisitorTest");
        assert_eq!(body["context"]["client"]["clientName"], "ANDROID_VR");
    }

    #[test]
    fn extract_visitor_data_from_watch_html() {
        let html = r#"{"responseContext":{"visitorData":"CgtABC123xyz"}}"#;
        assert_eq!(
            extract_visitor_data_from_html(html).as_deref(),
            Some("CgtABC123xyz")
        );
        assert!(extract_visitor_data_from_html("no visitor here").is_none());
    }

    // 测试用常量镜像, 避免 pub(crate) 泄漏
    const ANDROID_MUSIC_PLAYER_PARAMS_CONST: &str = "CgIIAdgDAQ==";

    #[test]
    fn extract_prefers_audio_over_muxed() {
        let resp = json!({
            "streamingData": {
                "adaptiveFormats": [
                    {
                        "mimeType": "audio/mp4",
                        "bitrate": 128000,
                        "url": "https://googlevideo.com/a",
                        "contentLength": "100"
                    }
                ],
                "formats": [
                    {
                        "mimeType": "video/mp4",
                        "bitrate": 500000,
                        "url": "https://googlevideo.com/v",
                        "contentLength": "900"
                    }
                ]
            }
        });
        let streams = extract_audio_streams(&resp);
        assert_eq!(streams.len(), 1);
        assert!(streams[0].url.ends_with("/a"));
        assert!(streams[0].mime_type.starts_with("audio/"));
    }

    #[test]
    fn extract_prefers_m4a_over_webm_opus() {
        // symphonia 未启 opus: 同码率下必须优先 AAC/mp4, 否则解码 unsupported codec
        let resp = json!({
            "streamingData": {
                "adaptiveFormats": [
                    {
                        "mimeType": "audio/webm; codecs=\"opus\"",
                        "bitrate": 160000,
                        "url": "https://googlevideo.com/opus",
                        "contentLength": "200"
                    },
                    {
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 128000,
                        "url": "https://googlevideo.com/aac",
                        "contentLength": "180"
                    }
                ]
            }
        });
        let streams = extract_audio_streams(&resp);
        assert_eq!(streams.len(), 2);
        assert!(streams[0].url.ends_with("/aac"));
        assert!(streams[0].mime_type.contains("mp4"));
    }

    #[test]
    fn extract_falls_back_to_progressive_when_no_audio() {
        let resp = json!({
            "streamingData": {
                "formats": [
                    {
                        "mimeType": "video/mp4",
                        "bitrate": 500000,
                        "url": "https://googlevideo.com/v",
                        "contentLength": "900"
                    }
                ]
            }
        });
        let streams = extract_audio_streams(&resp);
        assert_eq!(streams.len(), 1);
        assert!(streams[0].url.ends_with("/v"));
    }

    #[test]
    fn collect_merges_adaptive_and_progressive() {
        let resp = json!({
            "streamingData": {
                "adaptiveFormats": [{ "url": "a" }],
                "formats": [{ "url": "b" }]
            }
        });
        let all = collect_format_arrays(&resp);
        assert_eq!(all.len(), 2);
    }
}
