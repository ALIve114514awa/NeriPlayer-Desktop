// Release 构建使用系统凭据存储；Debug 构建完全使用随机路径明文文件
// 各类凭据使用独立目录，删除 Cookie 时不会影响同步凭据

const SERVICE_NAME: &str = "moe.ouom.neriplayer.desktop";

pub const AUTH_STATE_KEY: &str = "auth-state-v1";
pub const GITHUB_TOKEN_KEY: &str = "github-token-v1";
pub const WEBDAV_PASSWORD_KEY: &str = "webdav-password-v1";

/// 从当前构建配置的凭据存储读取秘密值
pub fn get_secret(key: &str) -> Option<String> {
    #[cfg(debug_assertions)]
    {
        debug_secret_storage::get(key)
    }

    #[cfg(not(debug_assertions))]
    {
        get_keyring_secret(key)
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
        set_keyring_secret(key, value)
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
        delete_keyring_secret(key)
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
                log::error!("调试凭据存储读取失败: {error}");
                if let Err(cleanup_error) = delete_store_root(&root) {
                    log::error!("损坏的调试凭据存储清理失败: {cleanup_error}");
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
                log::error!("调试凭据存储写入失败: {error}");
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
                log::error!("调试凭据存储删除失败: {error}");
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
