// GitHub Contents API 客户端
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::{Client, RequestBuilder, StatusCode};

use crate::error::AppError;

const GITHUB_API_BASE: &str = "https://api.github.com";
const MAX_SYNC_FILE_BYTES: u64 = 12 * 1024 * 1024;
const MAX_API_ERROR_CHARS: usize = 240;

pub type GitHubResult<T> = Result<T, GitHubApiError>;

#[derive(Debug, thiserror::Error)]
pub enum GitHubApiError {
    #[error("GitHub token expired or invalid")]
    TokenExpired,
    #[error("GitHub resource not found: {0}")]
    NotFound(String),
    #[error("GitHub content conflict ({status}): {message}")]
    ContentConflict { status: u16, message: String },
    #[error("GitHub API request failed ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("Invalid GitHub API response: {0}")]
    InvalidResponse(String),
    #[error("GitHub network error: {0}")]
    Network(#[from] reqwest::Error),
}

impl GitHubApiError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    pub fn is_content_conflict(&self) -> bool {
        matches!(self, Self::ContentConflict { .. })
    }
}

impl From<GitHubApiError> for AppError {
    fn from(error: GitHubApiError) -> Self {
        match error {
            GitHubApiError::TokenExpired => AppError::Api("GitHub token expired or invalid".into()),
            GitHubApiError::NotFound(message) => AppError::NotFound(message),
            GitHubApiError::ContentConflict { status, message } => {
                AppError::Api(format!("GitHub content conflict ({}): {}", status, message))
            }
            GitHubApiError::Api { status, message } => AppError::Api(format!(
                "GitHub API request failed ({}): {}",
                status, message
            )),
            GitHubApiError::InvalidResponse(message) => AppError::Other(message),
            GitHubApiError::Network(error) => AppError::Network(error),
        }
    }
}

pub struct GitHubApiClient {
    http: Client,
    token: String,
    api_base: String,
}

impl GitHubApiClient {
    pub fn new(http: &Client, token: &str) -> Self {
        Self::with_api_base(http, token, GITHUB_API_BASE)
    }

