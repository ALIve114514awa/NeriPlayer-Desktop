// YouTube Music 后端子系统
//
// 分层约定 (工业级 / Premium + 防双端互踢):
// - client / account / playlist / refresh / session
//   已登录库能力: WEB_REMIX + Cookie + SAPISID*HASH
//   首页、歌单、账号资料、会话保鲜; 不负责音轨直链
// - playback
//   可播音频流: ANDROID_VR(+visitorData) -> IOS -> ANDROID -> ANDROID_MUSIC -> TVHTML5
//   对齐 yt-dlp jsless 默认 (ANDROID_VR); IOS/ANDROID plain url 会在 ~1MB 后 CDN 403
//   已登录时 player 只带 Cookie, 不带 SAPISID*HASH (mobile+hash=HTTP 400)
//   故意不走 WEB_REMIX player + PO token 完整浏览器模拟
//   googlevideo CDN 拉流不附带登录 Cookie; UA 按直链 c= 匹配
//   排序优先 audio/mp4 (symphonia 未启 opus)
//
// 参考:
// - yt-dlp INNERTUBE_CLIENTS / _DEFAULT_JSLESS_CLIENTS (android_vr)
// - Android YouTubeMusicPlaybackRepository (player 带 Cookie, stream 不带)
// - 桌面 Innertube 客户端: plain-url 客户端优先, 防互踢

mod account;
pub mod client;
pub mod duration;
pub mod playback;
pub mod playlist;
pub mod session;
pub mod refresh;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

/// YouTube 国际化模式：开启后强制用海外 locale 访问，关闭则跟随应用语言
static INTERNATIONAL_MODE: AtomicBool = AtomicBool::new(false);
static APP_LOCALE: RwLock<String> = RwLock::new(String::new());

pub fn set_locale_preferences(international: bool, locale: &str) {
    INTERNATIONAL_MODE.store(international, Ordering::Release);
    if let Ok(mut slot) = APP_LOCALE.write() {
        *slot = locale.to_string();
    }
}

pub fn is_international_mode() -> bool {
    INTERNATIONAL_MODE.load(Ordering::Acquire)
}

/// YouTube Music 不提供服务的区域
///
/// 把 gl 设成这些区域会让 InnerTube 直接返回空目录（曾经因为按应用语言
/// 推导出 gl=CN 导致云端歌单整个读不到），所以只用于界面语言，不用于区域。
const UNSUPPORTED_REGIONS: &[&str] = &["CN"];

/// InnerTube 缺省区域
///
/// YouTube Music 在部分地区不可用，桌面端历来固定用 JP 兜底以保证目录可读，
/// 这里保持该行为，不要改成按语言推导。
const DEFAULT_REGION: &str = "JP";

/// 返回 InnerTube 用的 (hl, gl)
///
/// 开启国际化时统一走 en/US；关闭时界面语言跟随应用设置，但区域仍走可用区，
/// 避免把 gl 设成 YouTube Music 不服务的地区导致目录为空。
pub fn innertube_locale() -> (String, String) {
    if is_international_mode() {
        return ("en".into(), "US".into());
    }
    let locale = APP_LOCALE
        .read()
        .map(|value| value.clone())
        .unwrap_or_default();
    let (hl, gl) = match locale.as_str() {
        "zh-TW" => ("zh-TW", "TW"),
        "ja" => ("ja", "JP"),
        "en" => ("en", "US"),
        "zh-CN" => ("zh-CN", DEFAULT_REGION),
        _ => ("zh-CN", DEFAULT_REGION),
    };
    let gl = if UNSUPPORTED_REGIONS.contains(&gl) { DEFAULT_REGION } else { gl };
    (hl.into(), gl.into())
}

#[cfg(test)]
mod locale_tests {
    use super::*;

    /// locale 偏好是进程级全局状态，必须在同一个测试里顺序断言，
    /// 拆成多个测试会因并行执行互相覆盖
    #[test]
    fn locale_resolution_never_targets_an_unavailable_market() {
        for locale in ["zh-CN", "zh-TW", "ja", "en", "", "unknown"] {
            set_locale_preferences(false, locale);
            let (hl, gl) = innertube_locale();
            assert!(!hl.is_empty(), "locale {locale} resolved to an empty hl");
            assert!(
                !UNSUPPORTED_REGIONS.contains(&gl.as_str()),
                "locale {locale} resolved to unsupported region {gl}",
            );
        }

        set_locale_preferences(true, "zh-CN");
        assert_eq!(innertube_locale(), ("en".to_string(), "US".to_string()));

        set_locale_preferences(false, "zh-CN");
        assert_eq!(innertube_locale().1, DEFAULT_REGION);
    }
}
