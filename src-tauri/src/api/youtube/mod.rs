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

/// 返回 InnerTube 用的 (hl, gl)
///
/// 开启国际化时统一走 en/US，关闭时按应用语言推导区域，
/// 这样这个开关才真正影响返回的内容目录，而不是摆设。
pub fn innertube_locale() -> (String, String) {
    if is_international_mode() {
        return ("en".into(), "US".into());
    }
    let locale = APP_LOCALE
        .read()
        .map(|value| value.clone())
        .unwrap_or_default();
    match locale.as_str() {
        "zh-TW" => ("zh-TW".into(), "TW".into()),
        "ja" => ("ja".into(), "JP".into()),
        "en" => ("en".into(), "US".into()),
        "zh-CN" => ("zh-CN".into(), "CN".into()),
        _ => ("en".into(), "US".into()),
    }
}
