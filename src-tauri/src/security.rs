// Release 构建使用应用侧加密文件存储；Debug 构建完全使用随机路径明文文件
// 各类凭据使用独立目录，删除 Cookie 时不会影响同步凭据
//
// 为什么不用系统钥匙串：未经正式签名的应用每次更新后二进制指纹变化，
// macOS Keychain 会把它当成新应用反复弹授权框（甚至要求输入登录密码），
// 用户会以为软件在偷凭据。对标 Android 端的 EncryptedSharedPreferences：
// 应用侧加密、零系统交互。威胁模型一致——防的是「文件被拷走后可读」，
// 不承诺抵御同机同用户的恶意进程（Keychain 对未签名应用同样给不出这个承诺）。
// 旧 Keychain 凭据在首次读取时自动迁入并删除。

const SERVICE_NAME: &str = "moe.ouom.neriplayer.desktop";

pub const AUTH_STATE_KEY: &str = "auth-state-v1";
pub const GITHUB_TOKEN_KEY: &str = "github-token-v1";
pub const WEBDAV_PASSWORD_KEY: &str = "webdav-password-v1";

/// 从当前构建配置的凭据存储读取秘密值
///
/// Release 首选加密文件；读不到时回退旧 Keychain 并就地迁移——
/// 老用户升级后第一次读取还会碰一次 Keychain，此后彻底不再触发系统弹窗。
pub fn get_secret(key: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    {
        debug_secret_storage::get(key)
    }

    #[cfg(not(debug_assertions))]
    {
        if let Some(value) = encrypted_secret_storage::get(key) {
            return Some(value);
        }
        let legacy = get_keyring_secret(key)?;
        if encrypted_secret_storage::set(key, &legacy) {
            let _ = delete_keyring_secret(key);
            log::info!(target: "security", "credential migrated off the keychain: {key}");
        }
        Some(legacy)
    }
}

/// 写入当前构建配置的凭据存储，失败时返回 false
pub fn set_secret(key: &str, value: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        debug_secret_storage::set(key, value)
    }

    #[cfg(not(debug_assertions))]
    {
        encrypted_secret_storage::set(key, value)
    }
}

/// 删除当前构建配置的凭据存储中的秘密值
pub fn delete_secret(key: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        debug_secret_storage::delete(key)
    }

    #[cfg(not(debug_assertions))]
    {
        let removed = encrypted_secret_storage::delete(key);
        // 旧 Keychain 里可能还留着迁移前的副本，一并清理
        let _ = delete_keyring_secret(key);
        removed
    }
}

/// Debug 构建中检查指定凭据是否存在，不会访问系统钥匙串
pub fn debug_secret_exists(key: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        debug_secret_storage::exists(key)
    }

    #[cfg(not(debug_assertions))]
    {
        let _ = key;
        false
    }
}

/// 应用侧加密凭据存储（Release 默认）
///
/// AES-256-CBC + HMAC-SHA256（encrypt-then-MAC），随机 IV。
/// 密钥派生自机器标识 + 应用盐：文件拷到别的机器无法解密，
/// 本机不落明文。文件权限收紧到 0600（Unix）。
/// Debug 构建用明文随机路径存储（见 debug_secret_storage），本模块只在
/// Release 与测试中编译，避免 debug 下的 dead_code 告警。
#[cfg(any(not(debug_assertions), test))]
mod encrypted_secret_storage {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
    use hmac::{Hmac, Mac};
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
    type HmacSha256 = Hmac<Sha256>;

    const MAGIC: &[u8; 4] = b"NPS1";
    const APP_SALT: &[u8] = b"moe.ouom.neriplayer.desktop/secret-store/v1";

    fn store_dir() -> PathBuf {
        let mut path = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push("NeriPlayer");
        path.push("secure");
        path
    }

    fn secret_path(key: &str) -> PathBuf {
        // key 是本模块内的常量（auth-state-v1 等），不含路径成分；
        // 再做一次散列命名，目录里看不出哪个文件对应哪类凭据
        let digest = Sha256::digest([APP_SALT, key.as_bytes()].concat());
        store_dir().join(format!("{}.bin", hex::encode(&digest[..16])))
    }

