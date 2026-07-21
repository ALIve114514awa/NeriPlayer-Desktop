/**
 * 把当前曲目的 syncPayload / 展示字段回写到本地歌单, 供下次同步上传
 * 对齐 Android: 编辑歌词 / 偏移写入 SongItem 后触发同步
 */
import { invoke } from '@tauri-apps/api/core'
import type { TrackInfo } from '@/stores/player'
import { createLogger } from '@/utils/logger'

const log = createLogger('sync-track-payload')

function inferTrackSource(trackId: string, source?: string | null): string {
  if (source) return source
  if (trackId.startsWith('netease:')) return 'netease'
  if (trackId.startsWith('qq:')) return 'qq'
  if (trackId.startsWith('bilibili:')) return 'bilibili'
  if (trackId.startsWith('youtube:')) return 'youtube'
  return 'local'
}

export function toBackendTrack(track: TrackInfo) {
  return {
    id: track.id,
    title: track.title,
    artist: track.artist,
    album: track.album || '',
    duration_ms: track.durationMs || 0,
    cover_url: track.coverUrl || null,
    url: track.audioUrl || '',
    source: inferTrackSource(track.id, track.source),
    added_at: Math.max(0, Math.round(track.addedAt || 0)),
    sync_payload: track.syncPayload ?? null,
    playlist_key: track.playlistKey ?? null,
  }
}

/**
 * 将 track 写回所有包含该曲的本地歌单 (playlistId 可选限定)
 * 返回更新条数; 0 表示歌单中没有该曲 (仍可只更新内存中的 currentTrack)
 */
export async function persistTrackSyncPayload(
  track: TrackInfo | null | undefined,
  playlistId?: number | null,
): Promise<number> {
  if (!track?.id) return 0
  try {
    const updated = await invoke<number>('update_playlist_track', {
      playlistId: playlistId ?? null,
      track: toBackendTrack(track),
    })
    return Number(updated) || 0
  } catch (e) {
    log.warn('persistTrackSyncPayload failed:', e)
    return 0
  }
}
