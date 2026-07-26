// B站 API 客户端
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::error::{AppError, AppResult};
use crate::api::transport::{parse_json_response, FallbackHttp};
use super::wbi;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub struct BiliClient {
    http: FallbackHttp,
    mixin_key: parking_lot::Mutex<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiliAudioStream {
    pub url: String,
    pub bandwidth: u64,
    pub codecs: String,
    pub quality_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiliVideoInfo {
    pub bvid: String,
    pub title: String,
    pub owner: String,
    pub cover: String,
    pub cid: u64,
    pub duration: u64,
}

impl BiliClient {
    pub fn new(http: &Client) -> Self {
        Self::with_transport(FallbackHttp::new(http, "bilibili"))
    }

    pub fn with_transport(http: FallbackHttp) -> Self {
        Self {
            http,
            mixin_key: parking_lot::Mutex::new(None),
        }
    }

    /// 获取或刷新 mixin_key
    async fn ensure_mixin_key(&self) -> AppResult<String> {
        if let Some(ref key) = *self.mixin_key.lock() {
            return Ok(key.clone());
        }

        let resp: Value = self
            .http
            .send(|client| {
                client
                    .get("https://api.bilibili.com/x/web-interface/nav")
                    .header("User-Agent", USER_AGENT)
                    .header("Referer", "https://www.bilibili.com")
            })
            .await
            .map(|resp| parse_json_response::<Value>(resp, "bilibili nav"))?
            .await?;

        let img_url = resp["data"]["wbi_img"]["img_url"].as_str()
            .ok_or_else(|| AppError::Api("No wbi img_url".into()))?;
        let sub_url = resp["data"]["wbi_img"]["sub_url"].as_str()
            .ok_or_else(|| AppError::Api("No wbi sub_url".into()))?;

        // 提取文件名（去掉路径和扩展名）
        let img_key = img_url.rsplit('/').next().unwrap_or("")
            .split('.').next().unwrap_or("");
        let sub_key = sub_url.rsplit('/').next().unwrap_or("")
            .split('.').next().unwrap_or("");

        let key = wbi::get_mixin_key(img_key, sub_key);
        *self.mixin_key.lock() = Some(key.clone());
        Ok(key)
    }

    /// 带 Wbi 签名的 GET 请求
    async fn wbi_get(&self, url: &str, mut params: BTreeMap<String, String>) -> AppResult<Value> {
        let mixin_key = self.ensure_mixin_key().await?;
        wbi::sign_params(&mut params, &mixin_key);

        let query: String = params.iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let full_url = format!("{}?{}", url, query);
        let resp: Value = self
            .http
            .send(|client| {
                client
                    .get(&full_url)
                    .header("User-Agent", USER_AGENT)
                    .header("Referer", "https://www.bilibili.com")
            })
            .await
            .map(|response| parse_json_response::<Value>(response, "bilibili wbi"))?
            .await?;

        if resp["code"].as_i64() != Some(0) {
            return Err(AppError::Api(format!("Bili API error: {}", resp["message"])));
        }
        Ok(resp)
    }

    /// 获取视频信息
    pub async fn get_video_info(&self, bvid: &str) -> AppResult<BiliVideoInfo> {
        let mut params = BTreeMap::new();
        params.insert("bvid".into(), bvid.into());

        let resp = self.wbi_get("https://api.bilibili.com/x/web-interface/wbi/view", params).await?;
        let data = &resp["data"];

        Ok(BiliVideoInfo {
            bvid: bvid.to_string(),
            title: data["title"].as_str().unwrap_or("").to_string(),
            owner: data["owner"]["name"].as_str().unwrap_or("").to_string(),
            cover: data["pic"].as_str().unwrap_or("").to_string(),
            cid: data["cid"].as_u64().unwrap_or(0),
            duration: data["duration"].as_u64().unwrap_or(0),
        })
    }

    /// 获取音频流 URL（DASH 模式）
    pub async fn get_audio_url(&self, bvid: &str, cid: u64) -> AppResult<Vec<BiliAudioStream>> {
        let mut params = BTreeMap::new();
        params.insert("bvid".into(), bvid.into());
        params.insert("cid".into(), cid.to_string());
        params.insert("fnval".into(), "272".into()); // DASH + Dolby
        params.insert("fnver".into(), "0".into());
        params.insert("fourk".into(), "0".into());
        params.insert("otype".into(), "json".into());
        params.insert("platform".into(), "pc".into());

        let resp = self.wbi_get("https://api.bilibili.com/x/player/wbi/playurl", params).await?;
        let dash = &resp["data"]["dash"];

        let mut streams = Vec::new();

        // 普通音频流
        if let Some(audios) = dash["audio"].as_array() {
            for a in audios {
                if let Some(url) = a["baseUrl"].as_str().or(a["base_url"].as_str()) {
                    streams.push(BiliAudioStream {
                        url: url.to_string(),
                        bandwidth: a["bandwidth"].as_u64().unwrap_or(0),
                        codecs: a["codecs"].as_str().unwrap_or("").to_string(),
                        quality_id: a["id"].as_u64().unwrap_or(0) as u32,
                    });
                }
            }
        }

        // FLAC 无损
        if let Some(flac) = dash["flac"]["audio"].as_object() {
            if let Some(url) = flac.get("baseUrl").or(flac.get("base_url")).and_then(|v| v.as_str()) {
                streams.push(BiliAudioStream {
                    url: url.to_string(),
                    bandwidth: flac.get("bandwidth").and_then(|v| v.as_u64()).unwrap_or(0),
                    codecs: "flac".to_string(),
                    quality_id: 30251,
                });
            }
        }

        // Dolby
        if let Some(dolby_audios) = dash["dolby"]["audio"].as_array() {
            for a in dolby_audios {
                if let Some(url) = a["baseUrl"].as_str().or(a["base_url"].as_str()) {
                    streams.push(BiliAudioStream {
                        url: url.to_string(),
                        bandwidth: a["bandwidth"].as_u64().unwrap_or(0),
                        codecs: "ec-3".to_string(),
                        quality_id: 30250,
                    });
                }
            }
        }

        // 按码率降序
        streams.sort_by_key(|stream| std::cmp::Reverse(stream.bandwidth));
        Ok(streams)
    }

    /// 搜索视频
    pub async fn search(&self, keyword: &str) -> AppResult<Value> {
        let mut params = BTreeMap::new();
        params.insert("search_type".into(), "video".into());
        params.insert("keyword".into(), keyword.into());
        params.insert("page".into(), "1".into());

        self.wbi_get("https://api.bilibili.com/x/web-interface/wbi/search/type", params).await
    }

    // 需要登录的 API
    /// 获取登录用户信息（也用于 Wbi key 刷新）
    pub async fn get_user_info(&self) -> AppResult<Value> {
        let resp = self
            .http
            .send(|client| {
                client
                    .get("https://api.bilibili.com/x/web-interface/nav")
                    .header("User-Agent", USER_AGENT)
                    .header("Referer", "https://www.bilibili.com")
            })
            .await?;
        parse_json_response(resp, "bilibili nav").await
    }

    /// 获取用户创建的收藏夹列表（分页版，包含封面）
    pub async fn get_user_favorites(&self, mid: u64) -> AppResult<Value> {
        let mut params = BTreeMap::new();
        params.insert("up_mid".into(), mid.to_string());
        params.insert("pn".into(), "1".into());
        params.insert("ps".into(), "50".into());
        self.wbi_get("https://api.bilibili.com/x/v3/fav/folder/created/list", params).await
    }

    /// 获取收藏夹内容
    pub async fn get_favorite_items(&self, media_id: u64, page: u32) -> AppResult<Value> {
        let mut params = BTreeMap::new();
        params.insert("media_id".into(), media_id.to_string());
        params.insert("pn".into(), page.to_string());
        params.insert("ps".into(), "20".into());
        params.insert("platform".into(), "web".into());
        self.wbi_get("https://api.bilibili.com/x/v3/fav/resource/list", params).await
    }

    /// 验证登录会话是否有效
    pub async fn validate_session(&self) -> AppResult<bool> {
        let resp = self.get_user_info().await?;
        // code == 0 且 isLogin == true 表示会话有效
        let is_login = resp["data"]["isLogin"].as_bool().unwrap_or(false);
        Ok(is_login)
    }

    /// 获取单个收藏夹信息
    pub async fn get_fav_folder_info(&self, media_id: u64) -> AppResult<Value> {
        let mut params = BTreeMap::new();
        params.insert("media_id".into(), media_id.to_string());
        self.wbi_get("https://api.bilibili.com/x/v3/fav/folder/info", params).await
    }

    /// 按 avid 获取视频信息
    pub async fn get_video_info_by_avid(&self, avid: u64) -> AppResult<BiliVideoInfo> {
        let mut params = BTreeMap::new();
        params.insert("aid".into(), avid.to_string());

        let resp = self.wbi_get("https://api.bilibili.com/x/web-interface/wbi/view", params).await?;
        let data = &resp["data"];

        Ok(BiliVideoInfo {
            bvid: data["bvid"].as_str().unwrap_or("").to_string(),
            title: data["title"].as_str().unwrap_or("").to_string(),
            owner: data["owner"]["name"].as_str().unwrap_or("").to_string(),
            cover: data["pic"].as_str().unwrap_or("").to_string(),
            cid: data["cid"].as_u64().unwrap_or(0),
            duration: data["duration"].as_u64().unwrap_or(0),
        })
    }

    /// 获取视频分 P 列表
    pub async fn get_video_pages(&self, bvid: &str) -> AppResult<Value> {
        let mut params = BTreeMap::new();
        params.insert("bvid".into(), bvid.into());
        self.wbi_get("https://api.bilibili.com/x/player/wbi/pagelist", params).await
    }

    /// 按 Android 旧式分 P 编号查找对应 cid
    pub async fn get_video_page_cid(&self, bvid: &str, page: u64) -> AppResult<Option<u64>> {
        let response = self.get_video_pages(bvid).await?;
        Ok(find_video_page_cid(&response, page))
    }
}

fn find_video_page_cid(response: &Value, page: u64) -> Option<u64> {
    response["data"]
        .as_array()?
        .iter()
        .find(|item| item["page"].as_u64() == Some(page))
        .and_then(|item| item["cid"].as_u64())
}

#[cfg(test)]
mod tests {
    use super::find_video_page_cid;
    use serde_json::json;

    #[test]
    fn finds_cid_for_legacy_page_number() {
        let response = json!({
            "data": [
                { "page": 1, "cid": 101 },
                { "page": 7, "cid": 707 }
            ]
        });

        assert_eq!(find_video_page_cid(&response, 7), Some(707));
        assert_eq!(find_video_page_cid(&response, 2), None);
    }
}
