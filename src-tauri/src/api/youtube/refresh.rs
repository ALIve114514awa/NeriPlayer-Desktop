// YouTube 登录会话主动保鲜: 轻量 InnerTube + Cookie 方案
// 通过加载 music/www 首页回收轮换 Cookie(SIDCC / __Secure-*SIDTS 等), 避免长期空闲掉登录;
// 不模拟完整浏览器, 因此不会与移动端同账号互相挤掉登录(对齐 Android YouTubeAuthAutoRefreshManager)
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Client;

use crate::auth::state::YouTubeAuth;
use crate::error::{AppError, AppResult};

use super::client::USER_AGENT;
use super::session;

// 常规保鲜冷却: 15 分钟
pub const REFRESH_COOLDOWN_MS: u64 = 15 * 60 * 1000;
// 强制保鲜退避: 90 秒, 用于 401/403 等恢复路径避免请求风暴
pub const FORCE_REFRESH_BACKOFF_MS: u64 = 90_000;
// 连续失败阈值, 达到即触发熔断
pub const MAX_CONSECUTIVE_FAILURES: u32 = 2;
// 熔断持续时间: 30 分钟
pub const CIRCUIT_BREAK_MS: u64 = 30 * 60 * 1000;

const MUSIC_HOME_URL: &str = "https://music.youtube.com/";
const WWW_REFRESH_URL: &str = "https://www.youtube.com/?themeRefresh=1";

/// 会话保鲜闸门: 冷却窗口 + 熔断状态机(纯逻辑, 便于单测与并发去重)
#[derive(Debug, Default)]
pub struct YouTubeRefreshGate {
    last_attempt_ms: Option<u64>,
    last_success_ms: Option<u64>,
    consecutive_failures: u32,
    circuit_open_until_ms: Option<u64>,
}

impl YouTubeRefreshGate {
    /// 是否允许发起一次保鲜: 未登录不刷; 熔断期一律不刷(强制也不例外); 其余按冷却窗口
    pub fn should_attempt(&self, now_ms: u64, force: bool, has_login: bool) -> bool {
        if !has_login {
            return false;
        }
        if let Some(until) = self.circuit_open_until_ms {
            if now_ms < until {
                return false;
            }
        }
        match self.last_attempt_ms {
            None => true,
            Some(last) => {
                let cooldown = if force {
                    FORCE_REFRESH_BACKOFF_MS
                } else {
                    REFRESH_COOLDOWN_MS
                };
                now_ms.saturating_sub(last) >= cooldown
            }
        }
    }

    /// 决定发起后立即记录, 使冷却窗口对并发调用生效
    pub fn record_attempt(&mut self, now_ms: u64) {
        self.last_attempt_ms = Some(now_ms);
    }

    /// 成功后清零失败计数并解除熔断
    pub fn record_success(&mut self, now_ms: u64) {
        self.last_success_ms = Some(now_ms);
        self.consecutive_failures = 0;
        self.circuit_open_until_ms = None;
    }

    /// 失败累计, 达到阈值则打开熔断窗口
    pub fn record_failure(&mut self, now_ms: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            self.circuit_open_until_ms = Some(now_ms.saturating_add(CIRCUIT_BREAK_MS));
        }
    }

    pub fn last_success_ms(&self) -> Option<u64> {
        self.last_success_ms
    }
}

