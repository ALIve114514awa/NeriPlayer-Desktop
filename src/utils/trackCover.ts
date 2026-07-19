export interface TrackCoverSource {
  coverUrl?: unknown
  cover_url?: unknown
  syncPayload?: Record<string, unknown> | null
}

/** 返回播放条与详情页都应使用的第一条可显示封面地址 */
export function getTrackCoverUrl(track: TrackCoverSource | null | undefined): string {
  const payload = track?.syncPayload
  const candidates = [
    track?.coverUrl,
    track?.cover_url,
    payload?.customCoverUrl,
    payload?.custom_cover_url,
    payload?.coverUrl,
    payload?.cover_url,
    payload?.originalCoverUrl,
    payload?.original_cover_url,
  ]
  const value = candidates.find(candidate => typeof candidate === 'string' && candidate.trim())
  return typeof value === 'string' ? value.trim() : ''
}