    /// 机器标识：跨机器不可复制即可，不要求全球唯一
    fn machine_identity() -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
                return id.trim().as_bytes().to_vec();
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("ioreg")
                .args(["-rd1", "-c", "IOPlatformExpertDevice"])
                .output()
            {
                let text = String::from_utf8_lossy(&output.stdout);
                for line in text.lines() {
                    if let Some(rest) = line.split("\"IOPlatformUUID\" = \"").nth(1) {
                        if let Some(uuid) = rest.split('"').next() {
                            return uuid.as_bytes().to_vec();
                        }
                    }
                }
            }
        }
        // Windows 与兜底：用户与主机的稳定组合。弱于硬件 UUID，
        // 但仍满足「拷到别的机器/别的账号读不出」
        let mut fallback = Vec::new();
        for var in ["COMPUTERNAME", "HOSTNAME", "USER", "USERNAME", "HOME", "USERPROFILE"] {
            if let Ok(value) = std::env::var(var) {
                fallback.extend_from_slice(value.as_bytes());
                fallback.push(0);
            }
        }
        fallback
    }

    fn derive_keys() -> ([u8; 32], [u8; 32]) {
        let identity = machine_identity();
        let enc = Sha256::digest([APP_SALT, b"/enc/", identity.as_slice()].concat());
        let mac = Sha256::digest([APP_SALT, b"/mac/", identity.as_slice()].concat());
        (enc.into(), mac.into())
    }

    pub(super) fn get(key: &str) -> Option<String> {
        let blob = std::fs::read(secret_path(key)).ok()?;
        // 布局: MAGIC(4) | IV(16) | MAC(32) | ciphertext
        if blob.len() < 4 + 16 + 32 + 16 || &blob[..4] != MAGIC {
            return None;
        }
        let (enc_key, mac_key) = derive_keys();
        let iv: [u8; 16] = blob[4..20].try_into().ok()?;
        let stored_mac = &blob[20..52];
        let ciphertext = &blob[52..];

        let mut mac = HmacSha256::new_from_slice(&mac_key).ok()?;
        mac.update(&iv);
        mac.update(ciphertext);
        mac.verify_slice(stored_mac).ok()?;

        let mut buffer = ciphertext.to_vec();
        let plain = Aes256CbcDec::new(&enc_key.into(), &iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buffer)
            .ok()?;
        String::from_utf8(plain.to_vec()).ok()
    }

    pub(super) fn set(key: &str, value: &str) -> bool {
        let dir = store_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let (enc_key, mac_key) = derive_keys();
        let mut iv = [0_u8; 16];
        rand::thread_rng().fill_bytes(&mut iv);

        let mut buffer = vec![0_u8; value.len() + 16];
        let Ok(ciphertext) = Aes256CbcEnc::new(&enc_key.into(), &iv.into())
            .encrypt_padded_b2b_mut::<Pkcs7>(value.as_bytes(), &mut buffer)
        else {
            return false;
        };

        let Ok(mut mac) = HmacSha256::new_from_slice(&mac_key) else {
            return false;
        };
        mac.update(&iv);
        mac.update(ciphertext);
        let tag = mac.finalize().into_bytes();

        let mut blob = Vec::with_capacity(4 + 16 + 32 + ciphertext.len());
        blob.extend_from_slice(MAGIC);
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&tag);
        blob.extend_from_slice(ciphertext);

        let path = secret_path(key);
        if std::fs::write(&path, blob).is_err() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        true
    }

    pub(super) fn delete(key: &str) -> bool {
        match std::fs::remove_file(secret_path(key)) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }

    #[cfg(test)]
    mod tests {
        /// 密文必须过 MAC 校验：翻转任何一个字节都读不出值
        #[test]
        fn tampered_blob_is_rejected() {
            let key = "test-tamper-v1";
            assert!(super::set(key, "secret-value"));
            let path = super::secret_path(key);
            let mut blob = std::fs::read(&path).unwrap();
            let last = blob.len() - 1;
            blob[last] ^= 0x01;
            std::fs::write(&path, blob).unwrap();
            assert_eq!(super::get(key), None, "篡改后的密文必须拒收");
            assert!(super::delete(key));
        }

        #[test]
        fn roundtrip_and_isolation() {
            let key_a = "test-roundtrip-a";
            let key_b = "test-roundtrip-b";
            assert!(super::set(key_a, "值A：含中文 / emoji 🎵"));
            assert!(super::set(key_b, "value-b"));
            assert_eq!(super::get(key_a).as_deref(), Some("值A：含中文 / emoji 🎵"));
            assert_eq!(super::get(key_b).as_deref(), Some("value-b"));
            assert!(super::delete(key_a));
            assert_eq!(super::get(key_a), None, "删除后不得残留");
            assert_eq!(super::get(key_b).as_deref(), Some("value-b"));
            assert!(super::delete(key_b));
        }

        /// 明文绝不允许出现在落盘文件里
        #[test]
        fn plaintext_never_touches_disk() {
            let key = "test-no-plaintext";
            let secret = "MUSIC_U=super-secret-cookie-value";
            assert!(super::set(key, secret));
            let blob = std::fs::read(super::secret_path(key)).unwrap();
            let haystack = String::from_utf8_lossy(&blob);
            assert!(!haystack.contains("super-secret"), "密文文件里不得有明文片段");
            assert!(super::delete(key));
        }
    }
}

