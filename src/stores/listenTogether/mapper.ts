/**
 * TrackInfo ↔ ListenTogetherTrack 双向映射
 * stableKey 生成规则与安卓端一致
 */
import type { TrackInfo } from '@/stores/player'
import { LtChannels, type ListenTogetherTrack } from './protocol'

/**
 * TrackInfo → ListenTogetherTrack
 * 解析 track.id 前缀 → 填充 channelId/audioId/subAudioId
 */
export function trackInfoToLtTrack(
  track: TrackInfo,
  streamUrl?: string,
): ListenTogetherTrack {
  let channelId: string
  let audioId: string
  let subAudioId: string | undefined

  if (track.id.startsWith('netease:')) {
    channelId = LtChannels.NETEASE
    audioId = track.id.replace('netease:', '')
  } else if (track.id.startsWith('bilibili:')) {
    channelId = LtChannels.BILIBILI
    audioId = track.id.replace('bilibili:', '')
    // 从 album 字段提取 cid 作为 subAudioId
    const cidMatch = track.album?.match(/^Bilibili\|(\d+)/)
    if (cidMatch) {
      subAudioId = cidMatch[1]
    }
  } else if (track.id.startsWith('youtube:')) {
    channelId = LtChannels.YOUTUBE_MUSIC
    audioId = track.id.replace('youtube:', '')
  } else {
    channelId = LtChannels.LOCAL
    audioId = track.id
  }

  const stableKey = buildStableKey(channelId, audioId, subAudioId)

  return {
    stableKey,
    channelId,
    audioId,
    subAudioId,
    streamUrl,
    name: track.title,
    artist: track.artist,
    album: track.album || undefined,
    durationMs: track.durationMs,
    coverUrl: track.coverUrl || undefined,
  }
}

/**
 * ListenTogetherTrack → TrackInfo
 * 按 channelId 构造 Desktop 的 id 格式
 */
export function ltTrackToTrackInfo(lt: ListenTogetherTrack): TrackInfo {
  let id: string
  let album = lt.album || ''

  switch (lt.channelId) {
    case LtChannels.NETEASE:
      id = `netease:${lt.audioId}`
      break
    case LtChannels.BILIBILI:
      id = `bilibili:${lt.audioId}`
      // 用 subAudioId 重建 album 中的 cid 信息
      if (lt.subAudioId) {
        album = `Bilibili|${lt.subAudioId}`
      }
      break
    case LtChannels.YOUTUBE_MUSIC:
      id = `youtube:${lt.audioId}`
      break
    default:
      id = lt.audioId
  }

  return {
    id,
    title: lt.name,
    artist: lt.artist,
    album,
    durationMs: lt.durationMs,
    coverUrl: lt.coverUrl || '',
    audioUrl: lt.mediaUri || lt.streamUrl || '',
  }
}

/**
 * stableKey 生成规则（与安卓对齐）
 * netease:123456, bilibili:BVxxx:cid, youtubeMusic:videoId
 */
function buildStableKey(channelId: string, audioId: string, subAudioId?: string): string {
  if (subAudioId) {
    return `${channelId}:${audioId}:${subAudioId}`
  }
  return `${channelId}:${audioId}`
}

/**
 * 将当前播放队列映射为可分享的 ListenTogetherTrack 数组
 */
export function toShareableQueueSnapshot(
  queue: TrackInfo[],
  currentIndex: number,
  shareAudioLinks: boolean = true,
  currentStreamUrl?: string,
): { queue: ListenTogetherTrack[]; resolvedIndex: number } {
  const result: ListenTogetherTrack[] = []
  let resolvedIndex = 0
  const currentTrack = queue[currentIndex]
  const currentStableKey = currentTrack
    ? trackInfoToLtTrack(currentTrack).stableKey
    : null

  for (let i = 0; i < queue.length; i++) {
    const track = queue[i]
    const isCurrentTrack = i === currentIndex
    const streamUrl = isCurrentTrack && shareAudioLinks ? currentStreamUrl : undefined
    const ltTrack = trackInfoToLtTrack(track, streamUrl)
    result.push(ltTrack)
  }

  // 找到 currentStableKey 在结果中的位置
  if (currentStableKey) {
    const idx = result.findIndex(t => t.stableKey === currentStableKey)
    if (idx >= 0) resolvedIndex = idx
  }

  return { queue: result, resolvedIndex }
}
