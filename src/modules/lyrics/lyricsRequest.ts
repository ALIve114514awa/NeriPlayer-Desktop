import type { LyricLine, TrackInfo } from '@/stores/player'

const inFlightLyrics = new Map<string, Promise<LyricLine[]>>()

export function lyricsIdentity(track: TrackInfo): string {
  if (track.id) return track.id
  return `${track.title.trim()}::${track.artist.trim()}::${track.durationMs || 0}`
}

export function loadLyricsSingleFlight(
  track: TrackInfo,
  loader: () => Promise<LyricLine[]>,
): Promise<LyricLine[]> {
  const key = lyricsIdentity(track)
  const current = inFlightLyrics.get(key)
  if (current) return current

  let request: Promise<LyricLine[]>
  try {
    request = Promise.resolve(loader())
  } catch (error) {
    request = Promise.reject(error)
  }
  inFlightLyrics.set(key, request)
  const release = () => {
    if (inFlightLyrics.get(key) === request) {
      inFlightLyrics.delete(key)
    }
  }
  request.then(release, release)
  return request
}

export function hasLyricsRequestInFlight(track: TrackInfo): boolean {
  return inFlightLyrics.has(lyricsIdentity(track))
}
