export interface PlaybackQueueTrackIdentity {
  id: string
  playlistKey?: string
}

export function resolvePlaybackQueueStartIndex(
  tracks: readonly PlaybackQueueTrackIdentity[],
  requestedTrackId?: string,
  requestedPlaylistKey?: string,
): number {
  if (tracks.length === 0) return -1

  const normalizedPlaylistKey = requestedPlaylistKey?.trim()
  if (normalizedPlaylistKey) {
    const requestedIndex = tracks.findIndex(
      track => track.playlistKey === normalizedPlaylistKey,
    )
    if (requestedIndex >= 0) return requestedIndex
  }

  const normalizedTrackId = requestedTrackId?.trim()
  if (!normalizedTrackId) return 0

  const requestedIndex = tracks.findIndex(track => track.id === normalizedTrackId)
  return requestedIndex >= 0 ? requestedIndex : 0
}
