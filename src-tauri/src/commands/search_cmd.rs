use crate::api::bilibili::client::BiliClient;
use crate::api::lrclib::LrcLibClient;
use crate::api::netease::client::NeteaseClient;
use crate::api::qq::client::QqMusicClient;
use crate::error::AppResult;
use crate::state::AppState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub source: String,
    pub cover_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synced_lyrics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plain_lyrics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translated_lyrics: Option<String>,
}

#[tauri::command]
pub async fn search(
    query: String,
    platform: String,
    include_lyrics: Option<bool>,
    state: State<'_, AppState>,
) -> AppResult<Vec<SearchResult>> {
    let include_lyrics = include_lyrics.unwrap_or(false);
    match platform.as_str() {
        "netease" => search_netease(&query, &state).await,
        "qq" => search_qq(&query, &state, include_lyrics).await,
        "bilibili" => search_bilibili(&query, &state).await,
        "youtube" => search_youtube(&query, &state).await,
        "lrclib" => search_lrclib(&query, &state).await,
        // 全平台搜索
        _ => {
            let mut all = Vec::new();
            // 依次搜索，某个平台失败不影响其他
            if let Ok(mut r) = search_netease(&query, &state).await {
                all.append(&mut r);
            }
            if let Ok(mut r) = search_qq(&query, &state, include_lyrics).await {
                all.append(&mut r);
            }
            if let Ok(mut r) = search_bilibili(&query, &state).await {
                all.append(&mut r);
            }
            Ok(all)
        }
    }
}

async fn search_netease(query: &str, state: &State<'_, AppState>) -> AppResult<Vec<SearchResult>> {
    let client = NeteaseClient::new(&state.http());
    let results = client.search(query, 30, 0).await?;

    Ok(results
        .into_iter()
        .map(|r| SearchResult {
            id: format!("netease:{}", r.id),
            title: r.name,
            artist: r.artists.join(" / "),
            album: r.album,
            duration_ms: r.duration_ms,
            source: "netease".into(),
            cover_url: r.cover_url,
            synced_lyrics: None,
            plain_lyrics: None,
            translated_lyrics: None,
        })
        .collect())
}

async fn search_qq(
    query: &str,
    state: &State<'_, AppState>,
    include_lyrics: bool,
) -> AppResult<Vec<SearchResult>> {
    let client = QqMusicClient::new(&state.http());
    let mut songs = client.search(query, 1, 20).await?;
    songs.truncate(12);

    let mut results = Vec::with_capacity(songs.len());
    for song in songs {
        let cover_url = song.album_mid.as_ref().map(|mid| {
            format!(
                "https://y.qq.com/music/photo_new/T002R800x800M000{}.jpg",
                mid
            )
        });

        // QQ 歌词接口较慢：探索/首页搜索默认不预取，仅播放页补全按需开启
        let (synced_lyrics, translated_lyrics) = if include_lyrics {
            client
                .get_lyrics(&song.song_mid)
                .await
                .unwrap_or((None, None))
        } else {
            (None, None)
        };

        results.push(SearchResult {
            id: format!("qq:{}", song.song_mid),
            title: song.song_name,
            artist: song.artists.join(" / "),
            album: song.album_name,
            duration_ms: song.duration_ms,
            source: "qq".into(),
            cover_url,
            synced_lyrics,
            plain_lyrics: None,
            translated_lyrics,
        });
    }

    Ok(results)
}

