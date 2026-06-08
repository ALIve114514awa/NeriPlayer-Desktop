// 三平台登录/登出命令
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use std::time::Duration;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::auth::state::{AuthInfo, AuthStatusResponse, NeteaseAuth, BiliAuth, YouTubeAuth, CookieEntry};
use crate::auth::cookies;

// 登录检测机制：
// 1. 打开 WebviewWindow 加载平台登录页
// 2. 每 800ms 调用 Tauri 内置 cookies_for_url() 读取 Cookie（含 HttpOnly）
// 3. 检测到 sentinel cookie 后关闭窗口，保存 cookie

/// 从 WebView 窗口轮询提取 Cookie（使用 Tauri 内置 API 读取 HttpOnly）
async fn poll_webview_cookies(
    app: &AppHandle,
    window_label: &str,
    sentinel_cookie: &str,
    cookie_urls: &[&str],
    timeout_secs: u64,
) -> AppResult<Vec<CookieEntry>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let poll_interval = Duration::from_millis(800);

    loop {
        if tokio::time::Instant::now() > deadline {
            if let Some(w) = app.get_webview_window(window_label) {
                let _ = w.close();
            }
            return Err(AppError::Other("Login timeout".into()));
        }

        // 检测窗口是否仍然存在
        let window = match app.get_webview_window(window_label) {
            Some(w) => w,
            None => return Err(AppError::Other("Login cancelled".into())),
        };

        // 通过 Tauri 内置 cookies_for_url 读取所有 Cookie（包括 HttpOnly）
        let mut all_entries: Vec<CookieEntry> = Vec::new();
        let mut found_sentinel = false;

        for url_str in cookie_urls {
            if let Ok(url) = url::Url::parse(url_str) {
                // cookies_for_url 能读取 HttpOnly cookie
                if let Ok(cookies) = window.cookies_for_url(url) {
                    for c in &cookies {
                        let name = c.name().to_string();
                        let value = c.value().to_string();
                        let domain = c.domain().unwrap_or("").to_string();

                        // 检查 sentinel
                        if name == sentinel_cookie && !value.is_empty() {
                            found_sentinel = true;
                        }

                        // 去重
                        if !all_entries.iter().any(|e| e.name == name && e.domain == domain) {
                            all_entries.push(CookieEntry { name, value, domain });
                        }
                    }
                }
            }
        }

        if found_sentinel && !all_entries.is_empty() {
            let _ = window.close();
            return Ok(all_entries);
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// 网易云登录
#[tauri::command]
pub async fn login_netease(app: AppHandle, state: State<'_, AppState>) -> AppResult<AuthInfo> {
    let label = "netease-login";
    let _window = WebviewWindowBuilder::new(
        &app,
        label,
        WebviewUrl::External("https://music.163.com/#/login".parse().unwrap()),
    )
    .title("NeriPlayer - 网易云音乐登录")
    .inner_size(420.0, 600.0)
    .center()
    .build()
    .map_err(|e| AppError::Other(format!("Failed to create login window: {}", e)))?;

    let cookie_urls = &[
        "https://music.163.com",
        "https://interface.music.163.com",
        "https://interface3.music.163.com",
    ];
    let mut entries = poll_webview_cookies(&app, label, "MUSIC_U", cookie_urls, 300).await?;

    // 补全默认 Cookie（与 Android 一致）
    if !entries.iter().any(|c| c.name == "os") {
        entries.push(CookieEntry { name: "os".into(), value: "pc".into(), domain: "music.163.com".into() });
    }
    if !entries.iter().any(|c| c.name == "appver") {
        entries.push(CookieEntry { name: "appver".into(), value: "8.10.35".into(), domain: "music.163.com".into() });
    }

    // 注入 Jar
    cookies::inject_cookies(&state.cookie_jar, &entries);

    // 调用 API 获取用户信息
    let client = crate::api::netease::client::NeteaseClient::new(&state.http());
    let (user_id, nickname, avatar_url) = match client.get_user_account().await {
        Ok(account) => {
            let profile = &account["profile"];
            (
                profile["userId"].as_u64(),
                profile["nickname"].as_str().map(String::from),
                profile["avatarUrl"].as_str().map(String::from),
            )
        }
        Err(_) => (None, None, None),
    };

    let auth = NeteaseAuth { cookies: entries, user_id, nickname: nickname.clone(), avatar_url: avatar_url.clone() };
    {
        let mut auth_state = state.auth.lock();
        auth_state.netease = Some(auth);
        cookies::save_auth(&app, &auth_state);
    }

    Ok(AuthInfo {
        platform: "netease".into(),
        logged_in: true,
        nickname,
        avatar_url,
    })
}

/// B站登录
#[tauri::command]
pub async fn login_bilibili(app: AppHandle, state: State<'_, AppState>) -> AppResult<AuthInfo> {
    let label = "bilibili-login";
    let _window = WebviewWindowBuilder::new(
        &app,
        label,
        WebviewUrl::External("https://passport.bilibili.com/login".parse().unwrap()),
    )
    .title("NeriPlayer - 哔哩哔哩登录")
    .inner_size(420.0, 600.0)
    .center()
    .build()
    .map_err(|e| AppError::Other(format!("Failed to create login window: {}", e)))?;

    let cookie_urls = &[
        "https://www.bilibili.com",
        "https://passport.bilibili.com",
        "https://api.bilibili.com",
    ];
    let mut entries = poll_webview_cookies(&app, label, "SESSDATA", cookie_urls, 300).await?;

    // B站核心 cookie 必须关联到 .bilibili.com 域，确保 api.bilibili.com 子域名也能发送
    let bili_core_cookies = ["SESSDATA", "DedeUserID", "DedeUserID__ckMd5", "bili_jct", "sid"];
    for entry in &mut entries {
        if bili_core_cookies.contains(&entry.name.as_str()) && !entry.domain.starts_with('.') {
            entry.domain = ".bilibili.com".to_string();
        }
    }

    // 注入 Jar（含 Domain 属性，确保子域名 API 生效）
    cookies::inject_cookies(&state.cookie_jar, &entries);

    // 从 Cookie 提取 DedeUserID
    let mid = entries.iter()
        .find(|c| c.name == "DedeUserID")
        .and_then(|c| c.value.parse::<u64>().ok());

    // 调用 B站 nav API 获取用户信息
    let client = crate::api::bilibili::client::BiliClient::new(&state.http());
    let (nickname, avatar_url) = match client.get_user_info().await {
        Ok(info) => {
            let data = &info["data"];
            // 必须检查 isLogin，未登录时 data 中无有效用户信息
            let is_login = data["isLogin"].as_bool().unwrap_or(false);
            if is_login {
                (
                    data["uname"].as_str().map(String::from),
                    data["face"].as_str().map(String::from),
                )
            } else {
                log::warn!("Bilibili nav API 返回 isLogin=false，cookie 可能未生效");
                (None, None)
            }
        }
        Err(e) => {
            log::warn!("Bilibili get_user_info 失败: {}", e);
            (None, None)
        }
    };

    let auth = BiliAuth { cookies: entries, mid, nickname: nickname.clone(), avatar_url: avatar_url.clone() };
    {
        let mut auth_state = state.auth.lock();
        auth_state.bilibili = Some(auth);
        cookies::save_auth(&app, &auth_state);
    }

    Ok(AuthInfo {
        platform: "bilibili".into(),
        logged_in: true,
        nickname,
        avatar_url,
    })
}

/// YouTube Music 登录
#[tauri::command]
pub async fn login_youtube(app: AppHandle, state: State<'_, AppState>) -> AppResult<AuthInfo> {
    let login_url = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
    let label = "youtube-login";

    let _window = WebviewWindowBuilder::new(
        &app,
        label,
        WebviewUrl::External(login_url.parse().unwrap()),
    )
    .title("NeriPlayer - YouTube Music Login")
    .inner_size(480.0, 680.0)
    .center()
    .build()
    .map_err(|e| AppError::Other(format!("Failed to create login window: {}", e)))?;

    // YouTube cookie 分布在多个域
    let cookie_urls = &[
        "https://music.youtube.com",
        "https://www.youtube.com",
        "https://youtube.com",
        "https://accounts.google.com",
        "https://www.google.com",
        "https://google.com",
        "https://m.youtube.com",
    ];
    let entries = poll_webview_cookies(&app, label, "SAPISID", cookie_urls, 300).await?;

    // 注入 Jar
    cookies::inject_cookies(&state.cookie_jar, &entries);

    let auth = YouTubeAuth { cookies: entries, nickname: None, avatar_url: None };
    {
        let mut auth_state = state.auth.lock();
        auth_state.youtube = Some(auth);
        cookies::save_auth(&app, &auth_state);
    }

    Ok(AuthInfo {
        platform: "youtube".into(),
        logged_in: true,
        nickname: None,
        avatar_url: None,
    })
}

/// Cookie 粘贴登录（对齐 Android 端）
#[tauri::command]
pub async fn login_with_cookies(
    platform: String,
    raw_cookies: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<AuthInfo> {
    // 解析用户粘贴的 Cookie 文本
    let entries = cookies::parse_raw_cookie_text(&raw_cookies, &platform);

    if entries.is_empty() {
        return Err(AppError::Other("No valid cookies found".into()));
    }

    // 注入 Jar
    cookies::inject_cookies(&state.cookie_jar, &entries);

    match platform.as_str() {
        "netease" => {
            // 验证 MUSIC_U 存在
            if !entries.iter().any(|c| c.name == "MUSIC_U" && !c.value.is_empty()) {
                return Err(AppError::Other("Missing required cookie: MUSIC_U".into()));
            }

            let client = crate::api::netease::client::NeteaseClient::new(&state.http());
            let (user_id, nickname, avatar_url) = match client.get_user_account().await {
                Ok(account) => {
                    let profile = &account["profile"];
                    (
                        profile["userId"].as_u64(),
                        profile["nickname"].as_str().map(String::from),
                        profile["avatarUrl"].as_str().map(String::from),
                    )
                }
                Err(e) => return Err(AppError::Other(format!("Cookie validation failed: {}", e))),
            };

            let auth = NeteaseAuth { cookies: entries, user_id, nickname: nickname.clone(), avatar_url: avatar_url.clone() };
            {
                let mut auth_state = state.auth.lock();
                auth_state.netease = Some(auth);
                cookies::save_auth(&app, &auth_state);
            }

            Ok(AuthInfo { platform: "netease".into(), logged_in: true, nickname, avatar_url })
        }
        "bilibili" => {
            if !entries.iter().any(|c| c.name == "SESSDATA" && !c.value.is_empty()) {
                return Err(AppError::Other("Missing required cookie: SESSDATA".into()));
            }

            let mid = entries.iter()
                .find(|c| c.name == "DedeUserID")
                .and_then(|c| c.value.parse::<u64>().ok());

            let client = crate::api::bilibili::client::BiliClient::new(&state.http());
            let (nickname, avatar_url) = match client.get_user_info().await {
                Ok(info) => {
                    let data = &info["data"];
                    let is_login = data["isLogin"].as_bool().unwrap_or(false);
                    if is_login {
                        (data["uname"].as_str().map(String::from), data["face"].as_str().map(String::from))
                    } else {
                        return Err(AppError::Other("Cookie 验证失败：B站返回未登录状态".into()));
                    }
                }
                Err(e) => return Err(AppError::Other(format!("Cookie validation failed: {}", e))),
            };

            let auth = BiliAuth { cookies: entries, mid, nickname: nickname.clone(), avatar_url: avatar_url.clone() };
            {
                let mut auth_state = state.auth.lock();
                auth_state.bilibili = Some(auth);
                cookies::save_auth(&app, &auth_state);
            }

            Ok(AuthInfo { platform: "bilibili".into(), logged_in: true, nickname, avatar_url })
        }
        "youtube" => {
            if !entries.iter().any(|c| c.name == "SAPISID" && !c.value.is_empty()) {
                return Err(AppError::Other("Missing required cookie: SAPISID".into()));
            }

            let auth = YouTubeAuth { cookies: entries, nickname: None, avatar_url: None };
            {
                let mut auth_state = state.auth.lock();
                auth_state.youtube = Some(auth);
                cookies::save_auth(&app, &auth_state);
            }

            Ok(AuthInfo { platform: "youtube".into(), logged_in: true, nickname: None, avatar_url: None })
        }
        _ => Err(AppError::Other(format!("Unknown platform: {}", platform))),
    }
}

/// 查询所有平台登录状态
#[tauri::command]
pub async fn check_auth_status(state: State<'_, AppState>) -> AppResult<AuthStatusResponse> {
    let auth = state.auth.lock();
    Ok(auth.to_status_response())
}

/// 登出指定平台
#[tauri::command]
pub async fn logout(platform: String, app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let mut auth = state.auth.lock();

    // 过期 reqwest Jar 中的 Cookie
    cookies::expire_platform_cookies(&state.cookie_jar, &auth, &platform);

    // 清除内存状态
    match platform.as_str() {
        "netease" => auth.netease = None,
        "bilibili" => auth.bilibili = None,
        "youtube" => auth.youtube = None,
        _ => return Err(AppError::Other(format!("Unknown platform: {}", platform))),
    }

    // 持久化
    cookies::save_auth(&app, &auth);

    // 清除 WebView2 浏览器 cookie（所有平台共享一个 cookie store）
    // 清除后重新注入剩余已登录平台的 cookie
    let remaining_auth = auth.clone();
    drop(auth);

    // 在后台清除 WebView cookie
    let app_clone = app.clone();
    let jar = state.cookie_jar.clone();
    tokio::task::spawn(async move {
        if let Err(e) = clear_and_reinject_webview_cookies(&app_clone, &jar, &remaining_auth).await {
            log::warn!("清除 WebView cookie 失败: {}", e);
        }
    });

    Ok(())
}

/// 清除 WebView2 cookie 并重新注入剩余平台的 cookie
async fn clear_and_reinject_webview_cookies(
    app: &AppHandle,
    jar: &std::sync::Arc<reqwest::cookie::Jar>,
    remaining_auth: &crate::auth::state::AuthState,
) -> AppResult<()> {
    // 创建一个不可见的临时窗口来操作 WebView2 cookie
    let label = "cookie-cleaner";
    let window = WebviewWindowBuilder::new(
        app, label,
        WebviewUrl::External("about:blank".parse().unwrap()),
    )
    .visible(false)
    .build()
    .map_err(|e| AppError::Other(format!("Failed to create cleaner window: {}", e)))?;

    // 短暂等待窗口初始化
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 清除所有浏览数据（含所有 cookie）
    let _ = window.clear_all_browsing_data();

    // 关闭临时窗口
    let _ = window.close();

    // 重新注入剩余已登录平台的 cookie 到 reqwest Jar
    // （WebView cookie 已经清空，下次打开登录窗口时会是干净状态）
    cookies::inject_all(jar, remaining_auth);

    Ok(())
}