#[cfg(not(debug_assertions))]
fn get_keyring_secret(key: &str) -> Option<String> {
    let entry = keyring::Entry::new(SERVICE_NAME, key).ok()?;
    entry.get_password().ok()
}

#[cfg(not(debug_assertions))]
fn set_keyring_secret(key: &str, value: &str) -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE_NAME, key) else {
        return false;
    };
    entry.set_password(value).is_ok()
}

#[cfg(not(debug_assertions))]
fn delete_keyring_secret(key: &str) -> bool {
    let Ok(entry) = keyring::Entry::new(SERVICE_NAME, key) else {
        return false;
    };
    if entry.delete_credential().is_ok() {
        return true;
    }
    // 某些后端只支持覆盖值，清空后也不会再携带可用凭据
    entry.set_password("").is_ok()
}

#[cfg(debug_assertions)]
mod debug_secret_storage {
    use super::{AUTH_STATE_KEY, GITHUB_TOKEN_KEY, SERVICE_NAME, WEBDAV_PASSWORD_KEY};
    use serde::{Deserialize, Serialize};
    use std::fs::{self, OpenOptions};
    use std::io::{self, ErrorKind, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard};
    use uuid::Uuid;

    const AUTH_STORE_DIRECTORY: &str = "debug-auth";
    const GITHUB_STORE_DIRECTORY: &str = "debug-github-token";
    const WEBDAV_STORE_DIRECTORY: &str = "debug-webdav-password";
    const LOCATION_FILE: &str = "location.json";
    static STORE_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug, Deserialize, Serialize)]
    struct StorageLocation {
        directory: String,
        file: String,
    }

    impl StorageLocation {
        fn new() -> Self {
            Self {
                directory: Uuid::new_v4().to_string(),
                file: format!("{}.json", Uuid::new_v4()),
            }
        }

        fn is_valid(&self) -> bool {
            if Uuid::parse_str(&self.directory).is_err() {
                return false;
            }

            let Some(file_id) = self.file.strip_suffix(".json") else {
                return false;
            };
            Uuid::parse_str(file_id).is_ok()
        }

        fn credential_path(&self, root: &Path) -> PathBuf {
            root.join(&self.directory).join(&self.file)
        }
    }

    pub(super) fn get(key: &str) -> Option<String> {
        let root = storage_root(key)?;
        let _guard = lock_store();
        match read_secret_from_root(&root) {
            Ok(value) => value,
            Err(error) => {
                log::error!(target: "security", "调试凭据存储读取失败: {error}");
                if let Err(cleanup_error) = delete_store_root(&root) {
                    log::error!(target: "security", "损坏的调试凭据存储清理失败: {cleanup_error}");
                }
                None
            }
        }
    }

    pub(super) fn set(key: &str, value: &str) -> bool {
        let Some(root) = storage_root(key) else {
            return false;
        };
        let _guard = lock_store();
        match write_secret_to_root(&root, value) {
            Ok(()) => true,
            Err(error) => {
                log::error!(target: "security", "调试凭据存储写入失败: {error}");
                false
            }
        }
    }

    pub(super) fn delete(key: &str) -> bool {
        let Some(root) = storage_root(key) else {
            return false;
        };
        let _guard = lock_store();
        match delete_store_root(&root) {
            Ok(()) => true,
            Err(error) => {
                log::error!(target: "security", "调试凭据存储删除失败: {error}");
                false
            }
        }
    }

    pub(super) fn exists(key: &str) -> bool {
        let Some(root) = storage_root(key) else {
            return false;
        };
        let _guard = lock_store();
        read_location(&root)
            .ok()
            .flatten()
            .is_some_and(|location| location.credential_path(&root).is_file())
    }

    fn storage_root(key: &str) -> Option<PathBuf> {
        let directory = match key {
            AUTH_STATE_KEY => AUTH_STORE_DIRECTORY,
            GITHUB_TOKEN_KEY => GITHUB_STORE_DIRECTORY,
            WEBDAV_PASSWORD_KEY => WEBDAV_STORE_DIRECTORY,
            _ => return None,
        };
        dirs_next::cache_dir().map(|path| path.join(SERVICE_NAME).join(directory))
    }

    fn lock_store() -> MutexGuard<'static, ()> {
        STORE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read_secret_from_root(root: &Path) -> io::Result<Option<String>> {
        let Some(location) = read_location(root)? else {
            return Ok(None);
        };
        match fs::read_to_string(location.credential_path(root)) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                delete_store_root(root)?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn write_secret_to_root(root: &Path, value: &str) -> io::Result<()> {
        let location = match read_location(root) {
            Ok(Some(location)) => location,
            Ok(None) => {
                delete_store_root(root)?;
                StorageLocation::new()
            }
            Err(_) => {
                delete_store_root(root)?;
                StorageLocation::new()
            }
        };

        let credential_path = location.credential_path(root);
        let credential_directory = credential_path
            .parent()
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "调试凭据路径缺少父目录"))?;
        create_private_directory(root)?;
        create_private_directory(credential_directory)?;
        write_private_file(&credential_path, value.as_bytes())?;

        let serialized_location = serde_json::to_vec(&location)
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
        if let Err(error) = write_private_file(&root.join(LOCATION_FILE), &serialized_location) {
            let _ = delete_store_root(root);
            return Err(error);
        }
        Ok(())
    }

    fn read_location(root: &Path) -> io::Result<Option<StorageLocation>> {
        let bytes = match fs::read(root.join(LOCATION_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let location: StorageLocation = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
        if !location.is_valid() {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "调试凭据位置元数据无效",
            ));
        }
        Ok(Some(location))
    }

    fn create_private_directory(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(contents)?;
        file.sync_all()
    }

    fn delete_store_root(root: &Path) -> io::Result<()> {
        match fs::remove_dir_all(root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stores_cookie_in_random_directory_and_file() {
            let temp = tempfile::tempdir().expect("create temp directory");
            let root = temp.path().join(AUTH_STORE_DIRECTORY);

            write_secret_to_root(&root, "cookie-json").expect("write debug cookie");

            let location = read_location(&root)
                .expect("read location")
                .expect("location exists");
            assert!(Uuid::parse_str(&location.directory).is_ok());
            let file_id = location
                .file
                .strip_suffix(".json")
                .expect("random cookie file suffix");
            assert!(Uuid::parse_str(file_id).is_ok());
            assert_eq!(
                read_secret_from_root(&root).expect("read debug cookie"),
                Some("cookie-json".to_string())
            );
        }

        #[test]
        fn deleting_cookie_removes_random_storage() {
            let temp = tempfile::tempdir().expect("create temp directory");
            let root = temp.path().join(AUTH_STORE_DIRECTORY);
            write_secret_to_root(&root, "cookie-json").expect("write debug cookie");

            delete_store_root(&root).expect("delete debug cookie storage");

            assert!(!root.exists());
            assert_eq!(
                read_secret_from_root(&root).expect("read deleted debug cookie"),
                None
            );
        }

        #[test]
        fn rejects_location_outside_debug_storage() {
            let temp = tempfile::tempdir().expect("create temp directory");
            let root = temp.path().join(AUTH_STORE_DIRECTORY);
            let outside = temp.path().join("outside.json");
            fs::create_dir_all(&root).expect("create debug storage root");
            fs::write(&outside, "do-not-read").expect("write outside file");
            fs::write(
                root.join(LOCATION_FILE),
                r#"{"directory":"..","file":"outside.json"}"#,
            )
            .expect("write invalid location");

            let error = read_secret_from_root(&root).expect_err("reject invalid location");

            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert_eq!(
                fs::read_to_string(outside).expect("outside file remains"),
                "do-not-read"
            );
        }

        #[test]
        fn deleting_cookie_storage_keeps_sync_credentials() {
            let temp = tempfile::tempdir().expect("create temp directory");
            let auth_root = temp.path().join(AUTH_STORE_DIRECTORY);
            let github_root = temp.path().join(GITHUB_STORE_DIRECTORY);
            write_secret_to_root(&auth_root, "cookie-json").expect("write debug cookie");
            write_secret_to_root(&github_root, "github-token").expect("write debug token");

            delete_store_root(&auth_root).expect("delete debug cookie storage");

            assert!(!auth_root.exists());
            assert_eq!(
                read_secret_from_root(&github_root).expect("read debug token"),
                Some("github-token".to_string())
            );
        }

        #[test]
        fn known_secrets_use_independent_debug_directories() {
            let auth_root = storage_root(AUTH_STATE_KEY).expect("auth storage root");
            let github_root = storage_root(GITHUB_TOKEN_KEY).expect("GitHub storage root");
            let webdav_root = storage_root(WEBDAV_PASSWORD_KEY).expect("WebDAV storage root");

            assert_ne!(auth_root, github_root);
            assert_ne!(auth_root, webdav_root);
            assert_ne!(github_root, webdav_root);
            assert!(storage_root("unknown-secret").is_none());
        }
    }
}
