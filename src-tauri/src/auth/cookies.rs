// Cookie 持久化：Release 使用系统钥匙串，Debug 使用随机路径明文文件
// tauri-plugin-store 仅负责旧数据迁移
use std::collections::HashMap;
use std::sync::Arc;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::Url;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use super::state::{AuthState, CookieEntry};
use crate::security;

const STORE_FILE: &str = "auth.json";
const STORE_KEY: &str = "auth_state";

/// 持久化 AuthState
pub fn save_auth(app: &AppHandle, auth: &AuthState) {
    if !has_any_auth(auth) {
        if !delete_persisted_auth(app) {
            log::error!(target: "auth", "登录凭据存储不可用，无法清除登录凭据");
        }
        return;
    }

    let Ok(serialized) = serde_json::to_string(auth) else {
        log::error!(target: "auth", "登录凭据序列化失败，已跳过持久化");
        return;
    };

    if !security::set_secret(security::AUTH_STATE_KEY, &serialized) {
        log::error!(target: "auth", "登录凭据存储不可用，已跳过登录凭据持久化");
        clear_legacy_auth(app);
        return;
    }

    clear_legacy_auth(app);
}

/// 启动时恢复 AuthState，并迁移旧版明文数据
pub fn load_auth(app: &AppHandle) -> AuthState {
    if let Some(serialized) = security::get_secret(security::AUTH_STATE_KEY) {
        clear_legacy_auth(app);
        return match serde_json::from_str(&serialized) {
            Ok(auth) => auth,
            Err(_) => {
                log::error!(target: "auth", "登录凭据格式无效，已清除");
                let _ = security::delete_secret(security::AUTH_STATE_KEY);
                AuthState::default()
            }
        };
    }

    let legacy = load_legacy_auth(app);
    if !has_any_auth(&legacy) {
        clear_legacy_auth(app);
        return AuthState::default();
    }

    let Ok(serialized) = serde_json::to_string(&legacy) else {
        clear_legacy_auth(app);
        return AuthState::default();
    };

    if security::set_secret(security::AUTH_STATE_KEY, &serialized) {
        clear_legacy_auth(app);
        return legacy;
    }

    // 目标存储不可用时不继续使用旧明文凭据，避免下次启动再次暴露
    log::error!(target: "auth", "旧版登录凭据迁移失败，已清除明文凭据");
    clear_legacy_auth(app);
    AuthState::default()
}

/// 删除所有持久化登录凭据，包括旧版明文数据
pub fn delete_persisted_auth(app: &AppHandle) -> bool {
    let deleted = security::delete_secret(security::AUTH_STATE_KEY);
    clear_legacy_auth(app);
    deleted
}

