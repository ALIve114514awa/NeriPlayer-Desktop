// WebDAV API 客户端
use reqwest::Client;
use sha2::{Sha256, Digest};
use crate::error::{AppError, AppResult};

pub struct WebDavApiClient {
    http: Client,
    server_url: String,
    username: String,
    password: String,
    base_path: String,
}

const SYNC_FILENAME: &str = "neriplayer-sync.json";

impl WebDavApiClient {
    pub fn new(http: &Client, server_url: &str, username: &str, password: &str, base_path: &str) -> Self {
        Self {
            http: http.clone(),
            server_url: server_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
            base_path: base_path.to_string(),
        }
    }

    fn remote_url(&self) -> String {
        if self.base_path.is_empty() {
            format!("{}/{}", self.server_url, SYNC_FILENAME)
        } else {
            let bp = self.base_path.trim_matches('/');
            format!("{}/{}/{}", self.server_url, bp, SYNC_FILENAME)
        }
    }

    /// 指纹算在**原始字节**上
    ///
    /// 必须与 Android 的 `calculateFingerprint(ByteArray)` 一致：
    /// 一端算字符串、一端算字节，同一份内容会得出不同指纹，
    /// 双端就会各自认为「远端变了」而无限对传。
    fn sha256_fingerprint(content: &[u8]) -> String {
        let hash = Sha256::digest(content);
        hex::encode(hash)
    }

    /// 验证连接（GET 请求，200/404 均视为连接成功）
    pub async fn validate_connection(&self) -> AppResult<()> {
        let url = self.remote_url();
        let resp = self.http.get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send().await?;

        let status = resp.status().as_u16();
        match status {
            200 | 404 => Ok(()),
            401 | 403 => Err(AppError::Api("WebDAV authentication failed".into())),
            _ => Err(AppError::Api(format!("WebDAV connection failed ({})", status))),
        }
    }

    /// 获取文件内容和指纹
    /// 不存在时返回 Ok(None)
    pub async fn get_file_content(&self) -> AppResult<Option<(Vec<u8>, String)>> {
        let url = self.remote_url();
        let resp = self.http.get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send().await?;

        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if status == 401 || status == 403 {
            return Err(AppError::Api("WebDAV authentication failed".into()));
        }
        if !resp.status().is_success() {
            return Err(AppError::Api(format!("WebDAV GET failed ({})", status)));
        }

        // 按字节读：省流备份是 GZIP 二进制，走 text() 会被 UTF-8 转换毁掉
        let content = resp.bytes().await?.to_vec();
        let fingerprint = Self::sha256_fingerprint(&content);
        Ok(Some((content, fingerprint)))
    }

    /// 上传文件内容，返回 SHA-256 指纹
    ///
    /// 省流模式以 octet-stream 传原始 GZIP 字节，与 Android 对齐；
    /// 对二进制正文声明 `application/json` 会让部分服务端做转码甚至拒收。
    pub async fn update_file_content(&self, content: &[u8], data_saver: bool) -> AppResult<String> {
        let url = self.remote_url();
        // 省流传 GZIP 二进制，非省流传 JSON 文本；与 Android 的 mediaType 选择一致
        let media_type = if data_saver {
            "application/octet-stream"
        } else {
            "application/json; charset=utf-8"
        };
        let resp = self.http.put(&url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", media_type)
            .body(content.to_vec())
            .send().await?;

        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(AppError::Api("WebDAV authentication failed".into()));
        }
        // WebDAV PUT 成功通常返回 200/201/204
        if !resp.status().is_success() {
            return Err(AppError::Api(format!("WebDAV PUT failed ({})", status)));
        }

        Ok(Self::sha256_fingerprint(content))
    }
}
