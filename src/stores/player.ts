import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { useHistoryStore } from './history'
import { useToastStore } from './toast'
import { useSettingsStore } from './settings'
import i18n from '@/i18n'

export interface TrackInfo {
  id: string
  title: string
  artist: string
  album: string
  durationMs: number
  coverUrl: string
  audioUrl: string
}

/**
 * 将后端返回的 snake_case TrackInfo 映射为前端 camelCase。
 * 前端手动构造的对象已经是 camelCase，此函数同时兼容两种格式
 */
export function normalizeTrack(raw: any): TrackInfo {
  return {
    id: raw.id ?? '',
    title: raw.title ?? '',
    artist: raw.artist ?? '',
    album: raw.album ?? '',
    durationMs: raw.durationMs ?? raw.duration_ms ?? 0,
    coverUrl: raw.coverUrl ?? raw.cover_url ?? '',
    audioUrl: raw.audioUrl ?? raw.audio_url ?? raw.url ?? '',
  }
}

/** UI 显示用的专辑名：清理 B站 "Bilibili|{cid}" 等内部格式 */
export function displayAlbum(album: string): string {
  if (album.startsWith('Bilibili|') || album === 'Bilibili') return 'Bilibili'
  if (album.startsWith('Netease')) return album.replace(/^Netease/, '').trim() || album
  return album
}

export interface LyricWord {
  startMs: number
  durationMs: number
  text: string
}

export interface LyricLine {
  startMs: number
  durationMs: number
  words: LyricWord[]
  text: string
  translation?: string
}

export type RepeatMode = 'off' | 'all' | 'one'

// ─── 均衡器预设（5频段: 60Hz, 230Hz, 910Hz, 3.6kHz, 14kHz，单位 mB） ───
export const EQ_PRESETS: Record<string, number[]> = {
  flat:           [0, 0, 0, 0, 0],
  acoustic:       [300, 200, 0, 100, 200],
  bass_boost:     [600, 400, 0, 0, 0],
  bass_reduce:    [-600, -400, 0, 0, 0],
  classical:      [400, 200, -100, 200, 300],
  dance:          [500, 200, 100, -100, 200],
  deep:           [500, 300, 100, -100, -200],
  electronic:     [500, 300, 0, 100, 400],
  hip_hop:        [500, 300, 0, 100, 300],
  jazz:           [300, 100, -100, 100, 300],
  latin:          [300, 0, -100, 200, 400],
  loudness:       [500, 200, 0, -100, -200],
  lounge:         [-200, -100, 0, 100, 200],
  piano:          [200, 100, 0, 100, 200],
  pop:            [-100, 200, 400, 200, -100],
  rnb:            [500, 400, 100, -100, 200],
  rock:           [400, 200, -100, 200, 400],
  small_speakers: [400, 200, 100, 200, 400],
  spoken_word:    [-200, 0, 300, 200, -100],
  treble_boost:   [0, 0, 0, 400, 600],
  treble_reduce:  [0, 0, 0, -400, -600],
  vocal_boost:    [-200, 0, 400, 300, 0],
  custom:         [0, 0, 0, 0, 0],
}

// ─── 播放位置插值状态（模块级，rAF 驱动） ───
let _interpAnchorMs = 0         // 上次后端报告的位置
let _interpAnchorTime = 0       // 对应的 performance.now() 锚点
let _interpRenderedMs = 0       // 上次渲染的插值位置
let _interpSpeed = 1.0          // 当前播放速度快照
let _interpIsPlaying = false    // 当前播放状态快照
let _interpDurationMs = 0       // 当前时长快照
let _interpLoopStarted = false  // rAF 循环是否已启动

// ─── 批次 1.2: 连续失败熔断 ───
let consecutivePlayFailures = 0
const MAX_CONSECUTIVE_FAILURES = 10
let _isAutoSkipping = false

// ─── 批次 2.1: Shuffle 三栈模型 ───
let shuffleBag: number[] = []       // 未播放索引池
let shuffleHistory: number[] = []   // 已播放栈 (previous 回溯)
let shuffleFuture: number[] = []    // 预排队栈 (next 或 previous 回退)

// ─── 批次 2.2: playbackRequestToken 防竞态 ───
let playbackRequestToken = 0

// ─── 批次 2.3: URL 过期检测 (10min) ───
let lastUrlResolveTime = 0
const URL_EXPIRY_MS = 10 * 60 * 1000

// ─── 批次 2.4: Track End 去重 ───
let lastTrackEndedId: string | null = null
let lastTrackEndedTime = 0

// ─── 批次 5.1: YouTube URL 预热缓存 ───
const prefetchedUrls = new Map<string, { url: string; time: number; bitrate: number; mime_type: string }>()
const PREFETCH_TTL_MS = 8 * 60 * 1000

// ─── 批次 1.1: 状态持久化 ───
const PLAYER_STATE_KEY = 'neri:player-state'
let _persistDebounceTimer: ReturnType<typeof setTimeout> | null = null
let _progressPersistTime = 0
const PERSIST_DEBOUNCE_MS = 250
const PROGRESS_PERSIST_INTERVAL_MS = 15000
// 恢复后需重新加载标记
let _needsReload = false

