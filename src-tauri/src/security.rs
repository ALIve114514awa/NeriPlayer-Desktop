// keyring 按平台使用 macOS Keychain、Windows Credential Manager、Linux Secret Service
// 敏感凭据不落盘到应用配置文件

const SERVICE_NAME: &str = "moe.ouom.neriplayer.desktop";

pub const AUTH_STATE_KEY: &str = "auth-state-v1";
pub const GITHUB_TOKEN_KEY: &str = "github-token-v1";
pub const WEBDAV_PASSWORD_KEY: &str = "webdav-password-v1";

/// 从系统凭据存储读取秘密值
pub fn get_secret(key: &str) -> Option<String> {
    let entry = keyring::Entry::new(SERVICE_NAME, key).ok()?;
    entry.get_password().ok()
}

/// 写入系统凭据存储，失败时返回 false
pub fn set_secret(key: &str, value: &str) -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE_NAME, key) else {
        return false;
    };
    entry.set_password(value).is_ok()
}

/// 删除系统凭据存储中的秘密值
pub fn delete_secret(key: &str) -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE_NAME, key) else {
        return false;
    };
    if entry.delete_credential().is_ok() {
        return true;
    }
    // 某些后端只支持覆盖值，清空后也不会再携带可用凭据
    entry.set_password("").is_ok()
}
