use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::task::JoinHandle;

use crate::audio::player::PlayerEngine;
use crate::audio::queue::PlayQueue;
use crate::auth::state::AuthState;
use crate::listen_together::session::LtSession;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub struct DownloadTaskControl {
    pub cancel_flag: Arc<AtomicBool>,
    pub handle: JoinHandle<()>,
}

/// 全局应用状态，通过 tauri::State 注入
pub struct AppState {
    pub player: Mutex<PlayerEngine>,
    pub queue: Mutex<PlayQueue>,
    pub http: parking_lot::RwLock<reqwest::Client>,
    /// 共享 Cookie Jar — 允许外部注入持久化登录 Cookie
    pub cookie_jar: Arc<reqwest::cookie::Jar>,
    /// 三平台登录状态
    pub auth: Mutex<AuthState>,
    /// 一起听会话
    pub lt_session: Mutex<LtSession>,
    /// 后台下载任务
    pub download_tasks: Mutex<HashMap<String, DownloadTaskControl>>,
}

impl AppState {
    pub fn new() -> Self {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::Client::builder()
            .cookie_provider(jar.clone())
            .user_agent(USER_AGENT)
            .no_proxy()
            .build()
            .expect("Failed to create HTTP client");

        Self {
            player: Mutex::new(PlayerEngine::new()),
            queue: Mutex::new(PlayQueue::new()),
            http: parking_lot::RwLock::new(http),
            cookie_jar: jar,
            auth: Mutex::new(AuthState::default()),
            lt_session: Mutex::new(LtSession::new()),
            download_tasks: Mutex::new(HashMap::new()),
        }
    }

    /// 重建 HTTP Client，切换代理模式
    pub fn rebuild_http(&self, bypass_proxy: bool) {
        let mut builder = reqwest::Client::builder()
            .cookie_provider(self.cookie_jar.clone())
            .user_agent(USER_AGENT);
        if bypass_proxy {
            builder = builder.no_proxy();
        }
        if let Ok(client) = builder.build() {
            *self.http.write() = client;
        }
    }

    /// 获取当前 HTTP Client 的克隆（O(1)，reqwest::Client 内部是 Arc）
    pub fn http(&self) -> reqwest::Client {
        self.http.read().clone()
    }
}

/// 曲目信息（前后端共享）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub source: TrackSource,
    /// 本地文件路径或远程 URL
    pub url: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TrackSource {
    Local,
    Netease,
    Qq,
    Bilibili,
    Youtube,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    Off,
    All,
    One,
}
