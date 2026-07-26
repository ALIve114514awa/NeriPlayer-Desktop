// 本地音乐文件扫描
use crate::error::AppResult;
use crate::state::TrackInfo;
use crate::state::TrackSource;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "wav", "m4a", "aac", "opus", "wma", "webm", "eac3",
];
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "albumart", "album"];
// 大小写无关匹配的封面子目录名
const COVER_DIR_NAMES: &[&str] = &["covers", "cover", "artwork", "scans"];

#[derive(Debug, Clone, Default)]
struct ParsedFileNameMetadata {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    source: Option<String>,
}

/// 扫描目录下的所有音频文件，读取元数据
pub fn scan_directory(dir: &str, name_template: Option<&str>) -> AppResult<Vec<TrackInfo>> {
    let mut tracks = Vec::new();
    // 扫描会话级封面索引缓存：同目录的封面查找只列举一次目录
    let mut cover_cache = CoverLookupCache::default();

    for entry in WalkDir::new(dir).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() { continue; }

        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if !AUDIO_EXTENSIONS.contains(&ext.as_str()) { continue; }

        match read_track_info(path, name_template, &mut cover_cache) {
            Ok(track) => tracks.push(track),
            Err(e) => log::warn!(target: "scanner", "Skip {}: {}", path.display(), e),
        }
    }

    Ok(tracks)
}

fn read_track_info(
    path: &Path,
    name_template: Option<&str>,
    cover_cache: &mut CoverLookupCache,
) -> AppResult<TrackInfo> {
    use lofty::prelude::*;
    use lofty::probe::Probe;

    let tagged = Probe::open(path)
        .map_err(|e| crate::error::AppError::Metadata(e.to_string()))?
        .read()
        .map_err(|e| crate::error::AppError::Metadata(e.to_string()))?;
    let properties = tagged.properties();
    let duration_ms = properties.duration().as_millis() as u64;

    // 尝试读取标签
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown")
        .to_string();
    let parsed = parse_file_name_metadata(path, name_template);

    let raw_title = tag.and_then(|t| t.title().map(|s| s.to_string()))
        .unwrap_or_else(|| {
            file_stem.clone()
        });
    let raw_artist = tag.and_then(|t| t.artist().map(|s| s.to_string()))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let raw_album = tag.and_then(|t| t.album().map(|s| s.to_string()))
        .unwrap_or_else(|| "Unknown Album".to_string());

    let title = if should_use_parsed_title(&raw_title, &file_stem) {
        parsed
            .as_ref()
            .and_then(|p| p.title.clone())
            .unwrap_or(raw_title)
    } else {
        raw_title
    };
    let artist = if is_unknown_artist(&raw_artist) || parsed_source_matches_artist(&raw_artist, parsed.as_ref()) {
        parsed
            .as_ref()
            .and_then(|p| p.artist.clone())
            .unwrap_or(raw_artist)
    } else {
        raw_artist
    };
    let album = if is_unknown_album(&raw_album) {
        parsed
            .as_ref()
            .and_then(|p| p.album.clone())
            .unwrap_or(raw_album)
    } else {
        raw_album
    };
    let cover_url =
        find_nearby_cover(path, cover_cache).map(|p| p.to_string_lossy().to_string());

    Ok(TrackInfo {
        id: format!("local:{}", path.display()),
        title,
        artist,
        album,
        duration_ms,
        source: TrackSource::Local,
        url: path.to_string_lossy().to_string(),
        cover_url,
        added_at: 0,
        sync_payload: None,
        playlist_key: None,
    })
}

/// 单次扫描会话内按目录缓存的封面索引。
///
/// 旧实现每个音频文件最多做 3 轮 `read_dir` 全扫，平铺大目录下整体
/// 近似 O(N²)；缓存后每个目录只列举一次，整体 O(N)
#[derive(Default)]
struct CoverLookupCache {
    dirs: HashMap<PathBuf, DirCoverIndex>,
}

struct DirCoverIndex {
    /// (小写去扩展名文件名, 完整路径)，保持 read_dir 原始顺序，
    /// 维持旧实现「首个命中」的选取行为
    images: Vec<(String, PathBuf)>,
    /// 首个大小写无关命中的封面子目录（Covers/cover/artwork/scans）
    cover_subdir: Option<PathBuf>,
}