async fn search_bilibili(query: &str, state: &State<'_, AppState>) -> AppResult<Vec<SearchResult>> {
    let client = BiliClient::new(&state.http());
    let resp = client.search(query).await?;

    let results = resp["data"]["result"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            let bvid = item["bvid"].as_str()?;
            // 清理 HTML 高亮标签
            let title = item["title"]
                .as_str()
                .unwrap_or("")
                .replace("<em class=\"keyword\">", "")
                .replace("</em>", "");
            let author = item["author"].as_str().unwrap_or("").to_string();
            let duration: u64 = {
                // B站返回 "mm:ss" 格式
                let d = item["duration"].as_str().unwrap_or("0:00");
                let parts: Vec<&str> = d.split(':').collect();
                if parts.len() == 2 {
                    let m: u64 = parts[0].parse().unwrap_or(0);
                    let s: u64 = parts[1].parse().unwrap_or(0);
                    (m * 60 + s) * 1000
                } else {
                    0
                }
            };
            let cover = item["pic"].as_str().map(|s| {
                if s.starts_with("//") {
                    format!("https:{}", s)
                } else {
                    s.to_string()
                }
            });

            Some(SearchResult {
                id: format!("bilibili:{}", bvid),
                title,
                artist: author,
                album: String::new(),
                duration_ms: duration,
                source: "bilibili".into(),
                cover_url: cover,
                synced_lyrics: None,
                plain_lyrics: None,
                translated_lyrics: None,
            })
        })
        .collect();

    Ok(results)
}

async fn search_youtube(_query: &str, state: &State<'_, AppState>) -> AppResult<Vec<SearchResult>> {
    let client = crate::api::youtube::client::YouTubeClient::new(&state.http());
    let resp = client.search(_query).await?;

    // InnerTube 搜索结果解析
    let mut results = Vec::new();
    if let Some(contents) = resp["contents"]["tabbedSearchResultsRenderer"]["tabs"]
        .get(0)
        .and_then(|t| t["tabRenderer"]["content"]["sectionListRenderer"]["contents"].as_array())
    {
        for section in contents {
            if let Some(items) = section["musicShelfRenderer"]["contents"].as_array() {
                for item in items {
                    let renderer = &item["musicResponsiveListItemRenderer"];
                    // 提取 videoId
                    let video_id = renderer["overlay"]["musicItemThumbnailOverlayRenderer"]
                        ["content"]["musicPlayButtonRenderer"]["playNavigationEndpoint"]
                        ["watchEndpoint"]["videoId"]
                        .as_str()
                        .or_else(|| {
                            renderer["flexColumns"].get(0).and_then(|c| {
                                c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
                                    .get(0)
                                    .and_then(|r| {
                                        r["navigationEndpoint"]["watchEndpoint"]["videoId"].as_str()
                                    })
                            })
                        });

                    if let Some(vid) = video_id {
                        let title = renderer["flexColumns"]
                            .get(0)
                            .and_then(|c| {
                                c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
                                    .get(0)
                                    .and_then(|r| r["text"].as_str())
                            })
                            .unwrap_or("")
                            .to_string();

                        let artist = renderer["flexColumns"]
                            .get(1)
                            .and_then(|c| {
                                c["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
                                    .get(0)
                                    .and_then(|r| r["text"].as_str())
                            })
                            .unwrap_or("")
                            .to_string();

                        let thumbnail = renderer["thumbnail"]["musicThumbnailRenderer"]
                            ["thumbnail"]["thumbnails"]
                            .as_array()
                            .and_then(|arr| arr.last())
                            .and_then(|t| t["url"].as_str())
                            .map(String::from);

                        results.push(SearchResult {
                            id: format!("youtube:{}", vid),
                            title,
                            artist,
                            album: String::new(),
                            duration_ms: 0, // InnerTube 不直接返回时长
                            source: "youtube".into(),
                            cover_url: thumbnail,
                            synced_lyrics: None,
                            plain_lyrics: None,
                            translated_lyrics: None,
                        });
                    }
                }
            }
        }
    }

    Ok(results)
}

async fn search_lrclib(query: &str, state: &State<'_, AppState>) -> AppResult<Vec<SearchResult>> {
    let client = LrcLibClient::new(&state.http());
    let results = client.search(query).await?;

    Ok(results
        .into_iter()
        .map(|r| SearchResult {
            id: format!("lrclib:{}", r.id),
            title: r.track_name,
            artist: r.artist_name,
            album: String::new(),
            duration_ms: (r.duration * 1000.0) as u64,
            source: "lrclib".into(),
            cover_url: None,
            synced_lyrics: r.synced_lyrics,
            plain_lyrics: r.plain_lyrics,
            translated_lyrics: None,
        })
        .collect())
}
