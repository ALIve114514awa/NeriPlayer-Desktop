import type { TrackInfo } from '@/stores/player'
import { getCachedValue, setCachedValue } from './persistentCache'

const PLAYLIST_DETAIL_CACHE_KEY = 'neri:playlist-detail-cache:v1'
const PLAYLIST_DETAIL_CACHE_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000
const PLAYLIST_DETAIL_CACHE_MAX_ENTRIES = 80

export interface PlaylistDetailCacheValue {
  playlistName?: string
  folderName?: string
  coverUrl?: string
  trackCount?: number
  playCount?: number
  mediaCount?: number
  description?: string
  creator?: string
  subtitle?: string
  tracks: TrackInfo[]
}

function normalizeTrack(track: TrackInfo): TrackInfo {
  return {
    id: String(track.id || ''),
    title: String(track.title || ''),
    artist: String(track.artist || ''),
    album: String(track.album || ''),
    durationMs: Number(track.durationMs || 0),
    coverUrl: String(track.coverUrl || ''),
    audioUrl: String(track.audioUrl || ''),
  }
}

function normalizeDetail(value: PlaylistDetailCacheValue): PlaylistDetailCacheValue {
  return {
    ...value,
    tracks: Array.isArray(value.tracks) ? value.tracks.map(normalizeTrack) : [],
  }
}

export function getCachedPlaylistDetail(key: string): PlaylistDetailCacheValue | null {
  const cached = getCachedValue<PlaylistDetailCacheValue>(
    PLAYLIST_DETAIL_CACHE_KEY,
    key,
    PLAYLIST_DETAIL_CACHE_MAX_AGE_MS,
  )
  if (!cached) return null

  const detail = normalizeDetail(cached)
  return detail.tracks.length > 0 ? detail : null
}

export function saveCachedPlaylistDetail(key: string, value: PlaylistDetailCacheValue) {
  const detail = normalizeDetail(value)
  if (detail.tracks.length === 0) return

  setCachedValue(PLAYLIST_DETAIL_CACHE_KEY, key, detail, {
    maxAgeMs: PLAYLIST_DETAIL_CACHE_MAX_AGE_MS,
    maxEntries: PLAYLIST_DETAIL_CACHE_MAX_ENTRIES,
  })
}

export function playlistDetailCacheKey(kind: string, id: string | number): string {
  return `${kind}:${id}`
}

export function readPlaylistDetailCache<T extends PlaylistDetailCacheValue>(key: string): T | null {
  return getCachedPlaylistDetail(key) as T | null
}

export function writePlaylistDetailCache<T extends PlaylistDetailCacheValue>(key: string, value: T) {
  saveCachedPlaylistDetail(key, value)
}