impl CoverLookupCache {
    fn index_of(&mut self, dir: &Path) -> &DirCoverIndex {
        self.dirs
            .entry(dir.to_path_buf())
            .or_insert_with(|| build_dir_cover_index(dir))
    }

    /// 在目录中查找文件名（小写、去扩展名）满足断言且扩展名为图片的文件
    fn find_image<F>(&mut self, dir: &Path, matcher: F) -> Option<PathBuf>
    where
        F: Fn(&str) -> bool,
    {
        self.index_of(dir)
            .images
            .iter()
            .find(|(stem, _)| matcher(stem))
            .map(|(_, path)| path.clone())
    }

    /// 大小写无关查找封面子目录（Covers/cover/artwork...）
    fn cover_subdir(&mut self, dir: &Path) -> Option<PathBuf> {
        self.index_of(dir).cover_subdir.clone()
    }
}

fn build_dir_cover_index(dir: &Path) -> DirCoverIndex {
    let mut images = Vec::new();
    let mut cover_subdir = None;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return DirCoverIndex {
            images,
            cover_subdir,
        };
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if cover_subdir.is_none() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
                if COVER_DIR_NAMES.contains(&name.as_str()) {
                    cover_subdir = Some(path);
                }
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        images.push((stem, path));
    }
    DirCoverIndex {
        images,
        cover_subdir,
    }
}

fn find_nearby_cover(audio_path: &Path, cache: &mut CoverLookupCache) -> Option<PathBuf> {
    let dir = audio_path.parent()?;
    let stem = audio_path.file_stem()?.to_str()?.to_lowercase();

    // 同目录下与音频同名的图片（大小写无关）
    if let Some(cover) = cache.find_image(dir, |name| name == stem) {
        return Some(cover);
    }

    // 大小写无关的封面子目录下与音频同名的图片
    if let Some(sub) = cache.cover_subdir(dir) {
        if let Some(cover) = cache.find_image(&sub, |name| name == stem) {
            return Some(cover);
        }
        // 子目录内任意常见封面名
        if let Some(cover) = cache.find_image(&sub, |name| COVER_NAMES.contains(&name)) {
            return Some(cover);
        }
    }

    // 同目录下常见封面文件名（cover/folder/front/albumart...，大小写无关）
    if let Some(cover) = cache.find_image(dir, |name| COVER_NAMES.contains(&name)) {
        return Some(cover);
    }

    None
}

fn parse_file_name_metadata(
    path: &Path,
    active_template: Option<&str>,
) -> Option<ParsedFileNameMetadata> {
    let base_name = path.file_stem()?.to_str()?.trim();
    if base_name.is_empty() {
        return None;
    }

    candidate_templates(active_template)
        .into_iter()
        .filter_map(|template| parse_base_name_with_template(base_name, &template))
        .find(|parsed| {
            parsed.title.as_deref().is_some_and(|s| !s.is_empty())
                || parsed.artist.as_deref().is_some_and(|s| !s.is_empty())
                || parsed.album.as_deref().is_some_and(|s| !s.is_empty())
        })
}

fn candidate_templates(active_template: Option<&str>) -> Vec<String> {
    let mut templates = Vec::new();
    if let Some(tpl) = active_template.map(str::trim).filter(|s| !s.is_empty()) {
        templates.push(tpl.to_string());
    }
    for tpl in [
        "{artist} - {title}",
        "{source} - {artist} - {title}",
        "%source% - %artist% - %title%",
        "%artist% - %title%",
    ] {
        if !templates.iter().any(|existing| existing == tpl) {
            templates.push(tpl.to_string());
        }
    }
    templates
}

