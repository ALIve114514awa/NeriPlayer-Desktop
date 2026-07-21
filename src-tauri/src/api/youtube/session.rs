// YouTube 登录会话维护：合并 Set-Cookie，避免会话漂移
use std::collections::{HashMap, HashSet};

use crate::auth::state::{CookieEntry, YouTubeAuth};

const IDENTITY_COOKIE_KEYS: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "LSID",
    "LOGIN_INFO",
    "__Secure-1PSID",
    "__Secure-3PSID",
];

const PERSISTED_COOKIE_KEYS: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "LOGIN_INFO",
    "SIDCC",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
    "__Secure-1PSIDTS",
    "__Secure-3PSIDTS",
    "__Secure-1PSIDCC",
    "__Secure-3PSIDCC",
    "PREF",
    "VISITOR_INFO1_LIVE",
    "YSC",
];

fn cookie_key_allowed(name: &str) -> bool {
    PERSISTED_COOKIE_KEYS
        .iter()
        .any(|key| key.eq_ignore_ascii_case(name))
        || name.starts_with("__Secure-")
        || name.starts_with("__Host-")
}

fn domain_for_name(name: &str) -> String {
    if name.starts_with("__Secure-") || name.starts_with("__Host-") {
        ".google.com".into()
    } else if name.eq_ignore_ascii_case("SAPISID")
        || name.eq_ignore_ascii_case("APISID")
        || name.eq_ignore_ascii_case("SID")
        || name.eq_ignore_ascii_case("HSID")
        || name.eq_ignore_ascii_case("SSID")
        || name.eq_ignore_ascii_case("LOGIN_INFO")
    {
        ".youtube.com".into()
    } else {
        ".youtube.com".into()
    }
}

/// 解析响应 Set-Cookie 头为 CookieEntry
pub fn parse_set_cookie_headers(headers: &[String]) -> Vec<CookieEntry> {
    let mut out = Vec::new();
    for header in headers {
        if let Some(entry) = parse_single_set_cookie(header) {
            out.push(entry);
        }
    }
    out
}

fn parse_single_set_cookie(header: &str) -> Option<CookieEntry> {
    let first = header.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() || !cookie_key_allowed(name) {
        return None;
    }

    let mut domain = domain_for_name(name);
    for part in header.split(';').skip(1) {
        let part = part.trim();
        if let Some(raw) = part
            .strip_prefix("Domain=")
            .or_else(|| part.strip_prefix("domain="))
        {
            let d = raw.trim();
            if !d.is_empty() {
                domain = if d.starts_with('.') {
                    d.to_string()
                } else {
                    format!(".{d}")
                };
            }
        }
    }

    Some(CookieEntry {
        name: name.to_string(),
        value: value.to_string(),
        domain,
    })
}

fn cookies_to_map(entries: &[CookieEntry]) -> HashMap<String, CookieEntry> {
    let mut map = HashMap::new();
    for entry in entries {
        if entry.name.is_empty() || entry.value.is_empty() {
            continue;
        }
        map.insert(entry.name.clone(), entry.clone());
    }
    map
}

fn identity_matches(previous: &HashMap<String, CookieEntry>, current: &HashMap<String, CookieEntry>) -> bool {
    IDENTITY_COOKIE_KEYS.iter().any(|key| {
        match (previous.get(*key), current.get(*key)) {
            (Some(left), Some(right)) => !left.value.is_empty() && left.value == right.value,
            _ => false,
        }
    })
}

fn has_conflicting_identity(
    previous: &HashMap<String, CookieEntry>,
    current: &HashMap<String, CookieEntry>,
) -> bool {
    IDENTITY_COOKIE_KEYS.iter().any(|key| {
        match (previous.get(*key), current.get(*key)) {
            (Some(left), Some(right)) => {
                !left.value.is_empty() && !right.value.is_empty() && left.value != right.value
            }
            _ => false,
        }
    })
}

/// 合并观察值到现有会话；同身份时保留旧 cookie，避免短暂响应冲掉完整登录态
pub fn merge_youtube_auth_cookies(
    base: &YouTubeAuth,
    observed: &[CookieEntry],
) -> YouTubeAuth {
    if observed.is_empty() {
        return base.clone();
    }

    let previous = cookies_to_map(&base.cookies);
    let mut merged = previous.clone();
    for entry in observed {
        if entry.name.is_empty() || entry.value.is_empty() || !cookie_key_allowed(&entry.name) {
            continue;
        }
        merged.insert(entry.name.clone(), entry.clone());
    }

    // 观察值与旧会话同身份时，补回观察值缺失的关键 cookie
    if identity_matches(&previous, &merged) && !has_conflicting_identity(&previous, &merged) {
        for (key, value) in &previous {
            merged.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }

    let mut cookies: Vec<CookieEntry> = merged.into_values().collect();
    cookies.sort_by(|a, b| a.name.cmp(&b.name));

    YouTubeAuth {
        cookies,
        nickname: base.nickname.clone(),
        avatar_url: base.avatar_url.clone(),
    }
}

pub fn youtube_auth_changed(previous: &YouTubeAuth, current: &YouTubeAuth) -> bool {
    let left: HashSet<_> = previous
        .cookies
        .iter()
        .map(|c| (c.name.as_str(), c.value.as_str(), c.domain.as_str()))
        .collect();
    let right: HashSet<_> = current
        .cookies
        .iter()
        .map(|c| (c.name.as_str(), c.value.as_str(), c.domain.as_str()))
        .collect();
    left != right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_cookie_keeps_sapisid() {
        let headers = vec![
            "SAPISID=abc123; Path=/; Domain=.youtube.com; Secure".into(),
            "random=1; Path=/".into(),
        ];
        let parsed = parse_set_cookie_headers(&headers);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "SAPISID");
        assert_eq!(parsed[0].value, "abc123");
    }

    #[test]
    fn merge_preserves_identity_cookies() {
        let base = YouTubeAuth {
            cookies: vec![
                CookieEntry {
                    name: "SID".into(),
                    value: "sid-old".into(),
                    domain: ".youtube.com".into(),
                },
                CookieEntry {
                    name: "SAPISID".into(),
                    value: "sap-old".into(),
                    domain: ".youtube.com".into(),
                },
            ],
            nickname: Some("n".into()),
            avatar_url: None,
        };
        let observed = vec![CookieEntry {
            name: "SAPISID".into(),
            value: "sap-new".into(),
            domain: ".youtube.com".into(),
        }];
        let merged = merge_youtube_auth_cookies(&base, &observed);
        assert!(merged.cookies.iter().any(|c| c.name == "SID" && c.value == "sid-old"));
        assert!(merged
            .cookies
            .iter()
            .any(|c| c.name == "SAPISID" && c.value == "sap-new"));
    }
}
