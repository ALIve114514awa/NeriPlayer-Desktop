// 歌词管理器 — 多源瀑布获取
use crate::api::lrclib::LrcLibClient;
use crate::api::netease::client::NeteaseClient;
use crate::api::qq::client::QqMusicClient;
use crate::error::AppResult;
use crate::lyrics::parser::{self, LyricLine};
use std::path::{Path, PathBuf};

pub struct LyricsManager {
    http: reqwest::Client,
}

impl LyricsManager {
    pub fn new(http: &reqwest::Client) -> Self {
        Self { http: http.clone() }
    }

    /// 多源获取歌词：本地 sidecar -> 网易云 API -> LRCLIB fallback
    pub async fn fetch_lyrics(
        &self,
        track_title: &str,
        track_artist: &str,
        duration_secs: u64,
        audio_path: Option<&str>,
        netease_id: Option<u64>,
        qq_song_mid: Option<&str>,
    ) -> AppResult<Vec<LyricLine>> {
        log::info!(
            target: "lyrics",
            "fetch: title={}, artist={}, dur={}s, netease_id={:?}, qq_song_mid={:?}",
            track_title, track_artist, duration_secs, netease_id, qq_song_mid
        );

        // 尝试本地 sidecar 歌词（对齐 Android LocalMediaSupport.findNearbyLyrics）
        if let Some(path) = audio_path {
            if let Some(lines) = load_local_sidecar_lyrics(path) {
                log::info!(target: "lyrics", "found local sidecar: {} lines", lines.len());
                return Ok(lines);
            }
        }

        // QQ 音乐：如果调用方提供 songmid，优先直接取 QQ 歌词（含翻译）
        if let Some(song_mid) = qq_song_mid.filter(|id| !id.trim().is_empty()) {
            let qq = QqMusicClient::new(&self.http);
            match self.parse_qq_lyrics(&qq, song_mid).await {
                Ok(Some(lines)) => return Ok(lines),
                Ok(None) => log::info!(target: "lyrics", "QQ lyrics empty for song_mid={}", song_mid),
                Err(e) => log::warn!(
                    target: "lyrics",
                    "QQ get_lyrics failed for song_mid={}: {}",
                    song_mid, e
                ),
            }
        } else if netease_id.is_none() {
            // 对齐 Android SearchManager：对无明确平台歌词 ID 的歌曲，先尝试 QQ 候选匹配。
            // 这能改善 Bilibili / YouTube / 本地曲目按歌名补全歌词时的命中率。
            let qq = QqMusicClient::new(&self.http);
            match self
                .search_qq_song_mid(&qq, track_title, track_artist)
                .await
            {
                Some(song_mid) => match self.parse_qq_lyrics(&qq, &song_mid).await {
                    Ok(Some(lines)) => return Ok(lines),
                    Ok(None) => log::info!(target: "lyrics", "matched QQ lyrics empty for song_mid={}", song_mid),
                    Err(e) => log::warn!(
                        target: "lyrics",
                        "matched QQ get_lyrics failed for song_mid={}: {}",
                        song_mid, e
                    ),
                },
                None => log::info!(
                    target: "lyrics",
                    "QQ candidate not found for {} / {}",
                    track_title, track_artist
                ),
            }
        }

        let client = NeteaseClient::new(&self.http);

        // 确定网易云歌曲 ID：直接提供或通过搜索获取
        let resolved_id = if let Some(id) = netease_id {
            log::info!(target: "lyrics", "using provided netease_id={}", id);
            Some(id)
        } else {
            // 用 title + artist 搜索网易云，取最匹配的结果
            let id = self
                .search_netease_id(&client, track_title, track_artist)
                .await;
            log::info!(target: "lyrics", "search_netease_id result: {:?}", id);
            id
        };

        // 网易云 API 取歌词（对齐 Android：YRC 优先，LRC 回退）
        if let Some(id) = resolved_id {
            match client.get_lyrics(id).await {
                Ok(lyrics_data) => {
                    log::info!(
                        target: "lyrics",
                        "netease lyrics for id={}: lrc={}, tlyric={}, yrc={}",
                        id,
                        lyrics_data.lrc.as_ref().map_or(0, |s| s.len()),
                        lyrics_data.tlyric.as_ref().map_or(0, |s| s.len()),
                        lyrics_data.yrc.as_ref().map_or(0, |s| s.len()),
                    );

                    // 翻译歌词：优先 ytlrc（YRC 翻译），回退 tlyric
                    let translation = lyrics_data
                        .ytlrc
                        .as_deref()
                        .or(lyrics_data.tlyric.as_deref())
                        .filter(|s| !s.is_empty());

                    // 优先 YRC（逐字歌词），对齐 Android extractPreferredNeteaseLyricContent
                    if let Some(ref yrc_str) = lyrics_data.yrc {
                        if !yrc_str.trim().is_empty() {
                            let mut lines = parser::parse_yrc(yrc_str);
                            if !lines.is_empty() {
                                if let Some(tl) = translation {
                                    parser::merge_translation(&mut lines, tl);
                                }
                                log::info!(
                                    target: "lyrics",
                                    "using netease YRC: {} lines, {} with words",
                                    lines.len(),
                                    lines.iter().filter(|l| !l.words.is_empty()).count()
                                );
                                return Ok(lines);
                            }
                        }
                    }

                    // 回退 LRC
                    if let Some(ref lrc_str) = lyrics_data.lrc {
                        let mut lines = parser::parse_auto(lrc_str);
                        if !lines.is_empty() {
                            if let Some(tl) = translation {
                                parser::merge_translation(&mut lines, tl);
                            }
                            log::info!(target: "lyrics", "using netease LRC: {} lines", lines.len());
                            return Ok(lines);
                        }
                    }
                }
                Err(e) => {
                    log::warn!(target: "lyrics", "netease get_lyrics failed for id={}: {}", id, e);
                }
            }
        }

        // LRCLIB fallback — 精确匹配
        let lrclib = LrcLibClient::new(&self.http);
        if let Ok(Some(result)) = lrclib
            .get_lyrics(track_title, track_artist, duration_secs)
            .await
        {
            if let Some(synced) = result.synced_lyrics {
                let lines = parser::parse_lrc(&synced);
                if !lines.is_empty() {
                    log::info!(target: "lyrics", "using LRCLIB exact: {} lines", lines.len());
                    return Ok(lines);
                }
            }
        }

        // LRCLIB fallback — 模糊搜索
        let query = format!("{} {}", track_title, track_artist);
        if let Ok(results) = lrclib.search(&query).await {
            for r in results {
                if let Some(synced) = r.synced_lyrics {
                    let lines = parser::parse_lrc(&synced);
                    if !lines.is_empty() {
                        log::info!(target: "lyrics", "using LRCLIB search: {} lines", lines.len());
                        return Ok(lines);
                    }
                }
            }
        }

        log::info!(
            target: "lyrics",
            "no lyrics found for: {} - {}",
            track_title, track_artist
        );
        Ok(Vec::new())
    }

