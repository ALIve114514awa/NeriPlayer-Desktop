use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, REFERER};
use tauri::State;
use url::Url;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const MAX_COVER_BYTES: u64 = 8 * 1024 * 1024;
const BILIBILI_REFERER: &str = "https://www.bilibili.com/";

#[tauri::command]
pub async fn fetch_bilibili_cover(url: String, state: State<'_, AppState>) -> AppResult<String> {
    let cover_url = normalize_bilibili_cover_url(&url)?;
    let response = state
        .http()
        .get(cover_url)
        .header(REFERER, BILIBILI_REFERER)
        .header(ACCEPT, "image/avif,image/webp,image/apng,image/*,*/*;q=0.8")
        .send()
        .await?
        .error_for_status()?;

    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_COVER_BYTES)
    {
        return Err(AppError::Api("Bilibili cover is too large".into()));
    }

    let mime_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| value.starts_with("image/"))
        .ok_or_else(|| AppError::Api("Bilibili cover response is not an image".into()))?
        .to_string();

    let bytes = response.bytes().await?;
    if bytes.len() as u64 > MAX_COVER_BYTES {
        return Err(AppError::Api("Bilibili cover is too large".into()));
    }

    Ok(format!(
        "data:{};base64,{}",
        mime_type,
        STANDARD.encode(bytes)
    ))
}

fn normalize_bilibili_cover_url(raw_url: &str) -> AppResult<Url> {
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return Err(AppError::Api("Bilibili cover URL is empty".into()));
    }

    let normalized = if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_string()
    };
    let mut url = Url::parse(&normalized)
        .map_err(|_| AppError::Api("Bilibili cover URL is invalid".into()))?;
    let host = url
        .host_str()
        .ok_or_else(|| AppError::Api("Bilibili cover URL has no host".into()))?;

    if !is_allowed_bilibili_image_host(host) {
        return Err(AppError::Api("Bilibili cover host is not allowed".into()));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(AppError::Api("Bilibili cover port is not allowed".into()));
    }
    if url.scheme() == "http" {
        url.set_scheme("https")
            .map_err(|_| AppError::Api("Failed to upgrade Bilibili cover URL".into()))?;
    } else if url.scheme() != "https" {
        return Err(AppError::Api("Bilibili cover URL must use HTTPS".into()));
    }

    Ok(url)
}

fn is_allowed_bilibili_image_host(host: &str) -> bool {
    ["hdslb.com", "biliimg.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::normalize_bilibili_cover_url;

    #[test]
    fn normalizes_protocol_relative_url() {
        let url = normalize_bilibili_cover_url("//i0.hdslb.com/bfs/archive/test.jpg")
            .expect("URL should be accepted");
        assert_eq!(url.as_str(), "https://i0.hdslb.com/bfs/archive/test.jpg");
    }

    #[test]
    fn upgrades_allowed_http_url() {
        let url = normalize_bilibili_cover_url("http://archive.biliimg.com/test.webp")
            .expect("URL should be accepted");
        assert_eq!(url.as_str(), "https://archive.biliimg.com/test.webp");
    }

    #[test]
    fn rejects_non_bilibili_host() {
        assert!(normalize_bilibili_cover_url("https://example.com/test.jpg").is_err());
    }

    #[test]
    fn rejects_non_https_port() {
        assert!(normalize_bilibili_cover_url("https://i0.hdslb.com:8443/test.jpg").is_err());
    }
}
