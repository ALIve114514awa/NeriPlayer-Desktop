// YouTube Music InnerTube API 客户端
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use parking_lot::Mutex;

use crate::error::{AppError, AppResult};

pub use super::account::YouTubeAccountProfile;

const INNERTUBE_URL: &str = "https://music.youtube.com/youtubei/v1";
pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

// 默认 API key（可能随时变化，需要从页面 bootstrap 获取）
const DEFAULT_API_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const DEFAULT_CLIENT_VERSION: &str = "1.20250415.01.00";

pub struct YouTubeClient {
    http: Client,
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
        Self {
            http: http.clone(),
            api_key: Mutex::new(DEFAULT_API_KEY.to_string()),
            client_version: Mutex::new(DEFAULT_CLIENT_VERSION.to_string()),
        }
    }

    /// 构建 InnerTube context
    fn build_context(&self) -> Value {
        let version = self.client_version.lock().clone();
        json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": version,
                "hl": "zh-CN",
                "gl": "JP",
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

        let resp = self.http
            .post(&url)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/json")
            .header("Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("X-YouTube-Client-Name", "67")
            .json(body)
            .send().await?;

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

    /// 获取音频流
    pub async fn get_streams(&self, video_id: &str) -> AppResult<Vec<YtAudioStream>> {
        let body = json!({
            "context": self.build_context(),
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true
        });

        let resp = self.innertube_post("player", &body).await?;

        let status = resp["playabilityStatus"]["status"].as_str().unwrap_or("");
        if status != "OK" {
            return Err(AppError::Api(format!("YouTube playback error: {}", status)));
        }

        let formats = resp["streamingData"]["adaptiveFormats"].as_array()
            .ok_or_else(|| AppError::Api("No adaptive formats".into()))?;

        let mut streams: Vec<YtAudioStream> = formats.iter()
            .filter(|f| {
                f["mimeType"].as_str()
                    .map(|m| m.starts_with("audio/"))
                    .unwrap_or(false)
            })
            .filter_map(|f| {
                let url = f["url"].as_str()?;
                Some(YtAudioStream {
                    url: url.to_string(),
                    bitrate: f["bitrate"].as_u64().unwrap_or(0),
                    mime_type: f["mimeType"].as_str().unwrap_or("").to_string(),
                    content_length: f["contentLength"].as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                })
            })
            .collect();

        // 按码率降序
        streams.sort_by(|a, b| b.bitrate.cmp(&a.bitrate));
        Ok(streams)
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

    // ===== 需要登录的 API =====

    /// 认证版 InnerTube POST — 附加 SAPISIDHASH + Cookie 头
    async fn innertube_post_auth(
        &self,
        endpoint: &str,
        body: &Value,
        sapisid: &str,
        cookie_header: &str,
    ) -> AppResult<Value> {
        let api_key = self.api_key.lock().clone();
        let url = format!("{}/{}?prettyPrint=false&key={}", INNERTUBE_URL, endpoint, api_key);

        let auth_header = crate::auth::youtube_hash::compute_sapisidhash(
            sapisid, "https://music.youtube.com",
        );

        let resp = self.http
            .post(&url)
            .header("User-Agent", USER_AGENT)
            .header("Content-Type", "application/json")
            .header("Origin", "https://music.youtube.com")
            .header("X-Origin", "https://music.youtube.com")
            .header("Referer", "https://music.youtube.com/")
            .header("X-YouTube-Client-Name", "67")
            .header("Authorization", auth_header)
            .header("X-Goog-AuthUser", "0")
            .header("Cookie", cookie_header)
            .json(body)
            .send().await?;

        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// 构建 Cookie 头字符串
    fn build_cookie_header(cookies: &[crate::auth::state::CookieEntry]) -> String {
        cookies.iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
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
        let sapisid = auth.get_sapisid()
            .ok_or_else(|| AppError::Api("No SAPISID for YouTube auth".into()))?;
        let cookie_header = Self::build_cookie_header(&auth.cookies);

        let body = json!({
            "context": self.build_context(),
            "browseId": "FEmusic_home"
        });

        self.innertube_post_auth("browse", &body, sapisid, &cookie_header).await
    }

    /// YouTube Music 用户音乐库歌单（需登录）
    pub async fn get_library_playlists(
        &self,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let sapisid = auth.get_sapisid()
            .ok_or_else(|| AppError::Api("No SAPISID for YouTube auth".into()))?;
        let cookie_header = Self::build_cookie_header(&auth.cookies);

        let body = json!({
            "context": self.build_context(),
            "browseId": "FEmusic_liked_playlists"
        });

        self.innertube_post_auth("browse", &body, sapisid, &cookie_header).await
    }

    /// YouTube Music 歌单详情（需登录）
    pub async fn get_playlist_detail(
        &self,
        browse_id: &str,
        auth: &crate::auth::state::YouTubeAuth,
    ) -> AppResult<Value> {
        let sapisid = auth.get_sapisid()
            .ok_or_else(|| AppError::Api("No SAPISID for YouTube auth".into()))?;
        let cookie_header = Self::build_cookie_header(&auth.cookies);

        let body = json!({
            "context": self.build_context(),
            "browseId": browse_id
        });

        self.innertube_post_auth("browse", &body, sapisid, &cookie_header).await
    }
}