/// 当前 Unix 毫秒时间
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn build_cookie_header(auth: &YouTubeAuth) -> String {
    auth.cookies
        .iter()
        .filter(|c| !c.name.is_empty() && !c.value.is_empty())
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 解析页面登录信号: Some(true)=已登录, Some(false)=游客, None=无法判断
pub fn html_session_logged_in(html: &str) -> Option<bool> {
    if html.contains("\"LOGGED_IN\":true") || html.contains("\"LOGGED_IN\": true") {
        return Some(true);
    }
    // 存在有效会话 id 同样视为已登录
    if html.contains("\"DELEGATED_SESSION_ID\"") || html.contains("\"USER_SESSION_ID\":\"") {
        return Some(true);
    }
    if html.contains("\"LOGGED_IN\":false") || html.contains("\"LOGGED_IN\": false") {
        return Some(false);
    }
    None
}

async fn fetch_with_cookies(
    http: &Client,
    url: &str,
    cookie_header: &str,
) -> AppResult<(Vec<String>, String)> {
    let resp = http
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Cookie", cookie_header)
        .send()
        .await
        .map_err(|e| AppError::Api(format!("youtube refresh network: {e}")))?;

    let set_cookie: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let body = resp.text().await.unwrap_or_default();
    Ok((set_cookie, body))
}

/// 主动保鲜一次: 加载 music/www 首页, 回收 Set-Cookie 并按身份安全合并
/// 返回 Some(新会话)=cookie 有更新且身份一致; None=无变化;
/// Err=网络失败或明确游客态(交由调用方计入失败, 触发熔断; 但绝不清除本地登录)
pub async fn refresh_youtube_session(
    http: &Client,
    auth: &YouTubeAuth,
) -> AppResult<Option<YouTubeAuth>> {
    if !auth.has_login() {
        return Ok(None);
    }
    let cookie_header = build_cookie_header(auth);

    let mut observed = Vec::new();
    let mut saw_logged_in = false;
    let mut saw_guest = false;
    let mut network_ok = false;

    for url in [MUSIC_HOME_URL, WWW_REFRESH_URL] {
        match fetch_with_cookies(http, url, &cookie_header).await {
            Ok((set_cookie, body)) => {
                network_ok = true;
                observed.extend(session::parse_set_cookie_headers(&set_cookie));
                match html_session_logged_in(&body) {
                    Some(true) => saw_logged_in = true,
                    Some(false) => saw_guest = true,
                    None => {}
                }
            }
            Err(e) => {
                log::warn!(target: "youtube-refresh", "load {url} failed: {e}");
            }
        }
    }

    if !network_ok {
        return Err(AppError::Api("youtube refresh network unreachable".into()));
    }

    // 明确游客态且无任何已登录信号: 拒绝合并, 避免用游客 cookie 冲掉完整登录
    if saw_guest && !saw_logged_in {
        return Err(AppError::Api("youtube refresh saw a signed-out session".into()));
    }

    if observed.is_empty() {
        return Ok(None);
    }

    let merged = session::merge_youtube_auth_cookies(auth, &observed);
    if session::youtube_auth_changed(auth, &merged) {
        Ok(Some(merged))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60 * 1000;

    #[test]
    fn skips_when_not_logged_in() {
        let gate = YouTubeRefreshGate::default();
        assert!(!gate.should_attempt(0, false, false));
        assert!(!gate.should_attempt(0, true, false));
    }

    #[test]
    fn first_attempt_allowed_then_cooldown() {
        let mut gate = YouTubeRefreshGate::default();
        assert!(gate.should_attempt(0, false, true));
        gate.record_attempt(0);
        // 冷却窗口内被拒
        assert!(!gate.should_attempt(MIN, false, true));
        assert!(!gate.should_attempt(REFRESH_COOLDOWN_MS - 1, false, true));
        // 冷却到期后放行
        assert!(gate.should_attempt(REFRESH_COOLDOWN_MS, false, true));
    }

    #[test]
    fn force_uses_shorter_backoff() {
        let mut gate = YouTubeRefreshGate::default();
        gate.record_attempt(0);
        // 常规冷却仍在, 但强制退避已过
        assert!(!gate.should_attempt(FORCE_REFRESH_BACKOFF_MS, false, true));
        assert!(gate.should_attempt(FORCE_REFRESH_BACKOFF_MS, true, true));
        assert!(!gate.should_attempt(FORCE_REFRESH_BACKOFF_MS - 1, true, true));
    }

    #[test]
    fn circuit_breaker_opens_after_threshold_failures() {
        let mut gate = YouTubeRefreshGate::default();
        gate.record_attempt(0);
        gate.record_failure(0);
        // 未达阈值仍可按冷却重试
        assert!(gate.should_attempt(REFRESH_COOLDOWN_MS, false, true));
        gate.record_attempt(REFRESH_COOLDOWN_MS);
        gate.record_failure(REFRESH_COOLDOWN_MS);
        // 达阈值后熔断: 即便强制也不放行
        let after = REFRESH_COOLDOWN_MS + 1;
        assert!(!gate.should_attempt(after, false, true));
        assert!(!gate.should_attempt(after, true, true));
        // 熔断到期后恢复
        let recovered = REFRESH_COOLDOWN_MS + CIRCUIT_BREAK_MS;
        assert!(gate.should_attempt(recovered, true, true));
    }

    #[test]
    fn success_resets_failures_and_circuit() {
        let mut gate = YouTubeRefreshGate::default();
        gate.record_failure(0);
        gate.record_failure(0);
        assert!(!gate.should_attempt(1, true, true));
        gate.record_success(CIRCUIT_BREAK_MS + 1);
        // 成功清零后, 只受冷却约束
        assert!(gate.should_attempt(CIRCUIT_BREAK_MS + 1 + REFRESH_COOLDOWN_MS, false, true));
    }

    #[test]
    fn logged_in_signal_detection() {
        assert_eq!(html_session_logged_in(r#"ytcfg.set({"LOGGED_IN":true});"#), Some(true));
        assert_eq!(html_session_logged_in(r#"{"LOGGED_IN": true}"#), Some(true));
        assert_eq!(html_session_logged_in(r#"{"LOGGED_IN":false}"#), Some(false));
        assert_eq!(
            html_session_logged_in(r#"{"DELEGATED_SESSION_ID":"abc"}"#),
            Some(true)
        );
        assert_eq!(html_session_logged_in("no signal here"), None);
    }
}
