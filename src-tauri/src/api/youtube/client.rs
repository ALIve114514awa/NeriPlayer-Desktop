// YouTube Music InnerTube API 客户端
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use parking_lot::Mutex;

use crate::error::{AppError, AppResult};
use crate::api::transport::FallbackHttp;

pub use super::account::YouTubeAccountProfile;

const INNERTUBE_URL: &str = "https://music.youtube.com/youtubei/v1";
pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

// 默认 API key（可能随时变化，需要从页面 bootstrap 获取）
const DEFAULT_API_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const DEFAULT_CLIENT_VERSION: &str = "1.20250415.01.00";

pub struct YouTubeClient {
    http: FallbackHttp,
    api_key: Mutex<String>,
    client_version: Mutex<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtSearchResult {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YtAudioStream {
    pub url: String,
    pub bitrate: u64,
    pub mime_type: String,
    pub content_length: u64,
}

impl YouTubeClient {
    pub fn new(http: &Client) -> Self {
        Self::with_transport(FallbackHttp::new(http, "youtube"))
    }

    pub fn with_transport(http: FallbackHttp) -> Self {
        Self {
            http,
            api_key: Mutex::new(DEFAULT_API_KEY.to_string()),
            client_version: Mutex::new(DEFAULT_CLIENT_VERSION.to_string()),
        }
    }

    /// 底层传输通道，供本模块内的自由函数复用同一套代理兜底
    pub fn transport(&self) -> &FallbackHttp {
        &self.http
    }

    /// 构建 InnerTube context
    fn build_context(&self) -> Value {
        let version = self.client_version.lock().clone();
        let (hl, gl) = super::innertube_locale();
        json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": version,
                "hl": hl,
                "gl": gl,
                "platform": "DESKTOP",
                "userAgent": USER_AGENT,
                "utcOffsetMinutes": 480
            },
            "user": { "lockedSafetyMode": false }
        })
    }

    /// InnerTube POST 请求
    async fn innertube_post(&self, endpoint: &str, body: &Value) -> AppResult<Value> {
        let api_key = self.api_key.lock().clone();
        let url = format!("{}/{}?prettyPrint=false&key={}", INNERTUBE_URL, endpoint, api_key);

        let resp = self
            .http
            .send(|client| {
                client
                    .post(&url)
                    .header("User-Agent", USER_AGENT)
                    .header("Content-Type", "application/json")
                    .header("Origin", "https://music.youtube.com")
                    .header("Referer", "https://music.youtube.com/")
                    .header("X-YouTube-Client-Name", "67")
                    .json(body)
            })
            .await?;

        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// 搜索音乐
    pub async fn search(&self, query: &str) -> AppResult<Value> {
        let body = json!({
            "context": self.build_context(),
            "query": query,
            "params": "EgWKAQIIAWoMEA4QChADEAQQCRAF"  // 搜索歌曲过滤器
        });

        self.innertube_post("search", &body).await
    }

    /// 获取音频流 (兼容入口: 委托 playback; 无 auth 时仅 guest 路径)
    /// 正式播放请走 commands 注入 YouTubeAuth, 以便 Premium 生效
    pub async fn get_streams(&self, video_id: &str) -> AppResult<Vec<YtAudioStream>> {
        let _ = self;
        super::playback::resolve_audio_streams(video_id, None).await
    }

    /// 获取歌词（通过 next endpoint）
    pub async fn get_lyrics(&self, video_id: &str) -> AppResult<Option<String>> {
        let body = json!({
            "context": self.build_context(),
            "videoId": video_id,
            "isAudioOnly": true
        });

        let resp = self.innertube_post("next", &body).await?;

        // 歌词在 tabs 中
        let tabs = resp["contents"]["singleColumnMusicWatchNextResultsRenderer"]
            ["tabbedRenderer"]["watchNextTabbedResultsRenderer"]["tabs"]
            .as_array();

        if let Some(tabs) = tabs {
            for tab in tabs {
                let endpoint = &tab["tabRenderer"]["endpoint"];
                if let Some(browse_id) = endpoint["browseEndpoint"]["browseId"].as_str() {
                    if browse_id.starts_with("MPLYt_") {
                        // 获取歌词内容
                        let lyrics_body = json!({
                            "context": self.build_context(),
                            "browseId": browse_id
                        });
                        let lyrics_resp = self.innertube_post("browse", &lyrics_body).await?;
                        let text = lyrics_resp["contents"]["sectionListRenderer"]["contents"]
                            [0]["musicDescriptionShelfRenderer"]["description"]["runs"]
                            [0]["text"].as_str();
                        return Ok(text.map(String::from));
                    }
                }
            }
        }

        Ok(None)
    }

    // 需要登录的 API
    /// 认证版 InnerTube POST: 使用完整 SAPISID*HASH (对齐 Android buildYouTubeInnertubeRequestHeaders)
    async fn innertube_post_auth(
        &self,
        endpoint: &str,
        body: &Value,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let (data, _) = self.innertube_post_auth_with_session(endpoint, body, auth).await?;
        Ok(data)
    }

    /// 构建 Cookie 头字符串
    fn build_cookie_header(cookies: &[crate::auth::state::CookieEntry]) -> String {
        cookies
            .iter()
            .filter(|c| !c.name.is_empty() && !c.value.is_empty())
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn cookie_map(auth: &crate::auth::state::YouTubeAuth) -> std::collections::BTreeMap<String, String> {
        let mut map = std::collections::BTreeMap::new();
        for cookie in &auth.cookies {
            if cookie.name.is_empty() || cookie.value.is_empty() {
                continue;
            }
            map.insert(cookie.name.clone(), cookie.value.clone());
        }
        map
    }

    fn authorization_header(
        auth: &crate::auth::state::YouTubeAuth,
        user_session_id: &str,
    ) -> AppResult<String> {
        let cookies = Self::cookie_map(auth);
        crate::auth::youtube_hash::build_youtube_authorization(
            cookies.get("SAPISID").map(String::as_str),
            cookies.get("__Secure-1PAPISID").map(String::as_str),
            cookies.get("__Secure-3PAPISID").map(String::as_str),
            "https://music.youtube.com",
            user_session_id,
        )
        .ok_or_else(|| AppError::Api("No SAPISID for YouTube auth".into()))
    }

    /// 获取当前 YouTube Music 账号资料
    pub async fn get_account_profile(
        &self,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<YouTubeAccountProfile> {
        super::account::get_account_profile(&self.http, auth).await
    }

    /// YouTube Music 首页信息流（需登录）
    pub async fn get_home_feed(
        &self,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let body = json!({
            "context": self.build_context(),
            "browseId": "FEmusic_home"
        });
        self.innertube_post_auth("browse", &body, auth).await
    }

    /// YouTube Music 用户音乐库歌单（需登录）
    pub async fn get_library_playlists(
        &self,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let body = json!({
            "context": self.build_context(),
            "browseId": "FEmusic_liked_playlists"
        });
        let data = self.innertube_post_auth("browse", &body, auth).await?;
        // 诊断用：区分「请求没发出」「返回了但目录为空」「返回有内容但前端解析不出」
        log::info!(
            target: "youtube",
            "library playlists response: shelves={}, has_contents={}, node_types=[{}]",
            count_browse_items(&data),
            data.get("contents").is_some(),
            summarize_node_types(&data),
        );
        Ok(data)
    }

    /// YouTube Music 歌单详情（需登录）
    pub async fn get_playlist_detail(
        &self,
        browse_id: &str,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let body = json!({
            "context": self.build_context(),
            "browseId": browse_id
        });
        self.innertube_post_auth("browse", &body, auth).await
    }

    /// 认证版 InnerTube POST, 并回收 Set-Cookie 用于会话保鲜
    /// 采用轻量 InnerTube + Cookie 方案, 不模拟完整浏览器环境, 避免与移动端会话互相挤掉登录
    async fn innertube_post_auth_with_session(
        &self,
        endpoint: &str,
        body: &Value,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<(Value, Option<crate::auth::state::YouTubeAuth>)> {
        if !auth.has_login() {
            return Err(AppError::Api("YouTube not logged in".into()));
        }
        let cookie_header = Self::build_cookie_header(&auth.cookies);

        // 必须用 bootstrap 而不是硬编码常量：InnerTube 对过期的 clientVersion
        // 往往返回 HTTP 200 + 空目录而不是报错，表现就是「登录了但没有云端歌单」。
        // 同时它提供 visitorData / sessionIndex / userSessionId，Android 也是这么做的。
        let bootstrap = super::account::cached_bootstrap(&self.http, auth).await?;
        let url = format!(
            "{}/{}?prettyPrint=false&key={}",
            INNERTUBE_URL, endpoint, bootstrap.api_key
        );
        let auth_header = Self::authorization_header(auth, &bootstrap.user_session_id)?;

        // 用 bootstrap 的 context 覆盖调用方拼的那份
        let mut body = body.clone();
        body["context"] = bootstrap.context();
        let body = &body;

        let resp = self
            .http
            .send(|client| {
                client
                    .post(&url)
                    .header("User-Agent", USER_AGENT)
                    .header("Content-Type", "application/json")
                    .header("Origin", "https://music.youtube.com")
                    .header("X-Origin", "https://music.youtube.com")
                    .header("Referer", "https://music.youtube.com/")
                    .header("X-YouTube-Client-Name", "67")
                    .header("X-YouTube-Client-Version", &bootstrap.client_version)
                    .header("X-Goog-Visitor-Id", &bootstrap.visitor_data)
                    .header("Authorization", &auth_header)
                    .header("X-Goog-AuthUser", &bootstrap.session_index)
                    .header("Cookie", &cookie_header)
                    .json(body)
            })
            .await?;

        let set_cookie: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_string))
            .collect();

        // 必须先看状态码：401/403 的错误体同样是 JSON，直接 json() 解析会得到
        // 一个"合法但没有内容"的响应，前端只会渲染成空列表，整条链路不报错
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            super::account::invalidate_bootstrap_cache();
            let detail = innertube_error_detail(&body);
            log::warn!(
                target: "youtube",
                "innertube {} failed: HTTP {} {}",
                endpoint,
                status,
                detail,
            );
            return Err(AppError::Api(format!(
                "YouTube request failed: HTTP {status}{}",
                if detail.is_empty() { String::new() } else { format!(" - {detail}") }
            )));
        }
        let data: Value = serde_json::from_str(&body).map_err(|error| {
            AppError::Api(format!("YouTube response is not valid JSON: {error}"))
        })?;
        if let Some(reason) = innertube_rejection_reason(&data) {
            // 被拒往往意味着 bootstrap 过期，丢掉缓存让下次重新抓
            super::account::invalidate_bootstrap_cache();
            log::warn!(target: "youtube", "innertube {} rejected: {}", endpoint, reason);
            return Err(AppError::Api(format!("YouTube request rejected: {reason}")));
        }

        // 若响应携带新的身份 cookie, 合并回会话(保留旧身份, 避免被短暂响应冲掉)
        let updated = if set_cookie.is_empty() {
            None
        } else {
            let observed = super::session::parse_set_cookie_headers(&set_cookie);
            if observed.is_empty() {
                None
            } else {
                let merged = super::session::merge_youtube_auth_cookies(auth, &observed);
                if super::session::youtube_auth_changed(auth, &merged) {
                    Some(merged)
                } else {
                    None
                }
            }
        };

        Ok((data, updated))
    }

    /// 歌单详情(携带会话刷新)
    pub async fn get_playlist_detail_with_session(
        &self,
        browse_id: &str,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<(Value, Option<crate::auth::state::YouTubeAuth>)> {
        let body = json!({
            "context": self.build_context(),
            "browseId": browse_id
        });
        self.innertube_post_auth_with_session("browse", &body, auth).await
    }

    /// 歌单分页 continuation(携带会话刷新)
    pub async fn continue_playlist_with_session(
        &self,
        continuation: &str,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<(Value, Option<crate::auth::state::YouTubeAuth>)> {
        let body = json!({
            "context": self.build_context(),
            "continuation": continuation
        });
        self.innertube_post_auth_with_session("browse", &body, auth).await
    }
}

/// 从 InnerTube 错误体里提取可读原因，截断避免把整页 HTML 灌进日志
fn innertube_error_detail(body: &str) -> String {
    const MAX: usize = 200;
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/error/status"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .unwrap_or_else(|| body.split_whitespace().collect::<Vec<_>>().join(" "));
    if message.chars().count() <= MAX {
        return message;
    }
    message.chars().take(MAX).collect::<String>() + "…"
}

/// 状态码 200 但内容被拒（登录失效、地区不可用等）时的原因
///
/// InnerTube 这类拒绝同样返回 200，不识别就会静默变成空列表。
fn innertube_rejection_reason(data: &Value) -> Option<String> {
    if let Some(message) = data
        .pointer("/error/message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
    {
        return Some(message.to_string());
    }
    let status = data
        .pointer("/responseContext/mainAppWebResponseContext/loggedOut")
        .and_then(Value::as_bool);
    if status == Some(true) {
        return Some("YouTube session is signed out".into());
    }
    None
}

/// 粗略统计 browse 响应里的条目数，仅用于诊断
fn count_browse_items(data: &Value) -> usize {
    fn walk(value: &Value, count: &mut usize) {
        match value {
            Value::Object(map) => {
                if map.contains_key("musicTwoRowItemRenderer")
                    || map.contains_key("musicResponsiveListItemRenderer")
                {
                    *count += 1;
                }
                for nested in map.values() {
                    walk(nested, count);
                }
            }
            Value::Array(items) => {
                for nested in items {
                    walk(nested, count);
                }
            }
            _ => {}
        }
    }
    let mut count = 0;
    walk(data, &mut count);
    count
}

/// 列出响应里出现的渲染器 / ViewModel 类型及数量
///
/// YouTube 会不打招呼地改渲染结构（例如从 *Renderer 迁到 *ViewModel），
/// 解析器认不出新类型时表现就是"请求成功但列表为空"。
/// 把真实类型名打出来，下次结构再变可以一眼定位，不用靠猜。
fn summarize_node_types(data: &Value) -> String {
    use std::collections::BTreeMap;

    fn walk(value: &Value, counts: &mut BTreeMap<String, usize>) {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    if key.ends_with("Renderer") || key.ends_with("ViewModel") {
                        *counts.entry(key.clone()).or_default() += 1;
                    }
                    walk(nested, counts);
                }
            }
            Value::Array(items) => items.iter().for_each(|nested| walk(nested, counts)),
            _ => {}
        }
    }

    let mut counts = BTreeMap::new();
    walk(data, &mut counts);
    let mut ordered: Vec<_> = counts.into_iter().collect();
    ordered.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ordered
        .into_iter()
        .take(12)
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}