    fn with_api_base(http: &Client, token: &str, api_base: &str) -> Self {
        Self {
            http: http.clone(),
            token: token.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_api_base(http: &Client, token: &str, api_base: &str) -> Self {
        Self::with_api_base(http, token, api_base)
    }

    fn request(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.api_base, path.trim_start_matches('/'))
    }

    /// 验证 token，返回用户名
    pub async fn validate_token(&self) -> GitHubResult<String> {
        let response = self
            .request(self.http.get(self.endpoint("user")))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if !status.is_success() {
            return Err(api_error(status, body, "validate token", false));
        }

        let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
            GitHubApiError::InvalidResponse(format!("failed to parse GitHub user: {}", error))
        })?;
        value["login"]
            .as_str()
            .filter(|login| !login.is_empty())
            .map(String::from)
            .ok_or_else(|| GitHubApiError::InvalidResponse("missing GitHub username".into()))
    }

    /// 创建私有仓库
    pub async fn create_repository(&self, repo_name: &str) -> GitHubResult<()> {
        let body = serde_json::json!({
            "name": repo_name,
            "private": true,
            "auto_init": true,
            "description": "NeriPlayer backup data"
        });
        let response = self
            .request(self.http.post(self.endpoint("user/repos")))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let response_body = response.text().await?;

        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if status.is_success()
            || (status == StatusCode::UNPROCESSABLE_ENTITY
                && response_body.contains("already exists"))
        {
            return Ok(());
        }
        Err(api_error(status, response_body, "create repository", false))
    }

    /// 检查仓库是否存在，返回默认分支名
    pub async fn check_repository(&self, owner: &str, repo: &str) -> GitHubResult<String> {
        let response = self
            .request(
                self.http
                    .get(self.endpoint(&format!("repos/{}/{}", owner, repo))),
            )
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(GitHubApiError::NotFound(format!("{}/{}", owner, repo)));
        }
        if !status.is_success() {
            return Err(api_error(status, body, "check repository", false));
        }

        let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
            GitHubApiError::InvalidResponse(format!("failed to parse GitHub repository: {}", error))
        })?;
        Ok(value["default_branch"]
            .as_str()
            .filter(|branch| !branch.is_empty())
            .unwrap_or("main")
            .to_string())
    }

    /// 获取文件内容和 SHA，只有 404 会返回 None
    pub async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
    ) -> GitHubResult<Option<(String, String)>> {
        let response = self
            .request(
                self.http
                    .get(self.endpoint(&format!("repos/{}/{}/contents/{}", owner, repo, path))),
            )
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if !status.is_success() {
            return Err(api_error(status, body, "get file", false));
        }

        let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
            GitHubApiError::InvalidResponse(format!(
                "failed to parse GitHub file response: {}",
                error
            ))
        })?;
        let size = value["size"].as_u64().unwrap_or(0);
        if size > MAX_SYNC_FILE_BYTES {
            return Err(GitHubApiError::InvalidResponse(
                "remote backup file is too large".into(),
            ));
        }
        let content_b64 = value["content"]
            .as_str()
            .ok_or_else(|| GitHubApiError::InvalidResponse("missing file content".into()))?;
        let sha = value["sha"]
            .as_str()
            .filter(|sha| !sha.is_empty())
            .ok_or_else(|| GitHubApiError::InvalidResponse("missing file SHA".into()))?
            .to_string();

        let cleaned: String = content_b64
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();

        // Contents API 对超过 1MB 的文件只回 size, content 为空串且 encoding=none
        // 此时必须改走 raw 媒体类型, 否则会被误判成"文件损坏"
        if cleaned.is_empty() && size > 0 {
            log::info!(
                target: "sync",
                "GitHub contents API omitted inline content (size={}), falling back to raw",
                size,
            );
            let content = self.get_file_raw(owner, repo, path).await?;
            return Ok(Some((content, sha)));
        }

        let decoded = BASE64.decode(&cleaned).map_err(|error| {
            GitHubApiError::InvalidResponse(format!("failed to decode file content: {}", error))
        })?;
        if decoded.len() as u64 > MAX_SYNC_FILE_BYTES {
            return Err(GitHubApiError::InvalidResponse(
                "remote backup file is too large".into(),
            ));
        }
        let content = String::from_utf8(decoded).map_err(|error| {
            GitHubApiError::InvalidResponse(format!("file content is not UTF-8: {}", error))
        })?;

        Ok(Some((content, sha)))
    }

    /// 以 raw 媒体类型读取文件正文, 绕开 Contents API 的 1MB 内联上限
    async fn get_file_raw(&self, owner: &str, repo: &str, path: &str) -> GitHubResult<String> {
        // 不能复用 request(): reqwest 的 header() 是追加语义,
        // 追加第二个 Accept 会让 GitHub 仍按 JSON 返回
        let response = self
            .http
            .get(self.endpoint(&format!("repos/{}/{}/contents/{}", owner, repo, path)))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github.raw")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        let status = response.status();

        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(GitHubApiError::NotFound(path.to_string()));
        }
        if !status.is_success() {
            let body = response.text().await?;
            return Err(api_error(status, body, "get raw file", false));
        }

        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_SYNC_FILE_BYTES {
            return Err(GitHubApiError::InvalidResponse(
                "remote backup file is too large".into(),
            ));
        }
        String::from_utf8(bytes.to_vec()).map_err(|error| {
            GitHubApiError::InvalidResponse(format!("file content is not UTF-8: {}", error))
        })
    }

    /// 创建或更新文件，sha 为空时表示新建
    pub async fn update_file_content(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        content: &str,
        sha: &str,
        message: &str,
    ) -> GitHubResult<String> {
        let branch = self.check_repository(owner, repo).await?;
        let mut body = serde_json::json!({
            "message": message,
            "content": BASE64.encode(content.as_bytes()),
            "branch": branch,
        });
        if !sha.is_empty() {
            body["sha"] = serde_json::Value::String(sha.to_string());
        }

        let response = self
            .request(
                self.http
                    .put(self.endpoint(&format!("repos/{}/{}/contents/{}", owner, repo, path))),
            )
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let response_body = response.text().await?;

        if status == StatusCode::UNAUTHORIZED {
            return Err(GitHubApiError::TokenExpired);
        }
        if !status.is_success() {
            return Err(api_error(status, response_body, "update file", true));
        }

        let value: serde_json::Value = serde_json::from_str(&response_body).map_err(|error| {
            GitHubApiError::InvalidResponse(format!(
                "failed to parse GitHub update response: {}",
                error
            ))
        })?;
        value["content"]["sha"]
            .as_str()
            .filter(|sha| !sha.is_empty())
            .map(String::from)
            .ok_or_else(|| GitHubApiError::InvalidResponse("missing updated file SHA".into()))
    }
}

fn api_error(
    status: StatusCode,
    body: String,
    operation: &str,
    detect_content_conflict: bool,
) -> GitHubApiError {
    let message = safe_api_error_message(status, &body, operation);
    let status = status.as_u16();
    if detect_content_conflict
        && (status == 409 || (status == 422 && message.to_ascii_lowercase().contains("sha")))
    {
        GitHubApiError::ContentConflict { status, message }
    } else {
        GitHubApiError::Api { status, message }
    }
}

