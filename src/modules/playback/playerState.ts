export interface PlaybackStateTrackIdentity {
  id: string
  playlistKey?: string
}

export interface PersistedPlaybackQueue<T> {
  queue: T[]
  queueIndex: number
  hasPlaybackSession: boolean
  currentTrackId?: string
  currentTrackPlaylistKey?: string
}

export interface RestoredPlaybackQueue<T> {
  queue: T[]
  queueIndex: number
  currentTrack: T | null
  hasPlaybackSession: boolean
}

export function buildPersistedPlaybackQueue<T extends PlaybackStateTrackIdentity>(
  queue: readonly T[],
  queueIndex: number,
  currentTrack: T | null,
  compact: boolean,
): PersistedPlaybackQueue<T> {
  const persistedQueue = compact
    ? currentTrack ? [currentTrack] : []
    : [...queue]
  let persistedIndex = -1
  if (persistedQueue.length > 0 && currentTrack) {
    persistedIndex = compact
      ? 0
      : clampQueueIndex(queueIndex, persistedQueue.length)
  }

  return {
    queue: persistedQueue,
    queueIndex: persistedIndex,
    hasPlaybackSession: currentTrack !== null,
    currentTrackId: currentTrack?.id,
    currentTrackPlaylistKey: currentTrack?.playlistKey,
  }
}

export function restorePersistedPlaybackQueue<T extends PlaybackStateTrackIdentity>(
  rawQueue: unknown,
  rawQueueIndex: unknown,
  rawHasPlaybackSession: unknown,
  currentTrackId: unknown,
  currentTrackPlaylistKey: unknown,
  normalizeTrack: (raw: unknown) => T,
  canRestoreTrack: (track: T) => boolean,
): RestoredPlaybackQueue<T> {
  const queue = Array.isArray(rawQueue)
    ? rawQueue.flatMap((raw) => {
        try {
          const track = normalizeTrack(raw)
          return canRestoreTrack(track) ? [track] : []
        } catch {
          return []
        }
      })
    : []

  if (queue.length === 0) {
    return {
      queue,
      queueIndex: -1,
      currentTrack: null,
      hasPlaybackSession: false,
    }
  }

  const hasPersistedIdentity = (
    typeof currentTrackId === 'string' && currentTrackId.length > 0
  ) || (
    typeof currentTrackPlaylistKey === 'string' && currentTrackPlaylistKey.length > 0
  )
  const explicitlyWithoutSession = rawHasPlaybackSession === false
  const legacyStateWithoutSession = rawHasPlaybackSession === undefined
    && !hasPersistedIdentity
    && typeof rawQueueIndex === 'number'
    && rawQueueIndex < 0
  if (explicitlyWithoutSession || legacyStateWithoutSession) {
    return {
      queue,
      queueIndex: -1,
      currentTrack: null,
      hasPlaybackSession: false,
    }
  }

  const restoredTrackIndex = findTrackIndex(
    queue,
    typeof currentTrackId === 'string' ? currentTrackId : '',
    typeof currentTrackPlaylistKey === 'string' ? currentTrackPlaylistKey : '',
  )
  const fallbackIndex = typeof rawQueueIndex === 'number'
    ? clampQueueIndex(rawQueueIndex, queue.length)
    : 0
  const queueIndex = restoredTrackIndex >= 0 ? restoredTrackIndex : fallbackIndex
  const currentTrack = queue[queueIndex] ?? null

  return {
    queue,
    queueIndex,
    currentTrack,
    hasPlaybackSession: currentTrack !== null,
  }
}

function findTrackIndex<T extends PlaybackStateTrackIdentity>(
  queue: readonly T[],
  currentTrackId: string,
  currentTrackPlaylistKey: string,
): number {
  if (currentTrackPlaylistKey) {
    const playlistIndex = queue.findIndex(track => track.playlistKey === currentTrackPlaylistKey)
    if (playlistIndex >= 0) return playlistIndex
  }
  if (currentTrackId) {
    return queue.findIndex(track => track.id === currentTrackId)
  }
  return -1
}

function clampQueueIndex(queueIndex: number, queueLength: number): number {
  if (queueLength <= 0) return -1
  const integerIndex = Number.isFinite(queueIndex) ? Math.trunc(queueIndex) : 0
  return Math.min(Math.max(integerIndex, 0), queueLength - 1)
}