export const usePlayerStore = defineStore('player', () => {
  const isPlaying = ref(false)
  const currentTrack = ref<TrackInfo | null>(null)
  const positionMs = ref(0)
  const durationMs = ref(0)
  const queue = ref<TrackInfo[]>([])
  const queueIndex = ref(-1)
  const repeatMode = ref<RepeatMode>('off')
  const shuffleEnabled = ref(false)
  const volume = ref(1)
  const lyrics = ref<LyricLine[]>([])

  // 播放错误信息（供 UI 展示）
  const playError = ref<string | null>(null)
  // 是否正在加载音频（下载/解码中）
  const isLoadingAudio = ref(false)

  // 当前音频质量信息
  interface AudioInfo {
    bitrate?: number  // kbps
    codec?: string    // e.g. "MP3", "FLAC", "AAC", "Opus"
    format?: string   // 原始格式标识
  }
  const audioInfo = ref<AudioInfo | null>(null)

  // 睡眠定时器
  const sleepTimerEndMs = ref(0) // 0 = 未启用
  const sleepTimerMode = ref<'countdown' | 'end_of_track' | 'end_of_queue' | null>(null)
  let _sleepTimerInterval: ReturnType<typeof setInterval> | null = null

  /** 剩余睡眠时间（秒） */
  const sleepRemainingSeconds = computed(() => {
    if (!sleepTimerMode.value || sleepTimerEndMs.value <= 0) return 0
    if (sleepTimerMode.value === 'end_of_track') return -1 // 特殊标记
    return Math.max(0, Math.ceil((sleepTimerEndMs.value - Date.now()) / 1000))
  })

  function startSleepTimer(minutes: number) {
    cancelSleepTimer()
    sleepTimerMode.value = 'countdown'
    sleepTimerEndMs.value = Date.now() + minutes * 60 * 1000
    _sleepTimerInterval = setInterval(() => {
      if (Date.now() >= sleepTimerEndMs.value) {
        pause()
        cancelSleepTimer()
      }
    }, 1000)
  }

  function startSleepTimerEndOfTrack() {
    cancelSleepTimer()
    sleepTimerMode.value = 'end_of_track'
    sleepTimerEndMs.value = 1 // 非零表示启用
  }

  function startSleepTimerEndOfQueue() {
    cancelSleepTimer()
    sleepTimerMode.value = 'end_of_queue'
    sleepTimerEndMs.value = 1
  }

  function cancelSleepTimer() {
    if (_sleepTimerInterval) {
      clearInterval(_sleepTimerInterval)
      _sleepTimerInterval = null
    }
    sleepTimerMode.value = null
    sleepTimerEndMs.value = 0
  }

  // 音频分析数据
  const audioLevel = ref(0)
  const beatImpulse = ref(0)

  // 插值后的播放位置（rAF 驱动，60fps 平滑）
  const interpolatedPositionMs = ref(0)
  const interpolatedProgress = computed(() =>
    durationMs.value > 0 ? interpolatedPositionMs.value / durationMs.value : 0
  )

  const progress = computed(() =>
    durationMs.value > 0 ? positionMs.value / durationMs.value : 0
  )
  const currentTimeFormatted = computed(() => formatTime(positionMs.value))
  const durationFormatted = computed(() => formatTime(durationMs.value))

  // 是否已初始化事件监听
  let eventsInitialized = false
  // seek 后忽略 position 事件的时间窗口
  let seekGuardUntil = 0
  // 记住最后 seek 的位置，用于 resume 时重新 seek（防止后端丢失 seek-while-paused）
  let lastSeekedMs: number | null = null
  // seek 后第一次接受 position 事件时的目标位置，用于检测偏差过大的旧事件
  let seekTargetMs: number | null = null

  // ─── 状态持久化函数 ───

  /** 保存播放器状态到 localStorage（250ms debounce） */
  function savePlayerState() {
    if (_persistDebounceTimer) clearTimeout(_persistDebounceTimer)
    _persistDebounceTimer = setTimeout(() => {
      _persistDebounceTimer = null
      try {
        const settings = useSettingsStore()
        const state: Record<string, any> = {
          queue: queue.value,
          queueIndex: queueIndex.value,
          volume: volume.value,
        }
        if (settings.keepProgress) {
          state.positionMs = positionMs.value
        }
        if (settings.keepPlaybackMode) {
          state.repeatMode = repeatMode.value
          state.shuffleEnabled = shuffleEnabled.value
        }
        localStorage.setItem(PLAYER_STATE_KEY, JSON.stringify(state))
      } catch { /* 存储失败忽略 */ }
    }, PERSIST_DEBOUNCE_MS)
  }

  /** 从 localStorage 恢复播放器状态（store 初始化时调用，不自动播放） */
  function loadPlayerState() {
    try {
      const raw = localStorage.getItem(PLAYER_STATE_KEY)
      if (!raw) return
      const state = JSON.parse(raw)
      const settings = useSettingsStore()

      if (Array.isArray(state.queue) && state.queue.length > 0) {
        queue.value = state.queue.map(normalizeTrack)
        queueIndex.value = typeof state.queueIndex === 'number'
          ? Math.min(Math.max(state.queueIndex, 0), queue.value.length - 1)
          : 0
        currentTrack.value = queue.value[queueIndex.value] ?? null
        _needsReload = true
      }

      if (settings.keepProgress && typeof state.positionMs === 'number') {
        positionMs.value = state.positionMs
        interpolatedPositionMs.value = state.positionMs
      }

      if (settings.keepPlaybackMode) {
        if (state.repeatMode && ['off', 'all', 'one'].includes(state.repeatMode)) {
          repeatMode.value = state.repeatMode
        }
        if (typeof state.shuffleEnabled === 'boolean') {
          shuffleEnabled.value = state.shuffleEnabled
          if (state.shuffleEnabled && queue.value.length > 1) {
            rebuildShuffleBag()
          }
        }
      }

      if (typeof state.volume === 'number') {
        volume.value = Math.max(0, Math.min(1, state.volume))
      }

      // 恢复 durationMs 以便 UI 显示进度条
      if (currentTrack.value && currentTrack.value.durationMs > 0) {
        durationMs.value = currentTrack.value.durationMs
      }
    } catch { /* 恢复失败忽略 */ }
  }

  /** 节流保存进度（每 15s，对齐 Android scheduleStatePersist） */
  function maybePersistProgress() {
    const now = Date.now()
    if (now - _progressPersistTime >= PROGRESS_PERSIST_INTERVAL_MS) {
      _progressPersistTime = now
      savePlayerState()
    }
  }

  // ─── Shuffle 三栈辅助函数 ───

  /** Fisher-Yates 洗牌重建 shuffleBag，排除当前索引 */
  function rebuildShuffleBag() {
    shuffleBag = []
    for (let i = 0; i < queue.value.length; i++) {
      if (i !== queueIndex.value) shuffleBag.push(i)
    }
    for (let i = shuffleBag.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [shuffleBag[i], shuffleBag[j]] = [shuffleBag[j], shuffleBag[i]]
    }
  }

  /** 队列插入后更新 shuffle 索引 */
  function shiftShuffleIndicesForInsert(insertIdx: number) {
    shuffleBag = shuffleBag.map(i => i >= insertIdx ? i + 1 : i)
    shuffleHistory = shuffleHistory.map(i => i >= insertIdx ? i + 1 : i)
    shuffleFuture = shuffleFuture.map(i => i >= insertIdx ? i + 1 : i)
    shuffleBag.push(insertIdx) // 新曲目加入未播放池
  }

  /** 队列移除后更新 shuffle 索引 */
  function shiftShuffleIndicesForRemove(removeIdx: number) {
    shuffleBag = shuffleBag.filter(i => i !== removeIdx).map(i => i > removeIdx ? i - 1 : i)
    shuffleHistory = shuffleHistory.filter(i => i !== removeIdx).map(i => i > removeIdx ? i - 1 : i)
    shuffleFuture = shuffleFuture.filter(i => i !== removeIdx).map(i => i > removeIdx ? i - 1 : i)
  }

  // ─── YouTube URL 预热 ───

  /** 预热下一首 YouTube 曲目的 URL */
  function maybePrefetchNext() {
    if (!queue.value.length) return
    let nextIdx: number
    if (shuffleEnabled.value) {
      if (shuffleFuture.length > 0) nextIdx = shuffleFuture[shuffleFuture.length - 1]
      else if (shuffleBag.length > 0) nextIdx = shuffleBag[0]
      else return
    } else {
      nextIdx = queueIndex.value + 1
      if (nextIdx >= queue.value.length) {
        if (repeatMode.value === 'all') nextIdx = 0
        else return
      }
    }

    const nextTrack = queue.value[nextIdx]
    if (!nextTrack || !nextTrack.id.startsWith('youtube:')) return

    // 缓存未过期则跳过
    const cached = prefetchedUrls.get(nextTrack.id)
    if (cached && Date.now() - cached.time < PREFETCH_TTL_MS) return

    const videoId = nextTrack.id.replace('youtube:', '')
    invoke<{ url: string; bitrate: number; mime_type: string }[]>('get_youtube_audio_url', { videoId })
      .then(streams => {
        const best = streams?.[0]
        if (best?.url) {
          prefetchedUrls.set(nextTrack.id, {
            url: best.url, time: Date.now(), bitrate: best.bitrate, mime_type: best.mime_type,
          })
        }
      })
      .catch(() => {}) // 预热失败不影响主流程
  }

  /** 启动 rAF 插值循环（仅调用一次） */
  function _startInterpolationLoop() {
    if (_interpLoopStarted) return
    _interpLoopStarted = true

    function tick() {
      requestAnimationFrame(tick)

      if (!_interpIsPlaying) {
        // 非播放时直接使用后端值
        interpolatedPositionMs.value = positionMs.value
        return
      }

      const now = performance.now()
      const elapsed = (now - _interpAnchorTime) * _interpSpeed
      const predicted = _interpAnchorMs + elapsed
      const clamped = Math.max(0, Math.min(predicted, _interpDurationMs))

      // 向后容忍 24ms（防抖动：后端偶尔报告比预测稍早的位置）
      if (clamped < _interpRenderedMs - 24) {
        // 后端回退过多，snap
        _interpRenderedMs = clamped
      } else {
        _interpRenderedMs = Math.max(_interpRenderedMs, clamped)
      }

      // Snap 阈值：与后端差距超过 220ms 直接跳转
      const backendPos = positionMs.value
      if (Math.abs(_interpRenderedMs - backendPos) > 220) {
        _interpRenderedMs = backendPos
        _interpAnchorMs = backendPos
        _interpAnchorTime = now
      }

      interpolatedPositionMs.value = Math.round(_interpRenderedMs)
    }

    requestAnimationFrame(tick)
  }

  function initEvents() {
    if (eventsInitialized) return
    eventsInitialized = true
    _startInterpolationLoop()

    // 监听后端播放位置更新
    listen<{ positionMs: number; durationMs: number }>('player:position', (e) => {
      // seek 后时间窗口内忽略旧位置事件
      if (Date.now() < seekGuardUntil) return
      // guard 过期后，检测第一批事件是否偏离 seek 目标过远（>3s = 后端还没跳到位）
      if (seekTargetMs !== null) {
        const delta = Math.abs(e.payload.positionMs - seekTargetMs)
        if (delta > 3000) return // 丢弃偏差过大的旧事件
        seekTargetMs = null // 第一个合理事件通过后清除
      }
      positionMs.value = e.payload.positionMs
      durationMs.value = e.payload.durationMs

      // 更新插值锚点
      _interpAnchorMs = e.payload.positionMs
      _interpAnchorTime = performance.now()
      _interpDurationMs = e.payload.durationMs

      // 节流保存播放进度（每 15s）
      if (_interpIsPlaying) {
        maybePersistProgress()
      }
    })

    // 监听音频电平
    listen<{ level: number; beat: number }>('player:audio-level', (e) => {
      audioLevel.value = e.payload.level
      beatImpulse.value = e.payload.beat
    })

    // 监听播放完成（对齐 Android handleTrackEnded）
    listen('player:track-ended', () => {
      handleTrackEnded()
    })

    // ─── 系统媒体键事件（SMTC / MPRIS，来自 Rust 后端） ───
    listen<{ isPlaying: boolean }>('media:play-state-changed', (e) => {
      isPlaying.value = e.payload.isPlaying
      _interpIsPlaying = e.payload.isPlaying
      if (e.payload.isPlaying) {
        _interpAnchorMs = positionMs.value
        _interpAnchorTime = performance.now()
        _interpRenderedMs = positionMs.value
        _interpSpeed = playbackSpeed.value
      }
    })

    listen('media:next', () => {
      next()
    })

    listen('media:previous', () => {
      previous()
    })

    listen<{ positionMs: number }>('media:seeked', (e) => {
      positionMs.value = e.payload.positionMs
      _interpAnchorMs = e.payload.positionMs
      _interpAnchorTime = performance.now()
      _interpRenderedMs = e.payload.positionMs
      interpolatedPositionMs.value = e.payload.positionMs
    })
  }

  async function play(track: TrackInfo) {
    initEvents()
    const token = ++playbackRequestToken

    // 加入队列
    if (!queue.value.find(t => t.id === track.id)) {
      queue.value.push(track)
    }
    currentTrack.value = track
    queueIndex.value = queue.value.findIndex(t => t.id === track.id)

    try {
      let dur: number
      playError.value = null
      isLoadingAudio.value = true
      audioInfo.value = null

      // Crossfade 判断：当前有正在播放的曲目且 crossfade 开启
      const settings = useSettingsStore()
      const useCrossfade = isPlaying.value && settings.crossfadeNext
        && settings.crossfadeOutDuration > 0 && settings.crossfadeInDuration > 0

      if (track.id.startsWith('netease:')) {
        // 网易云：先获取播放 URL，再调用 play_url
        const songId = parseInt(track.id.replace('netease:', ''))
        const urlResult = await invoke<{ url: string | null; bitrate: number; format: string }>('get_netease_song_url', {
          songId, quality: settings.neteaseQuality,
        })
        if (token !== playbackRequestToken) return
        if (urlResult.url) {
          if (useCrossfade) {
            dur = await invoke<number>('crossfade_url', {
              url: urlResult.url, durationHintMs: track.durationMs,
              fadeOutMs: settings.crossfadeOutDuration, fadeInMs: settings.crossfadeInDuration,
            })
          } else {
            dur = await invoke<number>('play_url', { url: urlResult.url, durationHintMs: track.durationMs })
          }
          if (token !== playbackRequestToken) return
          audioInfo.value = {
            bitrate: urlResult.bitrate > 0 ? Math.round(urlResult.bitrate / 1000) : undefined,
            codec: urlResult.format ? urlResult.format.toUpperCase() : undefined,
            format: urlResult.format || undefined,
          }
        } else {
          throw new Error('No playback URL')
        }
      } else if (track.id.startsWith('bilibili:')) {
        // B站：id 可能是 bvid 或 avid（同步歌曲）
        const biliId = track.id.replace('bilibili:', '')
        const isAvid = /^\d+$/.test(biliId)
        // 从 album 提取 cid（Android 格式："Bilibili|{cid}"）
        const cidMatch = track.album?.match(/^Bilibili\|(\d+)/)
        const cid = cidMatch ? parseInt(cidMatch[1]) : undefined
        const result = await invoke<{ url: string; bandwidth: number; codecs: string }>('get_bili_audio_url', {
          bvid: isAvid ? '' : biliId,
          avid: isAvid ? parseInt(biliId) : null,
          cid: cid || null,
        })
        if (token !== playbackRequestToken) return
        if (useCrossfade) {
          dur = await invoke<number>('crossfade_url', {
            url: result.url, durationHintMs: track.durationMs,
            fadeOutMs: settings.crossfadeOutDuration, fadeInMs: settings.crossfadeInDuration,
          })
        } else {
          dur = await invoke<number>('play_url', { url: result.url, durationHintMs: track.durationMs })
        }
        if (token !== playbackRequestToken) return
        audioInfo.value = {
          bitrate: result.bandwidth > 0 ? Math.round(result.bandwidth / 1000) : undefined,
          codec: result.codecs || undefined,
        }
      } else if (track.id.startsWith('youtube:')) {
        // YouTube：获取音频流，优先使用预热缓存
        const videoId = track.id.replace('youtube:', '')
        let best: { url: string; bitrate: number; mime_type: string } | undefined

        const cached = prefetchedUrls.get(track.id)
        if (cached && Date.now() - cached.time < PREFETCH_TTL_MS) {
          best = cached
          prefetchedUrls.delete(track.id)
        } else {
          const streams = await invoke<{ url: string; bitrate: number; mime_type: string }[]>('get_youtube_audio_url', { videoId })
          if (token !== playbackRequestToken) return
          best = streams?.[0]
        }

        if (!best?.url) throw new Error('No YouTube audio stream')
        if (useCrossfade) {
          dur = await invoke<number>('crossfade_url', {
            url: best.url, durationHintMs: track.durationMs || 0,
            fadeOutMs: settings.crossfadeOutDuration, fadeInMs: settings.crossfadeInDuration,
          })
        } else {
          dur = await invoke<number>('play_url', { url: best.url, durationHintMs: track.durationMs || 0 })
        }
        if (token !== playbackRequestToken) return
        audioInfo.value = {
          bitrate: best.bitrate > 0 ? Math.round(best.bitrate / 1000) : undefined,
          codec: extractCodecFromMime(best.mime_type),
        }
      } else {
        // 本地文件
        if (useCrossfade) {
          dur = await invoke<number>('crossfade_file', {
            path: track.audioUrl,
            fadeOutMs: settings.crossfadeOutDuration, fadeInMs: settings.crossfadeInDuration,
          })
        } else {
          dur = await invoke<number>('play_file', { path: track.audioUrl })
        }
        if (token !== playbackRequestToken) return
      }

      durationMs.value = dur || track.durationMs
      isPlaying.value = true
      isLoadingAudio.value = false
      positionMs.value = 0

      // 重置插值状态
      _interpAnchorMs = 0
      _interpAnchorTime = performance.now()
      _interpRenderedMs = 0
      _interpSpeed = playbackSpeed.value
      _interpIsPlaying = true
      _interpDurationMs = durationMs.value
      interpolatedPositionMs.value = 0

      // 重置连续失败计数
      consecutivePlayFailures = 0
      // 记录 URL 解析时间（用于过期检测）
      lastUrlResolveTime = Date.now()
      // 标记已加载（取消 restore 重载标记）
      _needsReload = false

      // 记录播放历史
      const history = useHistoryStore()
      history.record(track)

      // 持久化状态 + 预热下一首
      savePlayerState()
      maybePrefetchNext()
    } catch (e) {
      if (token !== playbackRequestToken) return // 竞态过期请求，静默忽略

      const msg = e instanceof Error ? e.message : String(e)
      console.error('Play failed:', msg)
      playError.value = msg
      isPlaying.value = false
      isLoadingAudio.value = false

      const toast = useToastStore()
      toast.error((i18n.global as any).t('player.play_failed', { msg }))

      // 连续失败熔断 + 自动 skip
      consecutivePlayFailures++
      if (consecutivePlayFailures < MAX_CONSECUTIVE_FAILURES && queue.value.length > 1) {
        _isAutoSkipping = true
        try {
          await next(true)
        } finally {
          _isAutoSkipping = false
        }
      } else if (consecutivePlayFailures >= MAX_CONSECUTIVE_FAILURES) {
        toast.error((i18n.global as any).t('player.too_many_failures', '连续播放失败过多，已停止'))
      }
    }
  }

  async function togglePlayPause() {
    // 恢复后首次播放：需要重新加载曲目
    if (!isPlaying.value && currentTrack.value && _needsReload) {
      _needsReload = false
      const savedPos = positionMs.value
      await play(currentTrack.value)
      if (savedPos > 1000) {
        setTimeout(() => seekTo(savedPos), 300)
      }
      return
    }

    // 乐观更新：立即翻转 UI 状态，消除 IPC 延迟感
    const optimistic = !isPlaying.value
    isPlaying.value = optimistic
    _interpIsPlaying = optimistic
    if (optimistic) {
      _interpAnchorMs = positionMs.value
      _interpAnchorTime = performance.now()
      _interpRenderedMs = positionMs.value
      _interpSpeed = playbackSpeed.value
    }

    try {
      const playing = await invoke<boolean>('toggle_play_pause')
      // 后端确认：如果与乐观预测不一致则修正
      if (playing !== optimistic) {
        isPlaying.value = playing
        _interpIsPlaying = playing
      }
      // 清除 pending seek 标记（seekTo 已经 fire-and-forget 发送过了）
      if (playing) {
        lastSeekedMs = null
      }
    } catch {
      isPlaying.value = !isPlaying.value
    }
  }

  async function pause() {
    // 乐观更新
    isPlaying.value = false
    _interpIsPlaying = false
    try {
      const settings = useSettingsStore()
      if (settings.fadeIn && settings.fadeOutDuration > 0) {
        await invoke('pause_with_fade', { durationMs: settings.fadeOutDuration })
      } else {
        await invoke('pause')
      }
    } catch {}
    savePlayerState()
  }

  async function resume() {
    // URL 过期检测（10min）：在线来源 URL 过期后需重新解析
    const isOnlineSource = currentTrack.value && (
      currentTrack.value.id.startsWith('netease:')
      || currentTrack.value.id.startsWith('bilibili:')
      || currentTrack.value.id.startsWith('youtube:')
    )
    if (isOnlineSource && lastUrlResolveTime > 0
      && Date.now() - lastUrlResolveTime > URL_EXPIRY_MS) {
      // URL 已过期，重新解析
      await play(currentTrack.value!)
      return
    }

    // 乐观更新
    isPlaying.value = true
    _interpAnchorMs = positionMs.value
    _interpAnchorTime = performance.now()
    _interpRenderedMs = positionMs.value
    _interpIsPlaying = true
    _interpSpeed = playbackSpeed.value
    try {
      const settings = useSettingsStore()
      if (settings.fadeIn && settings.fadeInDuration > 0) {
        await invoke('resume_with_fade', { durationMs: settings.fadeInDuration })
      } else {
        await invoke('resume')
      }
    } catch {}
  }

  async function seekTo(ms: number) {
    const posMs = Math.round(ms)
    positionMs.value = posMs
    lastSeekedMs = posMs
    seekTargetMs = posMs
    seekGuardUntil = Date.now() + 1500

    // 立即重置插值到 seek 目标（乐观更新，用户立即看到跳转）
    _interpAnchorMs = posMs
    _interpAnchorTime = performance.now()
    _interpRenderedMs = posMs
    interpolatedPositionMs.value = posMs

    // Fire-and-forget：不阻塞 UI，后端异步执行 seek
    invoke('seek', { positionMs: posMs }).then(() => {
      positionMs.value = posMs
      seekGuardUntil = Date.now() + 800
    }).catch((e) => {
      console.error('Seek failed:', e)
    })
  }

  /**
   * 播放结束自动触发（对齐 Android handleTrackEnded）
   * - repeat_one: 重新播放当前
   * - repeat_all: next(force=true) 强制推进
   * - off: 还有下一首则推进，否则停止播放但保留队列
   */
  async function handleTrackEnded() {
    // Track End 去重：200ms ticker 可能重复触发
    const trackId = currentTrack.value?.id ?? null
    if (trackId && trackId === lastTrackEndedId && Date.now() - lastTrackEndedTime < 2000) {
      return
    }
    lastTrackEndedId = trackId
    lastTrackEndedTime = Date.now()

    // 睡眠定时器
    const isLast = !shuffleEnabled.value && queueIndex.value >= queue.value.length - 1
    if (sleepTimerMode.value === 'end_of_track') {
      await pause()
      cancelSleepTimer()
      return
    }
    if (sleepTimerMode.value === 'end_of_queue') {
      if (isLast && repeatMode.value !== 'all') {
        await pause()
        cancelSleepTimer()
        return
      }
    }

    if (repeatMode.value === 'one') {
      // 单曲循环：重新播放当前曲目
      if (currentTrack.value) {
        await play(currentTrack.value)
      }
    } else if (repeatMode.value === 'all') {
      // 列表循环：强制推进到下一首（到末尾回到开头）
      await next(true)
    } else {
      // 顺序播放：还有下一首则推进，否则停止
      if (shuffleEnabled.value || queueIndex.value < queue.value.length - 1) {
        await next(false)
      } else {
        // 停止播放但保留队列（对齐 Android stopPlaybackPreservingQueue）
        await pause()
        positionMs.value = 0
      }
    }
  }

  /**
   * 用户手动下一首（对齐 Android nextImpl）
   * - 不管 repeat_one，始终推进
   * - force=true 时列表末尾回绕
   * - Shuffle 模式使用三栈模型
   */
  async function next(force: boolean = false) {
    if (queue.value.length === 0) return
    // 用户手动操作重置失败计数（自动 skip 不重置）
    if (!_isAutoSkipping) consecutivePlayFailures = 0

    let nextIdx: number
    if (shuffleEnabled.value) {
      // ─── Shuffle 三栈模型 ───
      if (shuffleFuture.length > 0) {
        // 优先从 future 栈弹出（previous 回退过的）
        shuffleHistory.push(queueIndex.value)
        nextIdx = shuffleFuture.pop()!
      } else if (shuffleBag.length > 0) {
        // 从未播放池随机取
        shuffleHistory.push(queueIndex.value)
        const bagIdx = Math.floor(Math.random() * shuffleBag.length)
        nextIdx = shuffleBag[bagIdx]
        shuffleBag.splice(bagIdx, 1)
      } else {
        // bag 已空
        if (force || repeatMode.value === 'all') {
          rebuildShuffleBag()
          if (shuffleBag.length > 0) {
            shuffleHistory.push(queueIndex.value)
            const bagIdx = Math.floor(Math.random() * shuffleBag.length)
            nextIdx = shuffleBag[bagIdx]
            shuffleBag.splice(bagIdx, 1)
          } else {
            return
          }
        } else {
          return // 顺序播放结束
        }
      }
    } else {
      if (queueIndex.value < queue.value.length - 1) {
        nextIdx = queueIndex.value + 1
      } else {
        if (force || repeatMode.value === 'all') {
          nextIdx = 0
        } else {
          // 已在末尾，不动
          return
        }
      }
    }
    await play(queue.value[nextIdx])
  }

  /**
   * 用户手动上一首（对齐 Android previousImpl）
   * - 播放超过 3 秒则回到开头
   * - Shuffle 模式使用 history 栈回溯
   * - 非 shuffle：只有 repeat_all 才回绕到末尾
   */
  async function previous() {
    consecutivePlayFailures = 0
    if (queue.value.length === 0) return

    // 播放超过 3 秒则回到开头
    if (positionMs.value > 3000) {
      seekTo(0)
      return
    }

    if (shuffleEnabled.value) {
      // Shuffle：从 history 栈回溯
      if (shuffleHistory.length > 0) {
        shuffleFuture.push(queueIndex.value)
        const prevIdx = shuffleHistory.pop()!
        await play(queue.value[prevIdx])
      } else {
        seekTo(0) // 无历史，重新开始当前曲目
      }
      return
    }

    // 非 shuffle 模式
    if (queueIndex.value > 0) {
      await play(queue.value[queueIndex.value - 1])
    } else if (repeatMode.value === 'all') {
      await play(queue.value[queue.value.length - 1])
    }
    // else: 已在开头且非列表循环，不动
  }

  async function toggleRepeatMode() {
    try {
      const mode = await invoke<string>('cycle_repeat')
      repeatMode.value = mode as RepeatMode
    } catch {
      const modes: RepeatMode[] = ['off', 'all', 'one']
      const idx = modes.indexOf(repeatMode.value)
      repeatMode.value = modes[(idx + 1) % modes.length]
    }
    savePlayerState()
  }

  async function toggleShuffle() {
    try {
      const enabled = await invoke<boolean>('toggle_shuffle')
      shuffleEnabled.value = enabled
    } catch {
      shuffleEnabled.value = !shuffleEnabled.value
    }
    // Shuffle 三栈管理
    if (shuffleEnabled.value) {
      rebuildShuffleBag()
      shuffleHistory = []
      shuffleFuture = []
    } else {
      shuffleBag = []
      shuffleHistory = []
      shuffleFuture = []
    }
    savePlayerState()
  }

  /**
   * 统一播放模式循环切换：顺序播放 -> 列表循环 -> 单曲循环 -> 随机播放 -> 顺序播放
   * 合并 repeat + shuffle 为一个按钮的逻辑
   */
  type PlayMode = 'sequential' | 'repeat_all' | 'repeat_one' | 'shuffle'

  const playMode = computed<PlayMode>(() => {
    if (shuffleEnabled.value) return 'shuffle'
    if (repeatMode.value === 'all') return 'repeat_all'
    if (repeatMode.value === 'one') return 'repeat_one'
    return 'sequential'
  })

  async function cyclePlayMode() {
    const current = playMode.value
    switch (current) {
      case 'sequential':
        // -> 列表循环
        if (shuffleEnabled.value) await toggleShuffle()
        repeatMode.value = 'all'
        try { await invoke<string>('cycle_repeat') } catch {}
        break
      case 'repeat_all':
        // -> 单曲循环
        repeatMode.value = 'one'
        try { await invoke<string>('cycle_repeat') } catch {}
        break
      case 'repeat_one':
        // -> 随机播放
        repeatMode.value = 'off'
        try { await invoke<string>('cycle_repeat') } catch {}
        if (!shuffleEnabled.value) await toggleShuffle()
        break
      case 'shuffle':
        // -> 顺序播放
        if (shuffleEnabled.value) await toggleShuffle()
        repeatMode.value = 'off'
        break
    }
    savePlayerState()
  }

  async function setVolume(vol: number) {
    volume.value = vol
    try { await invoke('set_volume', { level: vol }) } catch {}
    savePlayerState()
  }

  // 播放速度
  const playbackSpeed = ref(1.0)
  async function setSpeed(spd: number) {
    playbackSpeed.value = spd
    _interpSpeed = spd
    // 重新锚定以反映速度变化
    if (_interpIsPlaying) {
      _interpAnchorMs = interpolatedPositionMs.value
      _interpAnchorTime = performance.now()
      _interpRenderedMs = interpolatedPositionMs.value
    }
    try { await invoke('set_speed', { speed: spd }) } catch {}
  }

  // ─── 音效参数（响度增益 + 均衡器） ───
  const loudnessGainMb = ref(0)
  const equalizerEnabled = ref(false)
  const equalizerPresetId = ref('flat')
  const equalizerBands = ref([0, 0, 0, 0, 0]) // 5 bands, mB values

  /** 是否有任何非默认音效 */
  const hasActiveEffects = computed(() =>
    playbackSpeed.value !== 1.0
    || loudnessGainMb.value !== 0
    || equalizerEnabled.value
  )

  async function setLoudnessGain(mb: number) {
    loudnessGainMb.value = Math.round(Math.max(0, Math.min(1500, mb)))
    try { await invoke('set_loudness_gain', { gainMb: loudnessGainMb.value }) } catch {}
  }

  async function setEqualizer(enabled: boolean, bands: number[]) {
    equalizerEnabled.value = enabled
    equalizerBands.value = bands.map(v => Math.round(Math.max(-1500, Math.min(1500, v))))
    try { await invoke('set_equalizer', { enabled, bandLevelsMb: equalizerBands.value }) } catch {}
  }

  async function setEqualizerPreset(presetId: string) {
    equalizerPresetId.value = presetId
    const bands = EQ_PRESETS[presetId] || [0, 0, 0, 0, 0]
    await setEqualizer(presetId !== 'flat', [...bands])
  }

  async function resetAudioEffects() {
    loudnessGainMb.value = 0
    equalizerEnabled.value = false
    equalizerPresetId.value = 'flat'
    equalizerBands.value = [0, 0, 0, 0, 0]
    playbackSpeed.value = 1.0
    _interpSpeed = 1.0
    try {
      await invoke('reset_audio_effects')
      await invoke('set_speed', { speed: 1.0 })
    } catch {}
  }

  // 批量替换队列并播放第一首
  function playAll(tracks: TrackInfo[]) {
    if (tracks.length === 0) return
    queue.value = [...tracks]
    queueIndex.value = 0
    shuffleBag = []
    shuffleHistory = []
    shuffleFuture = []
    play(tracks[0])
  }

  // 洗牌后替换队列并播放
  function shufflePlay(tracks: TrackInfo[]) {
    if (tracks.length === 0) return
    const shuffled = [...tracks]
    for (let i = shuffled.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [shuffled[i], shuffled[j]] = [shuffled[j], shuffled[i]]
    }
    queue.value = shuffled
    queueIndex.value = 0
    shuffleBag = []
    shuffleHistory = []
    shuffleFuture = []
    play(shuffled[0])
  }

  // 插入到当前曲目之后
  function addToQueueNext(track: TrackInfo) {
    const existing = queue.value.findIndex(t => t.id === track.id)
    if (existing !== -1) {
      if (shuffleEnabled.value) shiftShuffleIndicesForRemove(existing)
      queue.value.splice(existing, 1)
      if (existing < queueIndex.value) queueIndex.value--
    }
    const idx = queueIndex.value + 1
    queue.value.splice(idx, 0, track)
    if (shuffleEnabled.value) shiftShuffleIndicesForInsert(idx)
    savePlayerState()
  }

  // 追加到队列末尾
  function addToQueueEnd(track: TrackInfo) {
    if (!queue.value.find(t => t.id === track.id)) {
      queue.value.push(track)
      if (shuffleEnabled.value) {
        shuffleBag.push(queue.value.length - 1)
      }
    }
    savePlayerState()
  }

  // 从队列移除指定索引
  function removeFromQueue(index: number) {
    if (index < 0 || index >= queue.value.length) return
    const wasCurrentTrack = index === queueIndex.value

    if (shuffleEnabled.value) shiftShuffleIndicesForRemove(index)

    queue.value.splice(index, 1)
    if (queue.value.length === 0) {
      queueIndex.value = -1
      currentTrack.value = null
      shuffleBag = []
      shuffleHistory = []
      shuffleFuture = []
      pause()
      savePlayerState()
      return
    }
    if (index < queueIndex.value) {
      queueIndex.value--
    } else if (wasCurrentTrack) {
      // 被删除的是当前曲目，索引保持（指向下一首），但不超界
      queueIndex.value = Math.min(queueIndex.value, queue.value.length - 1)
      // 同步 currentTrack 到新索引指向的曲目
      currentTrack.value = queue.value[queueIndex.value]
    }
    savePlayerState()
  }

  // 清空队列
  function clearQueue() {
    queue.value = []
    queueIndex.value = -1
    shuffleBag = []
    shuffleHistory = []
    shuffleFuture = []
    savePlayerState()
  }

  // 编辑当前曲目信息（仅前端状态，不持久化）
  let originalTrackInfo: TrackInfo | null = null

  function updateCurrentTrackInfo(patch: Partial<TrackInfo>) {
    if (!currentTrack.value) return
    if (!originalTrackInfo) {
      originalTrackInfo = { ...currentTrack.value }
    }
    currentTrack.value = { ...currentTrack.value, ...patch }
  }

  function restoreOriginalTrackInfo() {
    if (originalTrackInfo && currentTrack.value) {
      currentTrack.value = { ...originalTrackInfo }
      originalTrackInfo = null
    }
  }

  function hasOriginalTrackInfo() {
    return originalTrackInfo !== null
  }

  // 用指定音质重新播放当前曲目（保持进度）
  async function replayWithQuality() {
    const track = currentTrack.value
    if (!track) return
    const pos = positionMs.value
    const wasPlaying = isPlaying.value
    await play(track)
    if (pos > 1000) {
      // 等一小段让播放开始后再 seek
      setTimeout(() => seekTo(pos), 300)
    }
  }

  // ─── 初始化：恢复持久化状态 ───
  loadPlayerState()
  // 同步恢复的音量到后端
  if (volume.value !== 1) {
    invoke('set_volume', { level: volume.value }).catch(() => {})
  }

  return {
    isPlaying, currentTrack, positionMs, durationMs, queue, queueIndex,
    repeatMode, shuffleEnabled, volume, lyrics, playError, isLoadingAudio,
    audioLevel, beatImpulse, audioInfo,
    playbackSpeed, sleepTimerMode, sleepRemainingSeconds,
    loudnessGainMb, equalizerEnabled, equalizerPresetId, equalizerBands, hasActiveEffects,
    progress, interpolatedPositionMs, interpolatedProgress,
    currentTimeFormatted, durationFormatted,
    play, togglePlayPause, pause, resume, seekTo, next, previous,
    toggleRepeatMode, toggleShuffle, cyclePlayMode, playMode, setVolume, setSpeed,
    setLoudnessGain, setEqualizer, setEqualizerPreset, resetAudioEffects,
    startSleepTimer, startSleepTimerEndOfTrack, startSleepTimerEndOfQueue, cancelSleepTimer,
    playAll, shufflePlay, addToQueueNext, addToQueueEnd, removeFromQueue, clearQueue,
    updateCurrentTrackInfo, restoreOriginalTrackInfo, hasOriginalTrackInfo, replayWithQuality,
  }
})

