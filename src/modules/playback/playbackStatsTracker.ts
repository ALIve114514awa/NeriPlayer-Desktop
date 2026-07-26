// 播放统计计时器：逐条移植 Android PlaybackStatsTracker
//
// 关键语义（改动前请先对照 Android 实现）：
// - 用墙钟累计「实际在播的时长」，不是位置差值：变速播放不会放大或缩小统计
// - 单次连续收听 >= 30s，或整轨播完，才 +1 次播放
// - 手动 seek 到开头不能被误判成循环播放

export const PLAYBACK_STATS_PERIODIC_FLUSH_MS = 15_000
const MIN_LISTEN_MS_FOR_PLAY_COUNT = 30_000
const POSITION_WRAP_TOLERANCE_MS = 3_000
const NO_ACTIVE_SEGMENT = -1

export interface StatsTrackKey {
  identityKey: string
  durationMs: number
}

export interface PlaybackStatsSnapshot<T extends StatsTrackKey> {
  track: T
  listenedMs: number
  playCountIncrement: number
  scheduleSync: boolean
}

export class PlaybackStatsTracker<T extends StatsTrackKey> {
  private trackingTrack: T | null = null
  private trackingKey: string | null = null
  private segmentStartMs = NO_ACTIVE_SEGMENT
  private accumulatedMs = 0
  private currentPlayListenedMs = 0
  private playing = false
  private hasCountedCurrentPlay = false
  private lastPositionMs: number | null = null
  private suppressNextPositionWrap = false

  constructor(
    private readonly periodicFlushMs = PLAYBACK_STATS_PERIODIC_FLUSH_MS,
    private readonly now: () => number = () => performance.now(),
  ) {}

  onTrackChanged(track: T | null): PlaybackStatsSnapshot<T> | null {
    const nextKey = track?.identityKey ?? null
    if (nextKey === this.trackingKey) {
      if (track) this.trackingTrack = track
      return null
    }

    this.collectActiveSegment()
    const snapshot = this.flush(this.shouldCountCurrentPlay(), true)
    this.trackingTrack = track
    this.trackingKey = nextKey
    this.accumulatedMs = 0
    this.currentPlayListenedMs = 0
    this.hasCountedCurrentPlay = false
    this.lastPositionMs = null
    this.suppressNextPositionWrap = false
    this.segmentStartMs = NO_ACTIVE_SEGMENT
    return snapshot
  }

  onPlayingChanged(playing: boolean): PlaybackStatsSnapshot<T> | null {
    if (playing === this.playing) {
      if (playing) this.startSegmentIfNeeded()
      return null
    }
    if (playing) {
      this.playing = true
      this.startSegmentIfNeeded()
      return null
    }
    this.collectActiveSegment()
    this.playing = false
    this.segmentStartMs = NO_ACTIVE_SEGMENT
    return this.flush(this.shouldCountCurrentPlay(), true)
  }

  /// 位置回绕检测：上一次接近结尾 + 这一次接近开头 = 单曲循环了一轮
  onProgress(positionMs: number): PlaybackStatsSnapshot<T> | null {
    const track = this.trackingTrack
    if (!track) return null
    const position = Math.max(0, positionMs)
    const previous = this.lastPositionMs
    this.lastPositionMs = position
    if (this.suppressNextPositionWrap) {
      this.suppressNextPositionWrap = false
      return null
    }
    const durationMs = track.durationMs > 0 ? track.durationMs : 0
    if (!durationMs) return null
    if (previous === null || previous <= position) return null

    const nearEnd = previous >= Math.max(0, durationMs - POSITION_WRAP_TOLERANCE_MS)
    const nearStart = position <= POSITION_WRAP_TOLERANCE_MS
    return nearEnd && nearStart ? this.onTrackEnded() : null
  }

  onTrackEnded(): PlaybackStatsSnapshot<T> | null {
    this.collectActiveSegment()
    const snapshot = this.flush(true, true)
    this.hasCountedCurrentPlay = false
    this.currentPlayListenedMs = 0
    this.lastPositionMs = null
    if (this.trackingTrack && this.playing) {
      this.segmentStartMs = this.now()
    }
    return snapshot
  }

  onManualSeek(positionMs: number): void {
    this.lastPositionMs = Math.max(0, positionMs)
    this.suppressNextPositionWrap = true
  }

  flushPeriodic(): PlaybackStatsSnapshot<T> | null {
    this.collectActiveSegment()
    return this.flush(this.shouldCountCurrentPlay(), false)
  }

  flushFinal(): PlaybackStatsSnapshot<T> | null {
    this.collectActiveSegment()
    return this.flush(this.shouldCountCurrentPlay(), true)
  }

  shouldFlushPeriodically(): boolean {
    if (!this.playing || !this.trackingTrack || this.periodicFlushMs <= 0) return false
    return this.accumulatedMs + this.activeSegmentMs() >= this.periodicFlushMs
  }

  private collectActiveSegment(): void {
    if (!this.playing || this.segmentStartMs === NO_ACTIVE_SEGMENT) return
    const now = this.now()
    if (now > this.segmentStartMs) {
      const delta = now - this.segmentStartMs
      this.accumulatedMs += delta
      this.currentPlayListenedMs += delta
    }
    this.segmentStartMs = this.trackingTrack ? now : NO_ACTIVE_SEGMENT
  }

  private startSegmentIfNeeded(): void {
    if (this.trackingTrack && this.segmentStartMs === NO_ACTIVE_SEGMENT) {
      this.segmentStartMs = this.now()
    }
  }

  private flush(countPlay: boolean, scheduleSync: boolean): PlaybackStatsSnapshot<T> | null {
    const track = this.trackingTrack
    if (!track) return null
    const listenedMs = Math.max(0, Math.round(this.accumulatedMs))
    const playCountIncrement = countPlay && !this.hasCountedCurrentPlay ? 1 : 0
    if (listenedMs <= 0 && playCountIncrement <= 0) return null

    this.accumulatedMs = 0
    if (playCountIncrement > 0) this.hasCountedCurrentPlay = true
    return { track, listenedMs, playCountIncrement, scheduleSync }
  }

  private activeSegmentMs(): number {
    if (this.segmentStartMs === NO_ACTIVE_SEGMENT) return 0
    return Math.max(0, this.now() - this.segmentStartMs)
  }

  private shouldCountCurrentPlay(): boolean {
    return (
      !this.hasCountedCurrentPlay &&
      this.currentPlayListenedMs >= MIN_LISTEN_MS_FOR_PLAY_COUNT
    )
  }
}
