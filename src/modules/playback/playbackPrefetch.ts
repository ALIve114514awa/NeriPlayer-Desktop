import type { TrackInfo } from '@/stores/player'
import {
  isRemotePlaybackTrack,
  playbackPrefetchCacheId,
  type PlaybackSourceSettings,
  type PlaybackUrlResolver,
  type ResolvedPlaybackSource,
} from './playbackSource'
import { PlaybackDemandArbiter } from './playbackPolicy'

interface PrefetchEntry {
  result: ResolvedPlaybackSource
  expiresAt: number
}

const DEFAULT_TTL_MS = 90_000
const DEFAULT_MAX_ENTRIES = 16

export class PlaybackPrefetchManager {
  readonly demandArbiter = new PlaybackDemandArbiter()

  private readonly entries = new Map<string, PrefetchEntry>()
  private readonly jobs = new Map<string, Promise<void>>()
  private readonly ttlMs: number
  private readonly maxEntries: number
  private currentDemandKey: string | null = null

  constructor(ttlMs = DEFAULT_TTL_MS, maxEntries = DEFAULT_MAX_ENTRIES) {
    this.ttlMs = Math.max(1, ttlMs)
    this.maxEntries = Math.max(1, maxEntries)
  }

  replacePlaybackDemand(cacheKey: string | null): void {
    this.demandArbiter.replacePlaybackDemand(this.currentDemandKey, cacheKey)
    this.currentDemandKey = cacheKey
  }

  prefetch(
    track: TrackInfo,
    settings: PlaybackSourceSettings,
    resolver: PlaybackUrlResolver,
  ): void {
    if (!isRemotePlaybackTrack(track)) return
    const cacheKey = playbackPrefetchCacheId(track, settings)
    if (this.demandArbiter.shouldYieldPrefetch(cacheKey)) return
    if (this.hasFresh(cacheKey)) return
    if (this.jobs.has(cacheKey)) return

    const job = resolver.resolve(track, settings).then((resolution) => {
      if (resolution.type !== 'success') return
      if (this.demandArbiter.shouldYieldPrefetch(cacheKey)) return
      this.put(cacheKey, resolution)
    }).catch(() => {
      // 预热失败不影响当前播放
    }).finally(() => {
      if (this.jobs.get(cacheKey) === job) this.jobs.delete(cacheKey)
    })

    this.jobs.set(cacheKey, job)
  }

  prefetchWindow(
    tracks: TrackInfo[],
    settings: PlaybackSourceSettings,
    resolver: PlaybackUrlResolver,
  ): void {
    for (const track of tracks) this.prefetch(track, settings, resolver)
  }

  take(track: TrackInfo, settings: PlaybackSourceSettings): ResolvedPlaybackSource | null {
    const cacheKey = playbackPrefetchCacheId(track, settings)
    const entry = this.entries.get(cacheKey)
    if (!entry) return null
    this.entries.delete(cacheKey)
    return entry.expiresAt > Date.now() ? entry.result : null
  }

  clearForTrack(track: TrackInfo, settings: PlaybackSourceSettings): void {
    const cacheKey = playbackPrefetchCacheId(track, settings)
    this.entries.delete(cacheKey)
    this.jobs.delete(cacheKey)
  }

  clear(): void {
    this.entries.clear()
    this.jobs.clear()
  }

  private hasFresh(cacheKey: string): boolean {
    const entry = this.entries.get(cacheKey)
    if (!entry) return false
    if (entry.expiresAt > Date.now()) return true
    this.entries.delete(cacheKey)
    return false
  }

  private put(cacheKey: string, result: ResolvedPlaybackSource): void {
    this.removeExpired()
    if (!this.entries.has(cacheKey) && this.entries.size >= this.maxEntries) {
      const oldestKey = this.entries.keys().next().value as string | undefined
      if (oldestKey) this.entries.delete(oldestKey)
    }
    this.entries.set(cacheKey, {
      result,
      expiresAt: Date.now() + this.ttlMs,
    })
  }

  private removeExpired(): void {
    const now = Date.now()
    for (const [key, entry] of this.entries) {
      if (entry.expiresAt <= now) this.entries.delete(key)
    }
  }
}