    async fn parse_qq_lyrics(
        &self,
        client: &QqMusicClient,
        song_mid: &str,
    ) -> AppResult<Option<Vec<LyricLine>>> {
        let (lrc, translated) = client.get_lyrics(song_mid).await?;
        let Some(lrc) = lrc else {
            return Ok(None);
        };
        let mut lines = parser::parse_lrc(&lrc);
        if lines.is_empty() {
            return Ok(None);
        }
        if let Some(tl) = translated.as_deref() {
            parser::merge_translation(&mut lines, tl);
        }
        log::info!(target: "lyrics", "using QQ LRC: {} lines", lines.len());
        Ok(Some(lines))
    }

    async fn search_qq_song_mid(
        &self,
        client: &QqMusicClient,
        title: &str,
        artist: &str,
    ) -> Option<String> {
        const MINIMUM_MATCH_SCORE: i32 = 60;

        let results = client.search(title, 1, 10).await.ok()?;
        let target_title = normalize_match_text(title);
        let target_artist = normalize_match_text(artist);
        let target_artists = normalize_artists(artist);

        let best = results
            .into_iter()
            .map(|candidate| {
                let score = score_qq_candidate(
                    &candidate.song_name,
                    &candidate.artists.join(" / "),
                    &target_title,
                    &target_artist,
                    &target_artists,
                );
                (candidate, score)
            })
            .max_by_key(|(_, score)| *score)?;

        if best.1 < MINIMUM_MATCH_SCORE {
            log::info!(
                target: "lyrics",
                "no confident QQ match for {} / {}, best_score={}",
                title, artist, best.1
            );
            return None;
        }

        log::info!(
            target: "lyrics",
            "matched QQ song_mid={}, score={}, name={}",
            best.0.song_mid, best.1, best.0.song_name
        );
        Some(best.0.song_mid)
    }