fn load_legacy_auth(app: &AppHandle) -> AuthState {
    let Ok(store) = app.store(STORE_FILE) else {
        return AuthState::default();
    };
    store
        .get(STORE_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn clear_legacy_auth(app: &AppHandle) {
    if let Ok(store) = app.store(STORE_FILE) {
        let _ = store.delete(STORE_KEY);
        let _ = store.save();
    }
}

fn has_any_auth(auth: &AuthState) -> bool {
    auth.netease.is_some() || auth.bilibili.is_some() || auth.youtube.is_some()
}

/// 将所有已登录平台的 Cookie 注入 Jar
pub fn inject_all(jar: &Arc<Jar>, auth: &AuthState) {
    if let Some(ref netease) = auth.netease {
        inject_cookies(jar, &netease.cookies);
    }
    if let Some(ref bilibili) = auth.bilibili {
        inject_cookies(jar, &bilibili.cookies);
    }
    if let Some(ref youtube) = auth.youtube {
        inject_cookies(jar, &youtube.cookies);
    }
}

/// 将 Cookie 列表注入 Jar（包含 Domain 属性，确保子域名可用）
pub fn inject_cookies(jar: &Arc<Jar>, entries: &[CookieEntry]) {
    for entry in entries {
        let url = domain_to_url(&entry.domain);
        if let Ok(url) = url.parse::<Url>() {
            // 必须设置 Domain 属性，否则 reqwest 按精确域名匹配，子域名 API 拿不到 cookie
            jar.add_cookie_str(
                &format!("{}={}; Domain={}; Path=/", entry.name, entry.value, entry.domain),
                &url,
            );
        }
    }
}

/// 从共享 Jar 回收服务端轮换后的 Cookie 值
///
/// 服务端会在响应的 Set-Cookie 中轮换会话令牌，reqwest 会把新值写进 Jar，
/// 但 AuthState 只在登录时落盘。重启后 inject_all 会把登录当天的旧值重新写回
/// Jar，对实现了令牌轮换 + 重放检测的服务端就等于提交了一个已作废的令牌，
/// 整条会话链会被判为异常 —— 表现就是「另一台设备登录后这边掉登录」。
///
/// 返回是否发生了变更，调用方据此决定要不要重新落盘。
pub fn sync_auth_from_jar(jar: &Arc<Jar>, auth: &mut AuthState) -> bool {
    let mut cache: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut changed = false;
    if let Some(netease) = auth.netease.as_mut() {
        changed |= refresh_entries_from_jar(jar, &mut netease.cookies, &mut cache);
    }
    if let Some(bilibili) = auth.bilibili.as_mut() {
        changed |= refresh_entries_from_jar(jar, &mut bilibili.cookies, &mut cache);
    }
    if let Some(youtube) = auth.youtube.as_mut() {
        changed |= refresh_entries_from_jar(jar, &mut youtube.cookies, &mut cache);
    }
    changed
}

fn refresh_entries_from_jar(
    jar: &Arc<Jar>,
    entries: &mut [CookieEntry],
    cache: &mut HashMap<String, HashMap<String, String>>,
) -> bool {
    let mut changed = false;
    for entry in entries.iter_mut() {
        let values = cache
            .entry(entry.domain.clone())
            .or_insert_with(|| read_jar_cookies(jar, &entry.domain));
        let Some(current) = values.get(&entry.name) else {
            continue;
        };
        if current.is_empty() || current == &entry.value {
            continue;
        }
        entry.value = current.clone();
        changed = true;
    }
    changed
}

fn read_jar_cookies(jar: &Arc<Jar>, domain: &str) -> HashMap<String, String> {
    let Ok(url) = domain_to_url(domain).parse::<Url>() else {
        return HashMap::new();
    };
    let Some(header) = jar.cookies(&url) else {
        return HashMap::new();
    };
    let Ok(text) = header.to_str() else {
        return HashMap::new();
    };
    text.split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// 登出时过期指定平台的 Cookie
pub fn expire_platform_cookies(jar: &Arc<Jar>, auth: &AuthState, platform: &str) {
    let entries = match platform {
        "netease" => auth.netease.as_ref().map(|a| &a.cookies),
        "bilibili" => auth.bilibili.as_ref().map(|a| &a.cookies),
        "youtube" => auth.youtube.as_ref().map(|a| &a.cookies),
        _ => None,
    };

    if let Some(entries) = entries {
        for entry in entries {
            let url = domain_to_url(&entry.domain);
            if let Ok(url) = url.parse::<Url>() {
                // 必须带 Domain + Path 属性，与注入时一致，才能正确覆盖并过期
                jar.add_cookie_str(
                    &format!("{}=deleted; Domain={}; Path=/; Max-Age=0", entry.name, entry.domain),
                    &url,
                );
            }
        }
    }
}

/// 解析 document.cookie 字符串为 CookieEntry 列表
pub fn parse_document_cookies(cookie_str: &str, domain: &str) -> Vec<CookieEntry> {
    cookie_str
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            let (name, value) = pair.split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() {
                return None;
            }
            Some(CookieEntry {
                name: name.to_string(),
                value: value.to_string(),
                domain: domain.to_string(),
            })
        })
        .collect()
}

/// 解析用户粘贴的原始 Cookie 文本（对齐 Android RawCookieTextParser）
/// 支持分号、换行、回车分隔
pub fn parse_raw_cookie_text(raw: &str, platform: &str) -> Vec<CookieEntry> {
    let domain = match platform {
        "netease" => "music.163.com",
        "bilibili" => ".bilibili.com",
        "youtube" => ".youtube.com",
        _ => "unknown",
    };

    let mut entries = Vec::new();
    // 按 ; \r \n 分割
    for segment in raw.split([';', '\r', '\n']) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if let Some((name, value)) = segment.split_once('=') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if !name.is_empty() {
                entries.push(CookieEntry { name, value, domain: domain.to_string() });
            }
        }
    }

    // YouTube 需要额外为 google.com 注入部分 Cookie
    if platform == "youtube" {
        let google_entries: Vec<CookieEntry> = entries.iter()
            .filter(|c| matches!(c.name.as_str(), "SID" | "HSID" | "SSID" | "APISID" | "SAPISID" | "LSID" | "SIDCC"))
            .map(|c| CookieEntry {
                name: c.name.clone(),
                value: c.value.clone(),
                domain: ".google.com".into(),
            })
            .collect();
        entries.extend(google_entries);
    }

    entries
}

/// 域名转 URL（用于 Jar.add_cookie_str）
fn domain_to_url(domain: &str) -> String {
    let d = domain.trim_start_matches('.');
    format!("https://{}", d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::state::NeteaseAuth;

    #[test]
    fn raw_cookie_parser_accepts_all_supported_separators() {
        let entries = parse_raw_cookie_text("first=1;second=2\rthird=3\nfourth=4", "netease");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();

        assert_eq!(names, ["first", "second", "third", "fourth"]);
    }

    #[test]
    fn rotated_jar_cookies_are_written_back_into_auth_state() {
        let jar = Arc::new(Jar::default());
        let mut auth = AuthState {
            netease: Some(NeteaseAuth {
                cookies: vec![
                    CookieEntry {
                        name: "MUSIC_U".into(),
                        value: "stale".into(),
                        domain: "music.163.com".into(),
                    },
                    CookieEntry {
                        name: "os".into(),
                        value: "pc".into(),
                        domain: "music.163.com".into(),
                    },
                ],
                user_id: None,
                nickname: None,
                avatar_url: None,
            }),
            ..Default::default()
        };
        inject_cookies(&jar, &auth.netease.as_ref().unwrap().cookies);

        // 模拟服务端轮换
        jar.add_cookie_str(
            "MUSIC_U=rotated; Domain=music.163.com; Path=/",
            &"https://music.163.com".parse::<Url>().unwrap(),
        );

        assert!(sync_auth_from_jar(&jar, &mut auth));
        let cookies = &auth.netease.as_ref().unwrap().cookies;
        assert_eq!(
            cookies.iter().find(|c| c.name == "MUSIC_U").unwrap().value,
            "rotated"
        );
        assert_eq!(cookies.iter().find(|c| c.name == "os").unwrap().value, "pc");
        // 无变化时不应报告变更，避免每次心跳都写钥匙串
        assert!(!sync_auth_from_jar(&jar, &mut auth));
    }
}
