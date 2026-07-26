/**
 * 一起听 Pinia Store
 * 房间管理、WebSocket 通信、播放同步
 */
import { defineStore } from 'pinia'
import { ref, computed, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager'
import { usePlayerStore } from '@/stores/player'
import { useSettingsStore } from '@/stores/settings'
import { useToastStore } from '@/stores/toast'
import i18n from '@/i18n'
import type {
  ConnectionState,
  LtRole,
  ListenTogetherRoomState,
  ListenTogetherRoomSettings,
  ListenTogetherSocketEnvelope,
  ListenTogetherEvent,
  ListenTogetherInitialSnapshot,
} from './protocol'
import { desktopRepeatToWire } from './protocol'
import { trackInfoToLtTrack, ltTrackToTrackInfo, toShareableQueueSnapshot } from './mapper'
import { createLogger } from '@/utils/logger'

const log = createLogger('listen-together')

const LT_UUID_KEY = 'neri:lt-uuid'
const DEFAULT_BASE_URL = 'https://neriplayer.hancat.work'

// 进度纠偏阈值
const DRIFT_SOFT_MS = 800
const DRIFT_FORCE_MS = 2500
const CONTROL_EVENT_DEDUP_MS = 350
const SEEK_EVENT_DEDUP_MS = 800
const SEEK_EVENT_MIN_DELTA_MS = 300
const LOCAL_SEEK_REPORT_DEBOUNCE_MS = 450

// 心跳间隔
const HEARTBEAT_INTERVAL_MS = 10_000
// listener 侧存活探测间隔（房主走 HEARTBEAT，听众用 ping 保活半开连接检测）
const LISTENER_PING_INTERVAL_MS = 25_000
// 已处理转发请求 eventId 上限（对齐 Android ForwardedRequestDeduper 语义）
const HANDLED_FORWARDED_EVENT_LIMIT = 100

// 重连配置
const RECONNECT_DELAYS = [1000, 2000, 4000, 8000, 16000, 30000]

export const useListenTogetherStore = defineStore('listenTogether', () => {
  const settings = useSettingsStore()

  // 状态
  const connectionState = ref<ConnectionState>('disconnected')
  const roomId = ref<string | null>(null)
  const userUuid = ref(loadOrCreateUuid())
  const role = ref<LtRole | null>(null)
  const roomState = ref<ListenTogetherRoomState | null>(null)
  const sessionError = ref<string | null>(null)
  const lastSyncEventType = ref<string | null>(null)
  const lastSyncAt = ref<number | null>(null)
  const lastReconnectAt = ref<number | null>(null)

  // 从 settings store 读取（双向绑定）
  const baseUrl = computed({
    get: () => settings.ltServerUrl || DEFAULT_BASE_URL,
    set: (v: string) => { settings.ltServerUrl = v },
  })
  const nickname = computed({
    // 默认名从持久化 UUID 派生, 稳定不随机; 仅作展示回退, 不写入 settings
    // 注意: Android 昵称白名单不含连字符, 用户手动保存含 '-' 的昵称经云同步到
    // Android 端会被 sanitize 丢弃 (ListenTogetherPreferences.restore), 回退值不落盘无此问题
    get: () => settings.ltNickname
      || `NERI-PC-${userUuid.value.replace(/-/g, '').slice(0, 4).toUpperCase()}`,
    set: (v: string) => { settings.ltNickname = v },
  })
  const roomSettings = computed({
    get: () => ({
      allowMemberControl: settings.ltAllowMemberControl,
      autoPauseOnMemberChange: settings.ltAutoPauseOnMemberChange,
      shareAudioLinks: settings.ltShareAudioLinks,
    }),
    set: (v: ListenTogetherRoomSettings) => {
      settings.ltAllowMemberControl = v.allowMemberControl
      settings.ltAutoPauseOnMemberChange = v.autoPauseOnMemberChange
      settings.ltShareAudioLinks = v.shareAudioLinks
    },
  })

  // 内部状态
  let _heartbeatTimer: ReturnType<typeof setInterval> | null = null
  let _listenerPingTimer: ReturnType<typeof setInterval> | null = null
  let _reconnectAttempt = 0
  let _reconnectTimer: ReturnType<typeof setTimeout> | null = null
  // 会话代际：leaveRoom 后递增，让在途的延迟回调失效，不再操作播放器
  let _sessionGeneration = 0
  // 出站事件排序字段：实例标识会话内生成一次，序号单调递增（对齐 Android EventFactory）
  const _clientInstanceId = crypto.randomUUID()
  let _clientSequence = 0
  // 已处理的转发请求 eventId -> 处理时间，防重复送达二次执行
  const _handledForwardedEventIds = new Map<string, number>()
  let _wsUrl: string | null = null
  let _unlistenMessage: UnlistenFn | null = null
  let _unlistenConnected: UnlistenFn | null = null
  let _unlistenDisconnected: UnlistenFn | null = null
  let _suppressPlayerWatch = false
  let _lastAppliedRoomVersion = 0
  // 回环抑制：避免自己发出的事件触发回环
  const _recentOutboundEventIds = new Set<string>()
  // 记录最后上报的 track id，避免重复上报
  let _lastReportedTrackId: string | null = null
  let _lastReportedIsPlaying: boolean | null = null
  let _lastReportedRepeatMode: number | null = null
  let _lastReportedShuffle: boolean | null = null
  let _lastSentControlType: string | null = null
  let _lastSentControlAt = 0
  let _lastSentSeekPosition: number | null = null
  let _lastSentSeekAt = 0
  let _pendingSeekReport: { positionMs: number; trackId: string | null } | null = null
  let _pendingSeekTimer: ReturnType<typeof setTimeout> | null = null

  // 计算属性
  const isConnected = computed(() => connectionState.value === 'connected')
  const isController = computed(() => role.value === 'controller')
  const members = computed(() => roomState.value?.members ?? [])

  // 房间操作
  /** 创建房间 */
  async function createRoom() {
    const player = usePlayerStore()
    const toast = useToastStore()
    const t = (i18n.global as any).t

    try {
      sessionError.value = null
      connectionState.value = 'connecting'

      // 构建初始快照
      const { queue: ltQueue, resolvedIndex } = toShareableQueueSnapshot(
        player.queue,
        player.queueIndex,
        roomSettings.value.shareAudioLinks,
      )

      const snapshot: ListenTogetherInitialSnapshot = {
        queue: ltQueue,
        currentIndex: resolvedIndex,
        track: player.currentTrack ? trackInfoToLtTrack(player.currentTrack) : undefined,
        settings: roomSettings.value,
        isPlaying: player.isPlaying,
        positionMs: player.positionMs,
        // Align Android ListenTogetherInitialSnapshot (ExoPlayer ints)
        repeatMode: desktopRepeatToWire(player.repeatMode),
        shuffleEnabled: !!player.shuffleEnabled,
      }

      const resp = await invoke<any>('lt_create_room', {
        baseUrl: baseUrl.value,
        userUuid: userUuid.value,
        nickname: nickname.value,
        initialSnapshot: snapshot,
      })

      if (!resp.ok) {
        throw new Error(resp.error || 'Create room failed')
      }

      roomId.value = resp.roomId
      role.value = (resp.role as LtRole) || 'controller'
      if (resp.state) {
        roomState.value = resp.state
        _lastAppliedRoomVersion = resp.state.version || 0
        markSync('INITIAL_STATE', resp.state.updatedAt || Date.now())
      }

      // 连接 WebSocket
      const wsUrl = resp.wsUrl || buildWsUrl(baseUrl.value, resp.roomId, resp.token)
      await connectWs(wsUrl)

      startHeartbeat()
      setupPlayerWatch()

    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      sessionError.value = msg
      connectionState.value = 'disconnected'
      toast.error(t('listen_together.create_failed', { msg }))
    }
  }

  /** 加入房间 */
  async function joinRoom(targetRoomId: string) {
    const player = usePlayerStore()
    const toast = useToastStore()
    const t = (i18n.global as any).t

    try {
      sessionError.value = null
      connectionState.value = 'connecting'

      const resp = await invoke<any>('lt_join_room', {
        baseUrl: baseUrl.value,
        roomId: targetRoomId,
        userUuid: userUuid.value,
        nickname: nickname.value,
      })

      if (!resp.ok) {
        throw new Error(resp.error || 'Join room failed')
      }

      roomId.value = targetRoomId
      role.value = (resp.role as LtRole) || 'listener'
      if (resp.state) {
        roomState.value = resp.state
        _lastAppliedRoomVersion = resp.state.version || 0
        roomSettings.value = resp.state.settings || roomSettings.value
        markSync('INITIAL_STATE', resp.state.updatedAt || Date.now())
        // 将服务端状态应用到本地播放器
        applyRoomStateToPlayer(resp.state, 'join', resp.state.playback?.basePositionMs)
      }

      const wsUrl = resp.wsUrl || buildWsUrl(baseUrl.value, targetRoomId, resp.token)
      await connectWs(wsUrl)

      startListenerPing()
      setupPlayerWatch()

    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      sessionError.value = msg
      connectionState.value = 'disconnected'
      toast.error(t('listen_together.join_failed', { msg }))
    }
  }

  /** 离开房间 */
  async function leaveRoom() {
    // 递增会话代际，让在途的延迟同步回调失效
    _sessionGeneration++
    stopHeartbeat()
    stopListenerPing()
    teardownPlayerWatch()
    teardownListeners()
    if (_reconnectTimer) {
      clearTimeout(_reconnectTimer)
      _reconnectTimer = null
    }

    try {
      await invoke('lt_disconnect_ws')
    } catch {}

    roomId.value = null
    role.value = null
    roomState.value = null
    _lastAppliedRoomVersion = 0
    sessionError.value = null
    connectionState.value = 'disconnected'
    lastSyncEventType.value = null
    lastSyncAt.value = null
    lastReconnectAt.value = null
    _wsUrl = null
    _reconnectAttempt = 0
    _lastReportedTrackId = null
    _lastReportedIsPlaying = null
    _lastReportedRepeatMode = null
    _lastReportedShuffle = null
    _lastSentControlType = null
    _lastSentControlAt = 0
    _lastSentSeekPosition = null
    _lastSentSeekAt = 0
    clearPendingSeekReport()
    _recentOutboundEventIds.clear()
    _handledForwardedEventIds.clear()
  }

  // WebSocket 连接
  async function connectWs(wsUrl: string) {
    _wsUrl = wsUrl
    await setupListeners()
    await invoke('lt_connect_ws', { wsUrl })
  }

  async function setupListeners() {
    teardownListeners()

    _unlistenMessage = await listen<ListenTogetherSocketEnvelope>('lt:message', (event) => {
      handleSocketMessage(event.payload)
    })

    _unlistenConnected = await listen('lt:connected', () => {
      const reconnected = _reconnectAttempt > 0
      connectionState.value = 'connected'
      if (reconnected) {
        lastReconnectAt.value = Date.now()
        markSync('RECONNECTED', lastReconnectAt.value)
      }
      _reconnectAttempt = 0
      // 重连后恢复 listener 存活探测
      if (roomId.value && role.value === 'listener') startListenerPing()
    })

    _unlistenDisconnected = await listen<{ code: number; reason: string }>('lt:disconnected', (event) => {
      const wasConnected = connectionState.value === 'connected'
      connectionState.value = 'disconnected'
      stopListenerPing()

      // 是否需要重连
      if (wasConnected && roomId.value) {
        scheduleReconnect()
      }
    })
  }

  function teardownListeners() {
    _unlistenMessage?.()
    _unlistenConnected?.()
    _unlistenDisconnected?.()
    _unlistenMessage = null
    _unlistenConnected = null
    _unlistenDisconnected = null
  }

  // 消息处理
  function handleSocketMessage(envelope: ListenTogetherSocketEnvelope) {
    switch (envelope.type) {
      case 'welcome':
        handleWelcome(envelope)
        break
      case 'room_state_updated':
        handleRoomStateUpdated(envelope)
        break
      case 'link_requested':
        handleLinkRequested(envelope)
        break
      case 'member_control_requested':
        handleMemberControlRequested(envelope)
        break
      case 'room_suspended':
        handleRoomSuspended(envelope)
        break
      case 'room_resumed':
        handleRoomResumed(envelope)
        break
      case 'room_closed':
        handleRoomClosed(envelope)
        break
      case 'pong':
        // 心跳回复，忽略
        break
      case 'event_applied':
        handleEventApplied(envelope)
        break
      default:
        log.debug('unknown message type:', envelope.type)
    }
  }

  /// 控制命令应答
  ///
  /// 被拒绝（无权限、房间已关闭等）时必须回滚到服务端状态并告知用户，
  /// 否则本地乐观改动会留下来，两端就此分叉。
  function handleEventApplied(envelope: ListenTogetherSocketEnvelope) {
    const result = envelope.result
    if (!result || result.ok !== false) return

    log.warn('control rejected by server:', result.error)
    const t = (i18n.global as any).t
    useToastStore().error(result.error || t('listen_together.control_rejected'))
    // 以服务端状态为准重新对齐，不保留本地乐观改动
    if (envelope.state) {
      roomState.value = envelope.state
      _lastAppliedRoomVersion = envelope.state.version || 0
      roomSettings.value = envelope.state.settings || roomSettings.value
      markSync('EVENT_REJECTED', envelope.state.updatedAt || Date.now())
    }
  }

  function handleWelcome(envelope: ListenTogetherSocketEnvelope) {
    if (envelope.role) {
      role.value = envelope.role as LtRole
    }
    if (envelope.state) {
      roomState.value = envelope.state
      _lastAppliedRoomVersion = envelope.state.version || 0
      roomSettings.value = envelope.state.settings || roomSettings.value
      markSync('WELCOME', envelope.state.updatedAt || Date.now())
    }
  }

  function handleRoomStateUpdated(envelope: ListenTogetherSocketEnvelope) {
    if (!envelope.state) return
    if ((envelope.state.version || 0) < _lastAppliedRoomVersion) return

    // Echo suppression
    if (envelope.causedBy?.eventId && _recentOutboundEventIds.has(envelope.causedBy.eventId)) {
      _recentOutboundEventIds.delete(envelope.causedBy.eventId)
      // 仍然更新 roomState 但不应用到 player
      roomState.value = envelope.state
      _lastAppliedRoomVersion = envelope.state.version || 0
      roomSettings.value = envelope.state.settings || roomSettings.value
      markSync(envelope.causedBy?.type || 'STATE_SYNC', envelope.state.updatedAt || Date.now())
      return
    }

    roomState.value = envelope.state
    _lastAppliedRoomVersion = envelope.state.version || 0
    roomSettings.value = envelope.state.settings || roomSettings.value
    markSync(envelope.causedBy?.type || 'STATE_SYNC', envelope.state.updatedAt || Date.now())

    applyRoomStateToPlayer(
      envelope.state,
      envelope.causedBy?.type || 'state_update',
      envelope.expectedPositionMs,
    )
  }

  function handleLinkRequested(envelope: ListenTogetherSocketEnvelope) {
    // 房主收到链接请求：下发当前已解析的音频 URL
    if (!isController.value) return

    // 检查 requestTrackStableKey 是否与当前曲目匹配
    const player = usePlayerStore()
    if (!player.currentTrack) return

    const currentLt = trackInfoToLtTrack(player.currentTrack)
    if (envelope.requestTrackStableKey && envelope.requestTrackStableKey !== currentLt.stableKey) {
      return // 请求的曲目已不是当前播放的
    }

    // 构建 LINK_READY 事件（此处 streamUrl 在实际实现中需要从播放缓存获取）
    // Desktop 端暂无直接获取已解析 URL 的接口，跳过链接下发
    // 听众会自行解析
  }

  function handleMemberControlRequested(envelope: ListenTogetherSocketEnvelope) {
    // 房主处理听众的控制请求
    if (!isController.value) return

    // 鉴权：关闭成员控制时，只放行房主自己发出的请求
    // （对齐 Android ListenTogetherControlBlockPolicy，防魔改客户端越权控制）
    if (!roomSettings.value.allowMemberControl && envelope.causedBy?.userUuid !== userUuid.value) {
      log.warn('member control blocked: allowMemberControl=false, requester=', envelope.causedBy?.userUuid)
      return
    }

    // 按 causedBy.eventId 去重：重复送达的转发请求不得二次执行
    const causeEventId = envelope.causedBy?.eventId
    if (causeEventId) {
      if (_handledForwardedEventIds.has(causeEventId)) return
      _handledForwardedEventIds.set(causeEventId, Date.now())
      while (_handledForwardedEventIds.size > HANDLED_FORWARDED_EVENT_LIMIT) {
        const oldest = _handledForwardedEventIds.keys().next().value
        if (oldest === undefined) break
        _handledForwardedEventIds.delete(oldest)
      }
    }

    const player = usePlayerStore()
    const causeType = envelope.causedBy?.type

    _suppressPlayerWatch = true
    try {
      switch (causeType) {
        case 'REQUEST_PLAY':
          if (!player.isPlaying) player.togglePlayPause('local')
          reportPlayEvent()
          break
        case 'REQUEST_PAUSE':
          if (player.isPlaying) player.togglePlayPause('local')
          reportPauseEvent()
          break
        case 'REQUEST_SEEK':
          if (envelope.positionMs != null) {
            player.seekTo(envelope.positionMs, 'local')
            reportSeekEvent(envelope.positionMs)
          }
          break
        case 'REQUEST_SET_TRACK':
          if (envelope.track) {
            const trackInfo = ltTrackToTrackInfo(envelope.track)
            player.play(trackInfo, 'local')
            reportSetTrackEvent(envelope.track, envelope.currentIndex ?? 0)
          }
          break
        case 'REQUEST_PLAYBACK_MODE': {
          // Align Android: controller commits PLAYBACK_MODE for member request
          const repeatMode = envelope.repeatMode
            ?? envelope.state?.playback?.repeatMode
            ?? undefined
          const shuffleEnabled = envelope.shuffleEnabled
            ?? envelope.state?.playback?.shuffleEnabled
            ?? undefined
          player.applyListenTogetherPlaybackMode({
            repeatMode: typeof repeatMode === 'number' ? repeatMode : null,
            shuffleEnabled: typeof shuffleEnabled === 'boolean' ? shuffleEnabled : null,
          })
          reportPlaybackModeEvent()
          break
        }
      }
    } finally {
      setTimeout(() => { _suppressPlayerWatch = false }, 350)
    }
  }

  function handleRoomSuspended(_envelope: ListenTogetherSocketEnvelope) {
    const toast = useToastStore()
    const t = (i18n.global as any).t
    markSync('ROOM_SUSPENDED')
    toast.error(t('listen_together.controller_offline'))
  }

  function handleRoomResumed(_envelope: ListenTogetherSocketEnvelope) {
    markSync('ROOM_RESUMED')
  }

  function handleRoomClosed(_envelope: ListenTogetherSocketEnvelope) {
    const toast = useToastStore()
    const t = (i18n.global as any).t
    toast.error(t('listen_together.room_closed'))
    leaveRoom()
  }

  // 播放器同步
  function applyRoomStateToPlayer(
    state: ListenTogetherRoomState,
    causeType: string,
    expectedPositionMs?: number,
  ) {
    const player = usePlayerStore()
    if (!state.track) return

    _suppressPlayerWatch = true

    try {
      const remoteTrack = ltTrackToTrackInfo(state.track)
      const remoteIsPlaying = state.playback.state === 'playing'

      // 计算期望位置
      let expectedPos = expectedPositionMs ?? state.playback.basePositionMs
      if (remoteIsPlaying && state.playback.baseTimestampMs > 0) {
        const elapsed = Date.now() - state.playback.baseTimestampMs
        expectedPos = state.playback.basePositionMs + Math.max(0, elapsed) * state.playback.playbackRate
      }

      // 对比当前曲目
      const currentId = player.currentTrack?.id
      if (currentId !== remoteTrack.id) {
        // 需要切歌时，同时更新队列
        if (state.queue.length > 0) {
          const newQueue = state.queue.map(ltTrackToTrackInfo)
          player.queue.splice(0, player.queue.length, ...newQueue)
          const newIndex = Math.max(0, Math.min(state.currentIndex, newQueue.length - 1))
          player.queueIndex = newIndex
        }
        // 使用 remote_sync source 播放
        player.play(remoteTrack, 'remote_sync')
        // 播放后对齐进度与播放态；带会话代际 guard，退出房间后不得再操作播放器
        const generation = _sessionGeneration
        setTimeout(() => {
          if (generation !== _sessionGeneration) return
          if (expectedPos > 1000) {
            player.seekTo(expectedPos, 'remote_sync')
          }
          if (!remoteIsPlaying) {
            player.pause('remote_sync')
          }
        }, 300)
        _lastReportedTrackId = remoteTrack.id
        _lastReportedIsPlaying = remoteIsPlaying
        player.applyListenTogetherPlaybackMode({
          repeatMode: state.playback.repeatMode,
          shuffleEnabled: state.playback.shuffleEnabled,
        })
        _lastReportedRepeatMode = typeof state.playback.repeatMode === 'number'
          ? state.playback.repeatMode
          : desktopRepeatToWire(player.repeatMode)
        _lastReportedShuffle = typeof state.playback.shuffleEnabled === 'boolean'
          ? state.playback.shuffleEnabled
          : !!player.shuffleEnabled
        return
      }

      // 对比播放状态
      if (player.isPlaying !== remoteIsPlaying) {
        if (remoteIsPlaying) {
          player.resume('remote_sync')
        } else {
          player.pause('remote_sync')
        }
        _lastReportedIsPlaying = remoteIsPlaying
      }

      // 进度纠偏
      const diff = Math.abs(player.positionMs - expectedPos)
      if (diff > DRIFT_SOFT_MS) {
        player.seekTo(expectedPos, 'remote_sync')
      }

      // Align Android applyListenTogetherPlaybackMode
      player.applyListenTogetherPlaybackMode({
        repeatMode: state.playback.repeatMode,
        shuffleEnabled: state.playback.shuffleEnabled,
      })
      _lastReportedRepeatMode = typeof state.playback.repeatMode === 'number'
        ? state.playback.repeatMode
        : desktopRepeatToWire(player.repeatMode)
      _lastReportedShuffle = typeof state.playback.shuffleEnabled === 'boolean'
        ? state.playback.shuffleEnabled
        : !!player.shuffleEnabled
    } finally {
      // 延迟恢复 watch，避免同步操作触发上报
      setTimeout(() => { _suppressPlayerWatch = false }, 500)
    }
  }

  // 本地变化上报
  let _playerWatchStop: (() => void) | null = null
  let _seekWatchStop: (() => void) | null = null

  function setupPlayerWatch() {
    teardownPlayerWatch()
    const player = usePlayerStore()

    _playerWatchStop = watch(
      () => ({
        trackId: player.currentTrack?.id,
        isPlaying: player.isPlaying,
        repeatMode: player.repeatMode,
        shuffleEnabled: player.shuffleEnabled,
      }),
      (newVal) => {
        if (_suppressPlayerWatch || player.isRemoteSyncGuardActive() || connectionState.value !== 'connected') return

        // 曲目变化
        if (newVal.trackId && newVal.trackId !== _lastReportedTrackId) {
          _lastReportedTrackId = newVal.trackId
          if (player.currentTrack) {
            const ltTrack = trackInfoToLtTrack(player.currentTrack)
            if (isController.value) {
              reportSetTrackEvent(ltTrack, player.queueIndex)
            } else {
              sendRequestEvent('REQUEST_SET_TRACK', { track: ltTrack, currentIndex: player.queueIndex })
            }
          }
        }

        // 播放状态变化
        if (newVal.isPlaying !== _lastReportedIsPlaying) {
          _lastReportedIsPlaying = newVal.isPlaying
          if (newVal.isPlaying) {
            if (isController.value) reportPlayEvent()
            else if (!shouldSkipControlEvent('REQUEST_PLAY')) sendRequestEvent('REQUEST_PLAY')
          } else {
            if (isController.value) reportPauseEvent()
            else if (!shouldSkipControlEvent('REQUEST_PAUSE')) sendRequestEvent('REQUEST_PAUSE')
          }
        }

        // 循环/随机变化 -> PLAYBACK_MODE (Android-aligned)
        const wireRepeat = desktopRepeatToWire(newVal.repeatMode)
        const wireShuffle = !!newVal.shuffleEnabled
        if (
          wireRepeat !== _lastReportedRepeatMode
          || wireShuffle !== _lastReportedShuffle
        ) {
          _lastReportedRepeatMode = wireRepeat
          _lastReportedShuffle = wireShuffle
          if (isController.value) {
            reportPlaybackModeEvent()
          } else if (!shouldSkipControlEvent('REQUEST_PLAYBACK_MODE')) {
            sendRequestEvent('REQUEST_PLAYBACK_MODE', {
              repeatMode: wireRepeat,
              shuffleEnabled: wireShuffle,
            })
          }
        }
      },
      { deep: false },
    )

    _seekWatchStop = watch(
      () => player.lastSeekCommand.seq,
      () => {
        if (_suppressPlayerWatch || connectionState.value !== 'connected') return
        const seek = player.lastSeekCommand
        if (seek.source !== 'local') return
        if (player.isRemoteSyncGuardActive()) return

        scheduleLocalSeekReport(seek.positionMs)
      },
    )
  }

  function teardownPlayerWatch() {
    _playerWatchStop?.()
    _seekWatchStop?.()
    _playerWatchStop = null
    _seekWatchStop = null
    clearPendingSeekReport()
  }

  // 事件发送
  function generateEventId(): string {
    return `${userUuid.value.slice(0, 8)}-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`
  }

  async function sendEvent(event: ListenTogetherEvent) {
    if (!event.eventId) event.eventId = generateEventId()
    if (!event.clientTimeMs) event.clientTimeMs = Date.now()
    // 乱序保护字段（协议已声明、Android 每事件必带）：实例标识 + 单调递增序号
    if (!event.clientInstanceId) event.clientInstanceId = _clientInstanceId
    if (event.clientSequence == null) event.clientSequence = ++_clientSequence

    _recentOutboundEventIds.add(event.eventId!)
    // 清理过期 ID（保留最近 50 个）
    if (_recentOutboundEventIds.size > 50) {
      const iter = _recentOutboundEventIds.values()
      _recentOutboundEventIds.delete(iter.next().value!)
    }

    try {
      await invoke('lt_send_event', { event })
    } catch (e) {
      log.error('send event failed:', e)
    }
  }

  function shouldSkipControlEvent(type: string) {
    const now = Date.now()
    if (_lastSentControlType === type && now - _lastSentControlAt < CONTROL_EVENT_DEDUP_MS) {
      return true
    }
    _lastSentControlType = type
    _lastSentControlAt = now
    return false
  }

  function shouldSkipSeekEvent(positionMs: number) {
    const now = Date.now()
    if (
      _lastSentSeekPosition !== null
      && Math.abs(_lastSentSeekPosition - positionMs) < SEEK_EVENT_MIN_DELTA_MS
      && now - _lastSentSeekAt < SEEK_EVENT_DEDUP_MS
    ) {
      return true
    }
    _lastSentSeekPosition = positionMs
    _lastSentSeekAt = now
    return false
  }

  function reportPlayEvent() {
    const player = usePlayerStore()
    if (shouldSkipControlEvent('PLAY')) return
    sendEvent({
      type: 'PLAY',
      positionMs: player.positionMs,
      state: 'playing',
    })
  }

  function reportPauseEvent() {
    const player = usePlayerStore()
    if (shouldSkipControlEvent('PAUSE')) return
    sendEvent({
      type: 'PAUSE',
      positionMs: player.positionMs,
      state: 'paused',
    })
  }

  function reportSeekEvent(positionMs: number) {
    if (shouldSkipSeekEvent(positionMs)) return
    sendEvent({
      type: 'SEEK',
      positionMs,
    })
  }

  function reportPlaybackModeEvent() {
    const player = usePlayerStore()
    if (shouldSkipControlEvent('PLAYBACK_MODE')) return
    const repeatMode = desktopRepeatToWire(player.repeatMode)
    const shuffleEnabled = !!player.shuffleEnabled
    _lastReportedRepeatMode = repeatMode
    _lastReportedShuffle = shuffleEnabled
    sendEvent({
      type: 'PLAYBACK_MODE',
      repeatMode,
      shuffleEnabled,
      positionMs: player.positionMs,
      state: player.isPlaying ? 'playing' : 'paused',
    })
  }

  function scheduleLocalSeekReport(positionMs: number) {
    const player = usePlayerStore()
    _pendingSeekReport = {
      positionMs,
      trackId: player.currentTrack?.id ?? null,
    }

    if (_pendingSeekTimer) {
      clearTimeout(_pendingSeekTimer)
    }

    _pendingSeekTimer = setTimeout(() => {
      flushPendingSeekReport()
    }, LOCAL_SEEK_REPORT_DEBOUNCE_MS)
  }

  function flushPendingSeekReport() {
    if (_pendingSeekTimer) {
      clearTimeout(_pendingSeekTimer)
      _pendingSeekTimer = null
    }

    const pending = _pendingSeekReport
    _pendingSeekReport = null
    if (!pending || connectionState.value !== 'connected') return

    const player = usePlayerStore()
    if (pending.trackId && player.currentTrack?.id !== pending.trackId) return

    if (isController.value) {
      reportSeekEvent(pending.positionMs)
    } else {
      if (shouldSkipSeekEvent(pending.positionMs)) return
      sendRequestEvent('REQUEST_SEEK', { positionMs: pending.positionMs })
    }
  }

  function clearPendingSeekReport() {
    if (_pendingSeekTimer) {
      clearTimeout(_pendingSeekTimer)
      _pendingSeekTimer = null
    }
    _pendingSeekReport = null
  }

  function reportSetTrackEvent(track: any, currentIndex: number) {
    const player = usePlayerStore()
    const { queue: ltQueue } = toShareableQueueSnapshot(
      player.queue,
      player.queueIndex,
      roomSettings.value.shareAudioLinks,
    )
    sendEvent({
      type: 'SET_TRACK',
      track,
      currentIndex,
      queue: ltQueue,
      positionMs: 0,
      shouldPlay: player.isPlaying,
    })
  }

  function sendRequestEvent(type: string, extra: Partial<ListenTogetherEvent> = {}) {
    const player = usePlayerStore()
    sendEvent({
      type,
      positionMs: player.positionMs,
      ...extra,
    })
  }

  // 心跳
  function startHeartbeat() {
    stopHeartbeat()
    if (!isController.value) return

    _heartbeatTimer = setInterval(() => {
      if (connectionState.value !== 'connected') return

      const player = usePlayerStore()
      const { queue: ltQueue, resolvedIndex } = toShareableQueueSnapshot(
        player.queue,
        player.queueIndex,
        roomSettings.value.shareAudioLinks,
      )

      sendEvent({
        type: 'HEARTBEAT',
        positionMs: player.positionMs,
        state: player.isPlaying ? 'playing' : 'paused',
        queue: ltQueue,
        currentIndex: resolvedIndex,
        track: player.currentTrack ? trackInfoToLtTrack(player.currentTrack) : undefined,
        repeatMode: desktopRepeatToWire(player.repeatMode),
        shuffleEnabled: !!player.shuffleEnabled,
      })
    }, HEARTBEAT_INTERVAL_MS)
  }

  function stopHeartbeat() {
    if (_heartbeatTimer) {
      clearInterval(_heartbeatTimer)
      _heartbeatTimer = null
    }
  }

  // listener 侧存活探测：定期 ping 服务端，让 TCP 半开连接尽快暴露为断开
  function startListenerPing() {
    stopListenerPing()
    if (isController.value) return
    _listenerPingTimer = setInterval(() => {
      if (connectionState.value !== 'connected') return
      void invoke('lt_send_ping').catch(() => {})
    }, LISTENER_PING_INTERVAL_MS)
  }

  function stopListenerPing() {
    if (_listenerPingTimer) {
      clearInterval(_listenerPingTimer)
      _listenerPingTimer = null
    }
  }

  // 断线重连
  function scheduleReconnect() {
    if (_reconnectTimer) return

    const delay = RECONNECT_DELAYS[Math.min(_reconnectAttempt, RECONNECT_DELAYS.length - 1)]
    _reconnectAttempt++

    _reconnectTimer = setTimeout(async () => {
      _reconnectTimer = null
      if (!_wsUrl || !roomId.value) return

      connectionState.value = 'connecting'
      try {
        await invoke('lt_connect_ws', { wsUrl: _wsUrl })
        // 重连后拉取最新 state
        const stateResp = await invoke<any>('lt_get_room_state', {
          baseUrl: baseUrl.value,
          roomId: roomId.value,
        })
        if (stateResp.ok && stateResp.state) {
          roomState.value = stateResp.state
          _lastAppliedRoomVersion = stateResp.state.version || 0
          applyRoomStateToPlayer(stateResp.state, 'reconnect', stateResp.expectedPositionMs)
        }
        if (isController.value) startHeartbeat()
      } catch {
        scheduleReconnect()
      }
    }, delay)
  }

  // 房间设置更新
  async function updateRoomSettings(newSettings: Partial<ListenTogetherRoomSettings>) {
    if (newSettings.allowMemberControl !== undefined) settings.ltAllowMemberControl = newSettings.allowMemberControl
    if (newSettings.autoPauseOnMemberChange !== undefined) settings.ltAutoPauseOnMemberChange = newSettings.autoPauseOnMemberChange
    if (newSettings.shareAudioLinks !== undefined) settings.ltShareAudioLinks = newSettings.shareAudioLinks
    if (isController.value && connectionState.value === 'connected') {
      sendEvent({
        type: 'UPDATE_SETTINGS',
        roomSettings: roomSettings.value,
      })
    }
  }

  // 邀请链接
  function getInviteLink(): string {
    return `neriplayer://listen-together/join?roomId=${roomId.value}&baseUrl=${encodeURIComponent(baseUrl.value)}`
  }

  async function copyInviteLink() {
    const link = getInviteLink()
    try {
      await writeText(link)
      const toast = useToastStore()
      const t = (i18n.global as any).t
      toast.success(t('listen_together.invite_copied'))
    } catch {}
  }

  /** 检测剪贴板中的邀请链接 */
  async function checkClipboardInvite(): Promise<{ roomId: string; baseUrl?: string } | null> {
    try {
      const text = await readText()
      if (!text) return null
      const match = text.match(/neriplayer:\/\/listen-together\/join\?roomId=([^&]+)(?:&baseUrl=([^&\s]+))?/)
      if (match) {
        return {
          roomId: match[1],
          baseUrl: match[2] ? decodeURIComponent(match[2]) : undefined,
        }
      }
    } catch {}
    return null
  }

  // 工具函数
  function buildWsUrl(base: string, roomId: string, token: string): string {
    const normalized = base.replace(/\/$/, '')
    const httpUrl = `${normalized}/api/rooms/${roomId}/ws?token=${encodeURIComponent(token)}`
    return httpUrl.replace(/^http/, 'ws')
  }

  function loadOrCreateUuid(): string {
    let uuid = localStorage.getItem(LT_UUID_KEY)
    if (!uuid) {
      uuid = crypto.randomUUID()
      localStorage.setItem(LT_UUID_KEY, uuid)
    }
    return uuid
  }

  function markSync(eventType: string, timestamp = Date.now()) {
    lastSyncEventType.value = eventType
    lastSyncAt.value = timestamp
  }

  return {
    // 状态
    connectionState, roomId, userUuid, nickname, role,
    roomState, sessionError, baseUrl, roomSettings,
    lastSyncEventType, lastSyncAt, lastReconnectAt,
    // 计算属性
    isConnected, isController, members,
    // 方法
    createRoom, joinRoom, leaveRoom,
    updateRoomSettings, copyInviteLink, getInviteLink,
    checkClipboardInvite,
    // 暴露给外部（seek 上报）
    reportSeekEvent,
  }
})
