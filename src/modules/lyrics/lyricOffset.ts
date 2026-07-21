// 歌词偏移量模型：对齐 Android LyricDefaultOffset.kt, 并修正桌面端分桶
//
// 两层语义:
//   - 系统全局默认偏移(per-bucket): 设置里的基线
//   - 逐曲用户偏移(delta): 默认 0, 手动微调后才非 0
// 有效偏移 = 默认偏移(bucket) + 逐曲 delta
//
// 分桶规则 (按播放来源, 不是歌词匹配来源):
//   - netease → cloud 默认 (Android DEFAULT_CLOUD=1000)
//   - qq → qq 默认 (Android DEFAULT_QQ=500)
//   - youtube / bilibili / local / 其它 → none (默认 0)
// 之前 youtube 等也走 cloud, 会错误套用"网易云 +1000ms", 造成明显不同步.

// 与 Android DEFAULT_CLOUD_MUSIC_LYRIC_OFFSET_MS / DEFAULT_QQ_MUSIC_LYRIC_OFFSET_MS 对齐
export const DEFAULT_CLOUD_LYRIC_OFFSET_MS = 1000
export const DEFAULT_QQ_LYRIC_OFFSET_MS = 500

// 逐曲 delta 的安全存储边界, 防止脏数据写入本地映射
export const MIN_LYRIC_OFFSET_MS = -30000
export const MAX_LYRIC_OFFSET_MS = 30000

// none: 非网易/QQ 播放源, 系统默认恒为 0
export type LyricOffsetBucket = 'cloud' | 'qq' | 'none'

export function offsetBucketForSource(source?: string | null): LyricOffsetBucket {
  if (source === 'qq') return 'qq'
  if (source === 'netease') return 'cloud'
  return 'none'
}

// 取该来源的系统全局默认偏移; none 桶固定 0, 不受设置里的网易/QQ 滑条影响
export function resolveLyricDefaultOffsetMs(
  bucket: LyricOffsetBucket,
  cloudDefaultMs: number,
  qqDefaultMs: number,
): number {
  if (bucket === 'qq') return qqDefaultMs
  if (bucket === 'cloud') return cloudDefaultMs
  return 0
}

// 改动系统默认偏移时, 对已手动调过的歌曲重算 delta 以保持"绝对时序"不变
// 绝对时序 = 旧默认 + 旧delta = 新默认 + 新delta => 新delta = 旧delta + 旧默认 - 新默认
export function rebaseLyricUserOffsetMs(
  userOffsetMs: number,
  previousDefaultOffsetMs: number,
  newDefaultOffsetMs: number,
): number {
  return userOffsetMs + previousDefaultOffsetMs - newDefaultOffsetMs
}

// 仅对"来源匹配且确实调过(delta != 0)"的歌曲 rebase; 未调过的跟随新默认即可
export function shouldRebaseLyricOffset(
  bucket: LyricOffsetBucket,
  targetBucket: LyricOffsetBucket,
  userOffsetMs: number,
): boolean {
  if (userOffsetMs === 0) return false
  if (bucket === 'none' || targetBucket === 'none') return false
  return bucket === targetBucket
}

// 写入本地前的防御性归一: 非有限值归零, 取整并夹到安全边界
export function clampLyricOffsetMs(value: number): number {
  if (!Number.isFinite(value)) return 0
  const rounded = Math.round(value)
  return Math.min(MAX_LYRIC_OFFSET_MS, Math.max(MIN_LYRIC_OFFSET_MS, rounded))
}

// 逐曲偏移的稳定键: 优先曲目 id(如 netease:123, 同步回来的曲目也会重建成同一 id), 兜底 playlistKey
export function lyricUserOffsetStorageKey(
  track: { id?: string | null; playlistKey?: string | null } | null | undefined,
): string {
  if (!track) return ''
  const id = (track.id ?? '').trim()
  if (id) return id
  return (track.playlistKey ?? '').trim()
}

// 读取同步载荷里 Android 写下的逐曲偏移(delta); 缺失或非法则 0
// 兼容 camelCase / snake_case 两种字段名
export function readSyncedUserOffsetMs(
  track: { syncPayload?: Record<string, unknown> | null } | null | undefined,
): number {
  const payload = track?.syncPayload
  if (!payload || typeof payload !== 'object') return 0
  const raw = payload.userLyricOffsetMs ?? payload.user_lyric_offset_ms
  const value = typeof raw === 'number' ? raw : Number(raw)
  return Number.isFinite(value) ? clampLyricOffsetMs(value) : 0
}

/** 把逐曲偏移写回 syncPayload (对齐 Android SongItem.userLyricOffsetMs) */
export function withUpdatedUserOffsetPayload(
  payload: Record<string, unknown> | undefined | null,
  userOffsetMs: number,
): Record<string, unknown> {
  const base: Record<string, unknown> = { ...(payload || {}) }
  const delta = clampLyricOffsetMs(userOffsetMs)
  base.userLyricOffsetMs = delta
  delete base.user_lyric_offset_ms
  // 标记 CURRENT, 避免被 legacy fill-missing 覆盖
  const version = Number(base.syncMetadataVersion ?? base.sync_metadata_version ?? 0)
  if (!Number.isFinite(version) || version < 1) {
    base.syncMetadataVersion = 1
    delete base.sync_metadata_version
  }
  return base
}
