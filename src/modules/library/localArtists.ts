// 本地歌手聚合：对齐 Android LocalArtistSummary + sortLocalArtists
//
// 聚合键与 Android localArtistStableKey 一致（trim + lowercase），
// 保证两端对同一批本地文件分出来的歌手组完全相同。

import type { TrackInfo } from '@/stores/player'

export type LocalArtistSortMode = 'name' | 'song_count' | 'recent'

export interface LocalArtistSummary {
  key: string
  name: string
  tracks: TrackInfo[]
  coverUrl?: string
  latestAddedAt: number
}

export function localArtistStableKey(name: string): string {
  return name.trim().toLowerCase()
}

/// 多歌手字段按常见分隔符拆开，避免「A/B」被当成一个独立歌手
const ARTIST_SEPARATORS = /[/、,;&]|\bfeat\.?\b|\bft\.?\b|\bvs\.?\b/i

export function splitArtistNames(raw: string): string[] {
  const names = raw
    .split(ARTIST_SEPARATORS)
    .map((name) => name.trim())
    .filter(Boolean)
  return names.length ? names : []
}

export function groupLocalArtists(
  tracks: readonly TrackInfo[],
  unknownLabel: string,
): LocalArtistSummary[] {
  const grouped = new Map<string, LocalArtistSummary>()

  for (const track of tracks) {
    const names = splitArtistNames(track.artist ?? '')
    const effective = names.length ? names : [unknownLabel]
    for (const name of effective) {
      const key = localArtistStableKey(name)
      let entry = grouped.get(key)
      if (!entry) {
        entry = { key, name, tracks: [], coverUrl: undefined, latestAddedAt: 0 }
        grouped.set(key, entry)
      }
      entry.tracks.push(track)
      if (!entry.coverUrl && track.coverUrl) entry.coverUrl = track.coverUrl
      entry.latestAddedAt = Math.max(entry.latestAddedAt, track.addedAt ?? 0)
    }
  }

  return [...grouped.values()]
}

export function sortLocalArtists(
  artists: readonly LocalArtistSummary[],
  mode: LocalArtistSortMode,
): LocalArtistSummary[] {
  const byName = (left: LocalArtistSummary, right: LocalArtistSummary) =>
    left.name.toLowerCase().localeCompare(right.name.toLowerCase())

  const sorted = [...artists]
  switch (mode) {
    case 'song_count':
      sorted.sort((left, right) => right.tracks.length - left.tracks.length || byName(left, right))
      break
    case 'recent':
      sorted.sort((left, right) => right.latestAddedAt - left.latestAddedAt || byName(left, right))
      break
    default:
      sorted.sort(byName)
  }
  return sorted
}

/// 搜索同时匹配歌手名与其名下曲目，对齐 Android matchesLocalArtistSearch
export function filterLocalArtists(
  artists: readonly LocalArtistSummary[],
  query: string,
): LocalArtistSummary[] {
  const trimmed = query.trim().toLowerCase()
  if (!trimmed) return [...artists]
  return artists.filter(
    (artist) =>
      artist.name.toLowerCase().includes(trimmed) ||
      artist.tracks.some(
        (track) =>
          track.title.toLowerCase().includes(trimmed) ||
          (track.album ?? '').toLowerCase().includes(trimmed),
      ),
  )
}
