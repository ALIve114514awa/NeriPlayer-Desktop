export interface TrackCoverSource {
  coverUrl?: unknown
  cover_url?: unknown
  syncPayload?: Record<string, unknown> | null
}

/** YouTube Music 缩略图尺寸参数：=w60-h60-l90-rj → =w1200-h1200 */
const YOUTUBE_SIZE_PARAM_RE = /=w\d+(-h\d+)?(-[a-zA-Z0-9-]+)*$/
const YOUTUBE_S_PARAM_RE = /=s\d+(-[a-zA-Z0-9-]+)*$/
const YOUTUBE_HIGH_RES_SIZE = '=w1200-h1200'

/**
 * 将 YouTube Music 缩略图 URL 升级为完整尺寸
 * 对齐 Android upgradeYouTubeThumbnailUrl
 */
export function upgradeYouTubeThumbnailUrl(url: string): string {
  if (typeof url !== 'string') return ''
  const trimmed = url.trim()
  if (!trimmed) return url

  if (YOUTUBE_SIZE_PARAM_RE.test(trimmed)) {
    return trimmed.replace(YOUTUBE_SIZE_PARAM_RE, YOUTUBE_HIGH_RES_SIZE)
  }

  if (YOUTUBE_S_PARAM_RE.test(trimmed) && isGoogleImageHost(trimmed)) {
    return trimmed.replace(YOUTUBE_S_PARAM_RE, YOUTUBE_HIGH_RES_SIZE)
  }

  if (isGoogleImageHost(trimmed) && !trimmed.includes('=')) {
    return `${trimmed}${YOUTUBE_HIGH_RES_SIZE}`
  }

  return trimmed
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
  if (typeof value !== 'string') return ''
  return upgradeYouTubeThumbnailUrl(value.trim())
}

function isGoogleImageHost(url: string): boolean {
  try {
    const host = new URL(url.startsWith('//') ? `https:${url}` : url).hostname.toLowerCase()
    return (
      host === 'lh3.googleusercontent.com'
      || host.endsWith('.googleusercontent.com')
      || host === 'yt3.ggpht.com'
      || host.endsWith('.ggpht.com')
    )
  } catch {
    return (
      url.includes('lh3.googleusercontent.com')
      || url.includes('googleusercontent.com')
      || url.includes('yt3.ggpht.com')
      || url.includes('ggpht.com')
    )
  }
}