fn safe_api_error_message(status: StatusCode, body: &str, operation: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return operation.to_string();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(message) = value.get("message").and_then(serde_json::Value::as_str) {
            return format!("{}: {}", operation, truncate_error_text(message));
        }
    }
    if looks_like_html(trimmed) {
        let description = if status.is_server_error() {
            "GitHub service is temporarily unavailable"
        } else {
            "GitHub returned an unexpected HTML response"
        };
        return format!("{}: {}", operation, description);
    }
    format!("{}: {}", operation, truncate_error_text(trimmed))
}

fn looks_like_html(value: &str) -> bool {
    let normalized = value.trim_start().to_ascii_lowercase();
    normalized.starts_with("<!doctype html")
        || normalized.starts_with("<html")
        || normalized.contains("<body")
        || normalized.contains("<style")
}

fn truncate_error_text(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_API_ERROR_CHARS {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(MAX_API_ERROR_CHARS.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    async fn mock_server(
        responses: Vec<String>,
    ) -> (String, mpsc::Receiver<String>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel(responses.len().max(1));
        let handle = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4);
                    let Some(header_end) = header_end else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                let _ = request_tx
                    .send(String::from_utf8_lossy(&request).into_owned())
                    .await;
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (format!("http://{}", address), request_rx, handle)
    }

    /// 测试必须绕开系统代理：mock server 监听 127.0.0.1，
    /// reqwest 默认会读取 macOS/Windows 的系统代理设置并把回环请求也发给代理
    fn loopback_client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("failed to build loopback test client")
    }

    fn response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        )
    }

    #[tokio::test]
    async fn get_file_only_maps_not_found_to_none() {
        let (base, _, server) = mock_server(vec![response("404 Not Found", "{}")]).await;
        let api = GitHubApiClient::new_with_api_base(&loopback_client(), "token", &base);

        let result = api.get_file_content("owner", "repo", "backup.bin").await;

        assert!(matches!(result, Ok(None)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn get_file_falls_back_to_raw_when_contents_api_omits_inline_content() {
        // Contents API 对 >1MB 的文件只回 size，content 为空串
        let (base, mut requests, server) = mock_server(vec![
            response(
                "200 OK",
                r#"{"size":2000000,"sha":"deadbeef","content":"","encoding":"none"}"#,
            ),
            response("200 OK", "RAW-BACKUP-PAYLOAD"),
        ])
        .await;
        let api = GitHubApiClient::new_with_api_base(&loopback_client(), "token", &base);

        let result = api
            .get_file_content("owner", "repo", "backup.bin")
            .await
            .unwrap();

        assert_eq!(
            result,
            Some(("RAW-BACKUP-PAYLOAD".to_string(), "deadbeef".to_string()))
        );
        let _json_request = requests.recv().await.unwrap();
        let raw_request = requests.recv().await.unwrap();
        assert!(raw_request.contains("application/vnd.github.raw"));
        // 追加第二个 Accept 会让 GitHub 仍按 JSON 返回，必须只带 raw 这一个
        assert!(!raw_request.contains("application/vnd.github+json"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn get_file_keeps_inline_content_when_present() {
        let (base, _, server) = mock_server(vec![response(
            "200 OK",
            r#"{"size":6,"sha":"abc123","content":"aW5saW5l","encoding":"base64"}"#,
        )])
        .await;
        let api = GitHubApiClient::new_with_api_base(&loopback_client(), "token", &base);

        let result = api
            .get_file_content("owner", "repo", "backup.bin")
            .await
            .unwrap();

        assert_eq!(result, Some(("inline".to_string(), "abc123".to_string())));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn get_file_propagates_server_errors() {
        let (base, _, server) = mock_server(vec![response(
            "500 Internal Server Error",
            r#"{"message":"temporary failure"}"#,
        )])
        .await;
        let api = GitHubApiClient::new_with_api_base(&loopback_client(), "token", &base);

        let error = api
            .get_file_content("owner", "repo", "backup.bin")
            .await
            .unwrap_err();

        assert!(matches!(error, GitHubApiError::Api { status: 500, .. }));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn update_uses_default_branch_and_classifies_sha_conflict() {
        let (base, mut requests, server) = mock_server(vec![
            response("200 OK", r#"{"default_branch":"sync-data"}"#),
            response("409 Conflict", r#"{"message":"sha does not match"}"#),
        ])
        .await;
        let api = GitHubApiClient::new_with_api_base(&loopback_client(), "token", &base);

        let error = api
            .update_file_content("owner", "repo", "backup.bin", "content", "old-sha", "sync")
            .await
            .unwrap_err();

        assert!(error.is_content_conflict());
        let _repository_request = requests.recv().await.unwrap();
        let update_request = requests.recv().await.unwrap();
        assert!(update_request.contains("\"branch\":\"sync-data\""));
        assert!(update_request.contains("\"sha\":\"old-sha\""));
        server.await.unwrap();
    }
}