function formatTime(ms: number): string {
  const totalSeconds = Math.floor(Math.max(0, ms) / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${minutes}:${seconds.toString().padStart(2, '0')}`
}

/** 从 MIME type 提取编解码器名称，例如 'audio/webm; codecs="opus"' -> 'Opus' */
function extractCodecFromMime(mime: string): string | undefined {
  // 尝试从 codecs 参数提取
  const codecsMatch = mime.match(/codecs="?([^";\s]+)"?/)
  if (codecsMatch) {
    const raw = codecsMatch[1].split('.')[0] // 去除 profile，如 mp4a.40.2 -> mp4a
    const codecMap: Record<string, string> = {
      opus: 'Opus', vorbis: 'Vorbis', mp4a: 'AAC', flac: 'FLAC', mp3: 'MP3',
    }
    return codecMap[raw.toLowerCase()] ?? raw.toUpperCase()
  }
  // 从 MIME 主类型推断
  const typeMatch = mime.match(/^audio\/(\w+)/)
  if (typeMatch) {
    const codecMap: Record<string, string> = {
      webm: 'WebM', mp4: 'AAC', mpeg: 'MP3', ogg: 'Vorbis', flac: 'FLAC', wav: 'WAV',
    }
    return codecMap[typeMatch[1].toLowerCase()] ?? typeMatch[1].toUpperCase()
  }
  return undefined
}
