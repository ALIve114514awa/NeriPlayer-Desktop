/**
 * 一起听协议 DTO 类型定义
 * 与安卓端 ListenTogether protocol 包完全对齐
 */

export const LtChannels = {
  NETEASE: 'netease',
  // 桌面扩展频道: Android 协议未声明, 服务端透传即可
  QQ_MUSIC: 'qqMusic',
  BILIBILI: 'bilibili',
  YOUTUBE_MUSIC: 'youtubeMusic',
  LOCAL: 'local',
} as const

/** ExoPlayer-aligned repeat integers used on the wire */
export const LtRepeatMode = {
  OFF: 0,
  ONE: 1,
  ALL: 2,
} as const

export type LtRepeatModeValue = (typeof LtRepeatMode)[keyof typeof LtRepeatMode]

export interface ListenTogetherTrack {
  stableKey: string
  channelId: string
  audioId: string
  subAudioId?: string
  playlistContextId?: string
  mediaUri?: string
  streamUrl?: string
  name: string
  artist: string
  album?: string
  durationMs: number
  coverUrl?: string
}

export interface ListenTogetherRoomSettings {
  allowMemberControl: boolean
  autoPauseOnMemberChange: boolean
  shareAudioLinks: boolean
}

export interface ListenTogetherMember {
  userUuid: string
  nickname: string
  userId?: string
  role: string
  joinedAt: number
}

export interface ListenTogetherPlaybackState {
  state: 'playing' | 'paused' | string
  basePositionMs: number
  baseTimestampMs: number
  playbackRate: number
  /** ExoPlayer: 0=OFF 1=ONE 2=ALL */
  repeatMode?: number | null
  shuffleEnabled?: boolean | null
}

export interface ListenTogetherRoomState {
  roomId: string
  version: number
  schemaVersion: number
  controllerUserUuid?: string
  controllerUserId?: string
  controllerHeartbeatAt?: number
  settings: ListenTogetherRoomSettings
  members: ListenTogetherMember[]
  queue: ListenTogetherTrack[]
  currentIndex: number
  track?: ListenTogetherTrack
  playback: ListenTogetherPlaybackState
  controllerOfflineSince?: number
  roomStatus: string
  closedReason?: string
  updatedAt: number
}

export interface ListenTogetherCause {
  userUuid?: string
  userId?: string
  nickname?: string
  eventId?: string
  type?: string
}

export interface ListenTogetherEvent {
  type: string
  eventId?: string
  clientTimeMs?: number
  clientInstanceId?: string
  clientSequence?: number
  positionMs?: number
  currentIndex?: number
  nextIndex?: number
  track?: ListenTogetherTrack
  queue?: ListenTogetherTrack[]
  roomSettings?: ListenTogetherRoomSettings
  shouldPlay?: boolean
  state?: string
  /** PLAYBACK_MODE / REQUEST_PLAYBACK_MODE */
  repeatMode?: number
  shuffleEnabled?: boolean
  requestTrackStableKey?: string
  finishedTrackStableKey?: string
}

export interface ListenTogetherSocketEnvelope {
  type: string
  sessionId?: string
  userUuid?: string
  userId?: string
  nickname?: string
  role?: string
  autoPauseOnJoin?: boolean
  state?: ListenTogetherRoomState
  expectedPositionMs?: number
  nowMs?: number
  t?: number
  ok?: boolean
  /// 控制命令应答：服务端拒绝时才有 error，缺它会让被拒绝的控制静默生效
  result?: {
    ok: boolean
    error?: string
    applied?: {
      type: string
      roomId?: string
      version?: number
      expectedPositionMs?: number
      nowMs?: number
    }
  }
  message?: string
  roomId?: string
  version?: number
  causedBy?: ListenTogetherCause
  track?: ListenTogetherTrack
  queue?: ListenTogetherTrack[]
  positionMs?: number
  currentIndex?: number
  requestTrackStableKey?: string
  shouldPlay?: boolean
  stateName?: string
  repeatMode?: number
  shuffleEnabled?: boolean
  clientTimeMs?: number
  clientInstanceId?: string
  clientSequence?: number
  requestSequence?: number
}

export interface ListenTogetherInitialSnapshot {
  queue: ListenTogetherTrack[]
  currentIndex: number
  track?: ListenTogetherTrack
  settings: ListenTogetherRoomSettings
  isPlaying: boolean
  positionMs: number
  repeatMode: number
  shuffleEnabled: boolean
}

export interface ListenTogetherRoomResponse {
  ok: boolean
  roomId?: string
  userUuid?: string
  userId?: string
  nickname?: string
  role?: string
  autoPauseOnJoin?: boolean
  token?: string
  state?: ListenTogetherRoomState
  wsUrl?: string
  error?: string
}

export interface ListenTogetherStateResponse {
  ok: boolean
  state?: ListenTogetherRoomState
  expectedPositionMs?: number
  serverNowMs?: number
  autoPauseOnJoin?: boolean
  error?: string
}

export type ConnectionState = 'disconnected' | 'connecting' | 'connected'
export type LtRole = 'controller' | 'listener'

/** 播放命令来源 */
export type PlaybackCommandSource = 'local' | 'remote_sync'

/** Desktop string mode <-> wire int (ExoPlayer) */
export function desktopRepeatToWire(mode: string | undefined | null): number {
  switch (mode) {
    case 'one':
      return LtRepeatMode.ONE
    case 'all':
      return LtRepeatMode.ALL
    default:
      return LtRepeatMode.OFF
  }
}

export function wireRepeatToDesktop(mode: number | null | undefined): 'off' | 'one' | 'all' | null {
  if (mode === null || mode === undefined || Number.isNaN(mode)) return null
  if (mode === LtRepeatMode.ONE) return 'one'
  if (mode === LtRepeatMode.ALL) return 'all'
  if (mode === LtRepeatMode.OFF) return 'off'
  return null
}