fn parse_base_name_with_template(
    base_name: &str,
    template: &str,
) -> Option<ParsedFileNameMetadata> {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Field {
        Title,
        Artist,
        Album,
        Source,
        Id,
        AudioId,
        SubAudioId,
    }

    let placeholders = [
        ("{title}", Field::Title),
        ("{artist}", Field::Artist),
        ("{album}", Field::Album),
        ("{source}", Field::Source),
        ("%title%", Field::Title),
        ("%artist%", Field::Artist),
        ("%album%", Field::Album),
        ("%source%", Field::Source),
        ("%id%", Field::Id),
        ("%audioId%", Field::AudioId),
        ("%subAudioId%", Field::SubAudioId),
    ];

    let mut fields = Vec::new();
    let mut pattern = String::from("^");
    let mut cursor = 0;
    let mut matched_any = false;

    while cursor < template.len() {
        let next = placeholders
            .iter()
            .filter_map(|(token, field)| {
                template[cursor..]
                    .find(token)
                    .map(|idx| (cursor + idx, *token, *field))
            })
            .min_by_key(|(idx, _, _)| *idx);

        let Some((idx, token, field)) = next else {
            pattern.push_str(&regex::escape(&template[cursor..]));
            break;
        };

        pattern.push_str(&regex::escape(&template[cursor..idx]));
        if fields.contains(&field) {
            return None;
        }
        fields.push(field);
        pattern.push_str("(.*?)");
        cursor = idx + token.len();
        matched_any = true;
    }

    if !matched_any {
        return None;
    }

    pattern.push('$');
    let regex = Regex::new(&pattern).ok()?;
    let captures = regex.captures(base_name)?;

    let mut parsed = ParsedFileNameMetadata::default();
    for (index, field) in fields.iter().enumerate() {
        let value = captures
            .get(index + 1)
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty());
        match field {
            Field::Title => parsed.title = value,
            Field::Artist => parsed.artist = value,
            Field::Album => parsed.album = value,
            Field::Source => parsed.source = value,
            Field::Id | Field::AudioId | Field::SubAudioId => {}
        }
    }
    Some(parsed)
}

fn should_use_parsed_title(current_title: &str, file_stem: &str) -> bool {
    let normalized = normalize_metadata_value(current_title);
    normalized.is_empty()
        || normalized == "unknown"
        || normalized == normalize_metadata_value(file_stem)
}

fn is_unknown_artist(value: &str) -> bool {
    matches!(
        normalize_metadata_value(value).as_str(),
        "" | "unknown" | "unknown artist" | "<unknown>"
    )
}

fn is_unknown_album(value: &str) -> bool {
    matches!(
        normalize_metadata_value(value).as_str(),
        "" | "unknown" | "unknown album" | "<unknown>"
    )
}

fn parsed_source_matches_artist(
    artist: &str,
    parsed: Option<&ParsedFileNameMetadata>,
) -> bool {
    parsed
        .and_then(|p| p.source.as_deref())
        .map(|source| normalize_metadata_value(source) == normalize_metadata_value(artist))
        .unwrap_or(false)
}

fn normalize_metadata_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{find_nearby_cover, CoverLookupCache};
    use std::path::PathBuf;

    fn temp_scan_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neri-scanner-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn cover_lookup_prefers_same_stem_then_common_names() {
        let dir = temp_scan_dir("flat");
        std::fs::write(dir.join("song.jpg"), b"img").expect("write image");
        std::fs::write(dir.join("cover.png"), b"img").expect("write image");
        let audio = dir.join("song.mp3");
        std::fs::write(&audio, b"audio").expect("write audio");
        let other = dir.join("other.flac");
        std::fs::write(&other, b"audio").expect("write audio");

        let mut cache = CoverLookupCache::default();
        assert_eq!(
            find_nearby_cover(&audio, &mut cache),
            Some(dir.join("song.jpg"))
        );
        // 同名不命中时回退到常见封面名
        assert_eq!(
            find_nearby_cover(&other, &mut cache),
            Some(dir.join("cover.png"))
        );
        // 目录只被列举并缓存一次
        assert_eq!(cache.dirs.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cover_lookup_finds_image_in_cover_subdir() {
        let dir = temp_scan_dir("subdir");
        let sub = dir.join("Covers");
        std::fs::create_dir_all(&sub).expect("create cover subdir");
        std::fs::write(sub.join("track.webp"), b"img").expect("write image");
        let audio = dir.join("track.mp3");
        std::fs::write(&audio, b"audio").expect("write audio");

        let mut cache = CoverLookupCache::default();
        assert_eq!(
            find_nearby_cover(&audio, &mut cache),
            Some(sub.join("track.webp"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
