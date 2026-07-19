import type { LyricLine, TrackInfo } from '@/stores/player'
import { getCachedValue, removeCachedValue, setCachedValue } from './persistentCache'
import { lyricsIdentity } from './lyricsRequest'

const LYRICS_CACHE_KEY = 'neri:lyrics-cache:v1'
const LYRICS_CACHE_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000
const LYRICS_CACHE_MAX_ENTRIES = 200

function normalizeLyricLine(line: LyricLine): LyricLine {
  return {
    startMs: Number(line.startMs || 0),
    durationMs: Number(line.durationMs || 0),
    words: Array.isArray(line.words)
      ? line.words.map(word => ({
        startMs: Number(word.startMs || 0),
        durationMs: Number(word.durationMs || 0),
        text: String(word.text || ''),
      }))
      : [],
    text: String(line.text || ''),
    translation: line.translation || undefined,
  }
}

function hasVisibleLyric(lines: LyricLine[]): boolean {
  return lines.some(line =>
    line.text.trim().length > 0
    || line.words.some(word => word.text.trim().length > 0),
  )
}

export function getCachedLyrics(track: TrackInfo): LyricLine[] | null {
  const cached = getCachedValue<LyricLine[]>(
    LYRICS_CACHE_KEY,
    lyricsIdentity(track),
    LYRICS_CACHE_MAX_AGE_MS,
  )
  if (!Array.isArray(cached)) return null

  const lines = cached.map(normalizeLyricLine)
  return hasVisibleLyric(lines) ? lines : null
}

export function saveCachedLyrics(track: TrackInfo, lines: LyricLine[]) {
  const normalized = lines.map(normalizeLyricLine)
  if (!hasVisibleLyric(normalized)) {
    clearCachedLyrics(track)
    return
  }

  setCachedValue(LYRICS_CACHE_KEY, lyricsIdentity(track), normalized, {
    maxAgeMs: LYRICS_CACHE_MAX_AGE_MS,
    maxEntries: LYRICS_CACHE_MAX_ENTRIES,
  })
}

export function clearCachedLyrics(track: TrackInfo) {
  removeCachedValue(LYRICS_CACHE_KEY, lyricsIdentity(track))
}
