/**
 * 一起听协议 DTO 类型定义
 * 与安卓端 ListenTogetherProtocol.kt 完全对齐
 */

export const LtChannels = {
  NETEASE: 'netease',
  BILIBILI: 'bilibili',
  YOUTUBE_MUSIC: 'youtubeMusic',
  LOCAL: 'local',
} as const

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
  state: 'playing' | 'paused'
  basePositionMs: number
  baseTimestampMs: number
  playbackRate: number
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
  positionMs?: number
  currentIndex?: number
  track?: ListenTogetherTrack
  queue?: ListenTogetherTrack[]
  roomSettings?: ListenTogetherRoomSettings
  shouldPlay?: boolean
  state?: string
  requestTrackStableKey?: string
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
  ok?: boolean
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
  clientTimeMs?: number
  requestSequence?: number
}

export interface ListenTogetherInitialSnapshot {
  queue: ListenTogetherTrack[]
  currentIndex: number
  track?: ListenTogetherTrack
  settings: ListenTogetherRoomSettings
  isPlaying: boolean
  positionMs: number
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
  autoPauseOnJoin?: boolean
  error?: string
}

export type ConnectionState = 'disconnected' | 'connecting' | 'connected'
export type LtRole = 'controller' | 'listener'

/** 播放命令来源 */
export type PlaybackCommandSource = 'local' | 'remote_sync'
