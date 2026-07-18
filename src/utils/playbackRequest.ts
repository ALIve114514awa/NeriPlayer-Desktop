export interface DeferredPlaybackSeek {
  requestGeneration: number
  positionMs: number
  seekSeq: number
}

export interface PlaybackLoadStart {
  positionMs: number
  seekSeq: number | null
}

export function shouldDeferPlaybackSeek(
  requestGeneration: number,
  loadedGeneration: number,
): boolean {
  return requestGeneration !== loadedGeneration
}

export function resolvePlaybackLoadStart(
  requestGeneration: number,
  requestedStartMs: number,
  deferredSeek: DeferredPlaybackSeek | null,
): PlaybackLoadStart {
  if (deferredSeek?.requestGeneration === requestGeneration) {
    return {
      positionMs: Math.max(0, Math.round(deferredSeek.positionMs)),
      seekSeq: deferredSeek.seekSeq,
    }
  }
  return {
    positionMs: Math.max(0, Math.round(requestedStartMs)),
    seekSeq: null,
  }
}

export function isPlaybackSeekCompletionCurrent(
  requestGeneration: number,
  currentRequestGeneration: number,
  seekSeq: number,
  currentSeekSeq: number,
): boolean {
  return requestGeneration === currentRequestGeneration
    && seekSeq === currentSeekSeq
}

export function hasVisiblePlaybackSession(
  hasPlaybackSession: boolean,
  trackId: string | null | undefined,
): boolean {
  return hasPlaybackSession && !!trackId
}

export function playbackSessionTrackKey(
  hasPlaybackSession: boolean,
  playlistKey: string | null | undefined,
  trackId: string | null | undefined,
): string {
  if (!hasVisiblePlaybackSession(hasPlaybackSession, trackId)) return 'empty'
  return playlistKey || trackId || 'empty'
}

export function shouldResolvePlaybackSourceInParallel(
  hadPlaybackSession: boolean,
  hasPrefetchedResolution: boolean,
): boolean {
  return !hadPlaybackSession && !hasPrefetchedResolution
}
