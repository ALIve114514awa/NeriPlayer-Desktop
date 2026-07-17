use crate::error::AppResult;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Copy, Default)]
struct FileStats {
    size_bytes: u64,
    file_count: u64,
}

impl std::ops::AddAssign for FileStats {
    fn add_assign(&mut self, rhs: Self) {
        self.size_bytes = self.size_bytes.saturating_add(rhs.size_bytes);
        self.file_count = self.file_count.saturating_add(rhs.file_count);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsageItem {
    pub id: String,
    pub size_bytes: u64,
    pub file_count: u64,
    pub path: Option<String>,
    pub cache_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsageSection {
    pub id: String,
    pub items: Vec<StorageUsageItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsageSummary {
    pub sections: Vec<StorageUsageSection>,
    pub total_size_bytes: u64,
    pub total_file_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StorageCacheClearOptions {
    pub audio_cache: bool,
    pub image_cache: bool,
    pub download_staging: bool,
    pub shared_media: bool,
    pub platform_list: bool,
}

impl Default for StorageCacheClearOptions {
    fn default() -> Self {
        Self {
            audio_cache: true,
            image_cache: true,
            download_staging: false,
            shared_media: false,
            platform_list: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageClearResult {
    pub cleared_bytes: u64,
    pub deleted_files: u64,
    pub failed_count: u64,
}

#[tauri::command]
pub async fn get_storage_usage(
    app: AppHandle,
    download_dir: Option<String>,
) -> AppResult<StorageUsageSummary> {
    let app_data_dir = app.path().app_data_dir().ok();
    let app_cache_dir = app.path().app_cache_dir().ok();
    let default_download_dir = app_data_dir.as_ref().map(|dir| dir.join("downloads"));
    let download_roots = download_roots(default_download_dir.as_deref(), download_dir.as_deref());

    let playback_cache = app_cache_dir.as_ref().map(|dir| dir.join("playback-audio"));
    let image_cache = child_paths(
        app_data_dir.as_deref(),
        &["covers", "thumbnails"],
    );
    let image_cache = append_child_path(
        image_cache,
        app_cache_dir.as_deref(),
        "image_cache",
    );
    let download_staging = child_paths(app_data_dir.as_deref(), &["temp"]);
    let download_staging = append_child_path(
        download_staging,
        app_cache_dir.as_deref(),
        "download_staging",
    );
    let shared_media = child_paths(app_data_dir.as_deref(), &["shared_media"]);
    let shared_media = append_child_path(
        shared_media,
        app_cache_dir.as_deref(),
        "shared_media_exports",
    );
    let platform_list = child_paths(
        app_data_dir.as_deref(),
        &[
            "netease_playlist_cache",
            "bili_favorite_cache",
            "youtube_music_playlist_cache",
        ],
    );
    let mut classified_cache_paths = image_cache.clone();
    classified_cache_paths.extend(download_staging.iter().cloned());
    classified_cache_paths.extend(shared_media.iter().cloned());
    classified_cache_paths.extend(platform_list.iter().cloned());
    if let Some(path) = playback_cache.as_ref() {
        classified_cache_paths.push(path.clone());
    }

    let mut sections = vec![StorageUsageSection {
        id: "cleanable_cache".into(),
        items: vec![
            usage_item("audio_cache", playback_cache.as_deref(), Some("audio")),
            aggregate_item("image_cache", &image_cache, Some("image")),
            aggregate_item(
                "download_staging",
                &download_staging,
                Some("download_staging"),
            ),
            aggregate_item("shared_media", &shared_media, Some("shared_media")),
            aggregate_item("platform_list_cache", &platform_list, Some("platform_list")),
            other_cache_item(app_cache_dir.as_deref(), &classified_cache_paths),
        ],
    }];

    let downloads = scan_download_roots(&download_roots);
    sections.push(StorageUsageSection {
        id: "downloads".into(),
        items: vec![
            stats_item("downloaded_music", downloads.music, None, None),
            stats_item("downloaded_lyrics", downloads.lyrics, None, None),
            aggregate_item("download_index", &download_index_paths(&download_roots), None),
        ],
    });

    let diagnostics = child_paths(
        app_data_dir.as_deref(),
        &["logs", "crashes", "error_logs", "crash-reports"],
    );
    sections.push(StorageUsageSection {
        id: "diagnostics".into(),
        items: vec![
            aggregate_item("logs", &diagnostics[..2.min(diagnostics.len())], None),
            aggregate_item(
                "crash_logs",
                &diagnostics[2.min(diagnostics.len())..],
                None,
            ),
        ],
    });

    let local_covers = app_data_dir.as_ref().map(|dir| dir.join("local-covers"));
    let custom_background = app_data_dir.as_ref().map(|dir| dir.join("background"));
    let playlist_data = dirs_next::data_dir()
        .map(|dir| dir.join("NeriPlayer").join("playlists.json"));
    let known_data = known_data_paths(
        &image_cache,
        &download_staging,
        &shared_media,
        &platform_list,
        &download_roots,
        local_covers.as_deref(),
        custom_background.as_deref(),
        playlist_data.as_deref(),
    );
    sections.push(StorageUsageSection {
        id: "app_data".into(),
        items: vec![
            usage_item("local_covers", local_covers.as_deref(), None),
            usage_item("custom_background", custom_background.as_deref(), None),
            usage_item("playlist_data", playlist_data.as_deref(), None),
            other_app_data_item(app_data_dir.as_deref(), &known_data),
        ],
    });

    let total_size_bytes = sections
        .iter()
        .flat_map(|section| section.items.iter())
        .map(|item| item.size_bytes)
        .sum();
    let total_file_count = sections
        .iter()
        .flat_map(|section| section.items.iter())
        .map(|item| item.file_count)
        .sum();

    Ok(StorageUsageSummary {
        sections,
        total_size_bytes,
        total_file_count,
    })
}

#[tauri::command]
pub async fn clear_storage_cache(
    app: AppHandle,
    options: StorageCacheClearOptions,
) -> AppResult<StorageClearResult> {
    let app_data_dir = app.path().app_data_dir().ok();
    let app_cache_dir = app.path().app_cache_dir().ok();
    let playback_cache = app_cache_dir.as_ref().map(|dir| dir.join("playback-audio"));
    let image_cache = append_child_path(
        child_paths(app_data_dir.as_deref(), &["covers", "thumbnails"]),
        app_cache_dir.as_deref(),
        "image_cache",
    );
    let download_staging = append_child_path(
        child_paths(app_data_dir.as_deref(), &["temp"]),
        app_cache_dir.as_deref(),
        "download_staging",
    );
    let shared_media = append_child_path(
        child_paths(app_data_dir.as_deref(), &["shared_media"]),
        app_cache_dir.as_deref(),
        "shared_media_exports",
    );
    let platform_list = child_paths(
        app_data_dir.as_deref(),
        &[
            "netease_playlist_cache",
            "bili_favorite_cache",
            "youtube_music_playlist_cache",
        ],
    );

    let mut targets = Vec::new();
    if options.audio_cache {
        targets.extend(playback_cache);
    }
    if options.image_cache {
        targets.extend(image_cache);
    }
    if options.download_staging {
        targets.extend(download_staging);
    }
    if options.shared_media {
        targets.extend(shared_media);
    }
    if options.platform_list {
        targets.extend(platform_list);
    }

    let mut result = StorageClearResult {
        cleared_bytes: 0,
        deleted_files: 0,
        failed_count: 0,
    };
    let mut seen = HashSet::new();
    for target in targets {
        let key = canonical_key(&target);
        if !seen.insert(key) {
            continue;
        }
        let stats = stats_for_path(&target, &[]);
        if stats.file_count == 0 && stats.size_bytes == 0 {
            continue;
        }
        match clear_directory_contents(&target) {
            Ok(()) => {
                result.cleared_bytes = result.cleared_bytes.saturating_add(stats.size_bytes);
                result.deleted_files = result.deleted_files.saturating_add(stats.file_count);
            }
            Err(_) => result.failed_count = result.failed_count.saturating_add(1),
        }
    }

    Ok(result)
}

fn usage_item(id: &str, path: Option<&Path>, cache_kind: Option<&str>) -> StorageUsageItem {
    let stats = path.map(|value| stats_for_path(value, &[])).unwrap_or_default();
    stats_item(
        id,
        stats,
        path.map(|value| value.to_string_lossy().to_string()),
        cache_kind,
    )
}

fn aggregate_item(id: &str, paths: &[PathBuf], cache_kind: Option<&str>) -> StorageUsageItem {
    let stats = paths.iter().fold(FileStats::default(), |mut total, path| {
        total += stats_for_path(path, &[]);
        total
    });
    let path = (!paths.is_empty()).then(|| {
        paths
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    });
    stats_item(id, stats, path, cache_kind)
}

fn stats_item(
    id: &str,
    stats: FileStats,
    path: Option<String>,
    cache_kind: Option<&str>,
) -> StorageUsageItem {
    StorageUsageItem {
        id: id.into(),
        size_bytes: stats.size_bytes,
        file_count: stats.file_count,
        path,
        cache_kind: cache_kind.map(str::to_string),
    }
}

fn other_cache_item(app_cache_dir: Option<&Path>, excluded: &[PathBuf]) -> StorageUsageItem {
    let stats = app_cache_dir
        .map(|path| stats_for_path(path, excluded))
        .unwrap_or_default();
    stats_item(
        "other_cache",
        stats,
        app_cache_dir.map(|path| path.to_string_lossy().to_string()),
        None,
    )
}

fn other_app_data_item(app_data_dir: Option<&Path>, excluded: &[PathBuf]) -> StorageUsageItem {
    let stats = app_data_dir
        .map(|path| stats_for_path(path, excluded))
        .unwrap_or_default();
    stats_item(
        "app_data",
        stats,
        app_data_dir.map(|path| path.to_string_lossy().to_string()),
        None,
    )
}

fn child_paths(root: Option<&Path>, names: &[&str]) -> Vec<PathBuf> {
    root.into_iter()
        .flat_map(|path| names.iter().map(move |name| path.join(name)))
        .collect()
}

fn append_child_path(mut paths: Vec<PathBuf>, root: Option<&Path>, name: &str) -> Vec<PathBuf> {
    if let Some(path) = root {
        paths.push(path.join(name));
    }
    paths
}

fn stats_for_path(path: &Path, excluded: &[PathBuf]) -> FileStats {
    if !path.exists() || is_excluded(path, excluded) {
        return FileStats::default();
    }
    if path.is_file() {
        return FileStats {
            size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            file_count: 1,
        };
    }

    let mut stats = FileStats::default();
    let Ok(entries) = fs::read_dir(path) else {
        return stats;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if is_excluded(&child, excluded) {
            continue;
        }
        stats += stats_for_path(&child, excluded);
    }
    stats
}

fn is_excluded(path: &Path, excluded: &[PathBuf]) -> bool {
    let path_key = canonical_key(path);
    excluded.iter().any(|root| {
        let root_key = canonical_key(root);
        path_key == root_key || path_key.starts_with(&format!("{}{}", root_key, std::path::MAIN_SEPARATOR))
    })
}

fn canonical_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn clear_directory_contents(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        if child.is_dir() {
            fs::remove_dir_all(child)?;
        } else {
            fs::remove_file(child)?;
        }
    }
    Ok(())
}

fn download_roots(default_root: Option<&Path>, custom_root: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = default_root {
        roots.push(root.to_path_buf());
    }
    if let Some(custom) = custom_root.map(str::trim).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(custom);
        if !roots.iter().any(|root| canonical_key(root) == canonical_key(&path)) {
            roots.push(path);
        }
    }
    roots
}

fn download_index_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|root| root.join("manifest.json"))
        .filter(|path| path.exists())
        .collect()
}

#[derive(Default)]
struct DownloadStats {
    music: FileStats,
    lyrics: FileStats,
}

fn scan_download_roots(roots: &[PathBuf]) -> DownloadStats {
    let mut total = DownloadStats::default();
    for root in roots {
        scan_download_path(root, &mut total);
    }
    total
}

fn scan_download_path(path: &Path, stats: &mut DownloadStats) {
    if !path.exists() {
        return;
    }
    if path.is_file() {
        let file_stats = FileStats {
            size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            file_count: 1,
        };
        let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("");
        if matches!(extension.to_ascii_lowercase().as_str(), "lrc" | "tlrc") {
            stats.lyrics += file_stats;
        } else if path.file_name().and_then(|value| value.to_str()) != Some("manifest.json") {
            stats.music += file_stats;
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        scan_download_path(&entry.path(), stats);
    }
}

fn known_data_paths(
    image_cache: &[PathBuf],
    download_staging: &[PathBuf],
    shared_media: &[PathBuf],
    platform_list: &[PathBuf],
    download_roots: &[PathBuf],
    local_covers: Option<&Path>,
    custom_background: Option<&Path>,
    playlist_data: Option<&Path>,
) -> Vec<PathBuf> {
    image_cache
        .iter()
        .chain(download_staging)
        .chain(shared_media)
        .chain(platform_list)
        .chain(download_roots)
        .cloned()
        .chain(local_covers.map(Path::to_path_buf))
        .chain(custom_background.map(Path::to_path_buf))
        .chain(playlist_data.map(Path::to_path_buf))
        .collect()
}
