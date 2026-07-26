// YouTube Music 歌单 browse / continuation
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

use super::client::YouTubeClient;
use crate::auth::state::YouTubeAuth;

/// 提取 continuation token（多种布局）
pub fn extract_continuation_token(root: &Value) -> Option<String> {
    // 直接 continuationContents
    if let Some(token) = root
        .pointer("/continuationContents/musicPlaylistShelfContinuation/continuations/0/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
    {
        return Some(token.to_string());
    }
    if let Some(token) = root
        .pointer("/continuationContents/musicShelfContinuation/continuations/0/nextContinuationData/continuation")
        .and_then(|v| v.as_str())
    {
        return Some(token.to_string());
    }

    // section 内 shelf continuations
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 8000 {
            break;
        }
        if let Some(obj) = node.as_object() {
            if let Some(token) = obj
                .get("nextContinuationData")
                .and_then(|v| v.get("continuation"))
                .and_then(|v| v.as_str())
            {
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
            for value in obj.values() {
                stack.push(value);
            }
        } else if let Some(arr) = node.as_array() {
            for value in arr {
                stack.push(value);
            }
        }
    }
    None
}

/// 拉取歌单详情，并尽量展开 continuation 分页
pub async fn fetch_playlist_detail_pages(
    client: &YouTubeClient,
    browse_id: &str,
    auth: &YouTubeAuth,
    max_pages: usize,
) -> AppResult<(Value, Option<YouTubeAuth>)> {
    let (mut root, mut auth_opt) = client
        .get_playlist_detail_with_session(browse_id, auth)
        .await?;

    let mut page = 1usize;
    while page < max_pages {
        let Some(token) = extract_continuation_token(&root) else {
            break;
        };
        let (next, next_auth) = client
            .continue_playlist_with_session(&token, auth_opt.as_ref().unwrap_or(auth))
            .await?;
        if let Some(updated) = next_auth {
            auth_opt = Some(updated);
        }
        merge_continuation_items(&mut root, &next);
        page += 1;
    }

    Ok((root, auth_opt))
}

fn merge_continuation_items(root: &mut Value, next: &Value) {
    let next_items = next
        .pointer("/continuationContents/musicPlaylistShelfContinuation/contents")
        .or_else(|| next.pointer("/continuationContents/musicShelfContinuation/contents"))
        .or_else(|| {
            next.pointer("/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems")
        })
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if next_items.is_empty() {
        return;
    }

    // 尝试追加到现有 shelf
    let section_paths = [
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/contents",
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
    ];

    for path in section_paths {
        if let Some(contents) = root.pointer_mut(path).and_then(|v| v.as_array_mut()) {
            for section in contents.iter_mut() {
                for shelf_key in ["musicPlaylistShelfRenderer", "musicShelfRenderer"] {
                    if let Some(items) = section
                        .pointer_mut(&format!("/{shelf_key}/contents"))
                        .and_then(|v| v.as_array_mut())
                    {
                        items.extend(next_items.clone());
                        // 更新 continuation token
                        if let Some(conts) = next
                            .pointer("/continuationContents/musicPlaylistShelfContinuation/continuations")
                            .or_else(|| {
                                next.pointer(
                                    "/continuationContents/musicShelfContinuation/continuations",
                                )
                            })
                        {
                            if let Some(slot) =
                                section.pointer_mut(&format!("/{shelf_key}/continuations"))
                            {
                                *slot = conts.clone();
                            }
                        } else if let Some(shelf) = section
                            .as_object_mut()
                            .and_then(|obj| obj.get_mut(shelf_key))
                            .and_then(|v| v.as_object_mut())
                        {
                            shelf.remove("continuations");
                        }
                        return;
                    }
                }
            }
        }
    }

    // 兜底: 把 continuation 结果挂到根上, 供前端深搜
    if let Some(obj) = root.as_object_mut() {
        match obj.get_mut("_neriMergedContinuationItems") {
            Some(Value::Array(existing)) => {
                existing.extend(next_items);
            }
            _ => {
                obj.insert(
                    "_neriMergedContinuationItems".into(),
                    json!(next_items),
                );
            }
        }
    }
}

pub fn ensure_browse_id(browse_id: &str) -> AppResult<String> {
    let id = browse_id.trim();
    if id.is_empty() {
        return Err(AppError::Api("empty youtube playlist id".into()));
    }
    Ok(normalize_playlist_browse_id(id))
}

/// 歌单 browseId 规范化: 裸 playlistId 需加 VL 前缀才能 browse 到歌单内容
/// 已是完整 browseId 的 VL/MP/FE 前缀保持不变(对齐 Android normalizePlaylistBrowseId)
pub fn normalize_playlist_browse_id(browse_id: &str) -> String {
    let trimmed = browse_id.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("VL")
        || trimmed.starts_with("MP")
        || trimmed.starts_with("FE")
    {
        return trimmed.to_string();
    }
    format!("VL{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::{extract_continuation_token, normalize_playlist_browse_id};
    use serde_json::json;

    #[test]
    fn extract_token_from_playlist_shelf() {
        let root = json!({
            "continuationContents": {
                "musicPlaylistShelfContinuation": {
                    "continuations": [{
                        "nextContinuationData": { "continuation": "TOKEN123" }
                    }]
                }
            }
        });
        assert_eq!(
            extract_continuation_token(&root).as_deref(),
            Some("TOKEN123")
        );
    }

    #[test]
    fn normalize_prefixes_bare_playlist_id() {
        assert_eq!(normalize_playlist_browse_id("PLabc"), "VLPLabc");
        assert_eq!(normalize_playlist_browse_id("OLAK5uy_x"), "VLOLAK5uy_x");
        assert_eq!(normalize_playlist_browse_id("LM"), "VLLM");
        assert_eq!(normalize_playlist_browse_id("VLPLabc"), "VLPLabc");
        assert_eq!(normalize_playlist_browse_id("MPREb_x"), "MPREb_x");
        assert_eq!(normalize_playlist_browse_id("FEmusic_home"), "FEmusic_home");
    }
}