    /// 通过搜索网易云获取匹配歌曲 ID
    async fn search_netease_id(
        &self,
        client: &NeteaseClient,
        title: &str,
        artist: &str,
    ) -> Option<u64> {
        let query = format!("{} {}", title, artist);
        let results = client.search(&query, 5, 0).await.ok()?;
        if results.is_empty() {
            return None;
        }

        // 优先精确匹配标题
        let title_lower = title.to_lowercase();
        for r in &results {
            if r.name.to_lowercase() == title_lower {
                return Some(r.id);
            }
        }
        // 没有精确匹配，取第一个结果
        Some(results[0].id)
    }
}

fn score_qq_candidate(
    candidate_title: &str,
    candidate_artist: &str,
    target_title: &str,
    target_artist: &str,
    target_artists: &std::collections::HashSet<String>,
) -> i32 {
    let candidate_title = normalize_match_text(candidate_title);
    let candidate_artist_normalized = normalize_match_text(candidate_artist);
    let candidate_artists = normalize_artists(candidate_artist);

    let mut score = if candidate_title == target_title {
        100
    } else if !target_title.is_empty()
        && !candidate_title.is_empty()
        && (candidate_title.contains(target_title) || target_title.contains(&candidate_title))
    {
        60
    } else {
        0
    };

    if !target_artist.is_empty() || !target_artists.is_empty() {
        score += if candidate_artist_normalized == target_artist {
            40
        } else if !candidate_artists.is_disjoint(target_artists) {
            25
        } else if candidate_artist_normalized.contains(target_artist)
            || target_artist.contains(&candidate_artist_normalized)
        {
            15
        } else {
            0
        };
    }

    score
}

fn normalize_match_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn normalize_artists(value: &str) -> std::collections::HashSet<String> {
    let normalized = value
        .replace(" feat. ", "/")
        .replace(" feat ", "/")
        .replace(" ft. ", "/")
        .replace(" ft ", "/")
        .replace(" x ", "/")
        .replace(" X ", "/");
    normalized
        .split(|c: char| matches!(c, '/' | ',' | '、' | '，' | '&'))
        .map(normalize_match_text)
        .filter(|s| !s.is_empty())
        .collect()
}

fn load_local_sidecar_lyrics(audio_path: &str) -> Option<Vec<LyricLine>> {
    let path = Path::new(audio_path);
    let lyric_path = find_nearby_lyric(path)?;
    let content = std::fs::read_to_string(&lyric_path).ok()?;
    let mut lines = parse_sidecar_lyrics_text(&content);
    if lines.is_empty() {
        return None;
    }

    if let Some(translation_path) = find_nearby_translation(path) {
        if let Ok(translation) = std::fs::read_to_string(&translation_path) {
            parser::merge_translation(&mut lines, &translation);
        }
    }

    Some(lines)
}

fn parse_sidecar_lyrics_text(content: &str) -> Vec<LyricLine> {
    let parsed = parser::parse_auto(content);
    if !parsed.is_empty() {
        return parsed;
    }

    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, text)| LyricLine {
            start_ms: index as u64 * 3000,
            duration_ms: 3000,
            text: text.to_string(),
            translation: None,
            words: Vec::new(),
        })
        .collect()
}

fn find_nearby_lyric(audio_path: &Path) -> Option<PathBuf> {
    let parent = audio_path.parent()?;
    let stem = audio_path.file_stem()?.to_str()?;

    for ext in ["lrc", "txt"] {
        let sibling = parent.join(format!("{}.{}", stem, ext));
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let lyrics_dir = parent.join("Lyrics");
    for ext in ["lrc", "txt"] {
        let nested = lyrics_dir.join(format!("{}.{}", stem, ext));
        if nested.is_file() {
            return Some(nested);
        }
    }

    None
}

fn find_nearby_translation(audio_path: &Path) -> Option<PathBuf> {
    let parent = audio_path.parent()?;
    let stem = audio_path.file_stem()?.to_str()?;

    for name in [
        format!("{}.tlrc", stem),
        format!("{}.translated.lrc", stem),
        format!("{}.translation.lrc", stem),
    ] {
        let sibling = parent.join(&name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let lyrics_dir = parent.join("Lyrics");
    for name in [
        format!("{}.tlrc", stem),
        format!("{}.translated.lrc", stem),
        format!("{}.translation.lrc", stem),
    ] {
        let nested = lyrics_dir.join(&name);
        if nested.is_file() {
            return Some(nested);
        }
    }

    None
}
