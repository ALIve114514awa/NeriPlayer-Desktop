import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { TrackInfo } from '@/stores/player'
import { useRecommendStore } from '@/stores/recommend'

interface PlaylistInfo {
  id: number
  name: string
}

const DEFAULT_LIKED_PLAYLIST_NAME = '我喜欢的音乐'
const LIKED_PLAYLIST_NAMES = [
  DEFAULT_LIKED_PLAYLIST_NAME,
  '我喜歡的音樂',
  'お気に入りの曲',
  'Liked Songs',
  'My Favorite Music',
]

export const useLikedSongsStore = defineStore('likedSongs', () => {
  const likedPlaylistId = ref<number | null>(null)
  const likedTrackIds = ref<Set<string>>(new Set())
  const isLoading = ref(false)
  const isReady = ref(false)

  let loadPromise: Promise<void> | null = null
  let unlistenPlaylistsChanged: UnlistenFn | null = null

  function inferTrackSource(trackId: string) {
    if (trackId.startsWith('netease:')) return 'netease'
    if (trackId.startsWith('qq:')) return 'qq'
    if (trackId.startsWith('bilibili:')) return 'bilibili'
    if (trackId.startsWith('youtube:')) return 'youtube'
    return 'local'
  }

  function toBackendTrack(track: TrackInfo) {
    return {
      id: track.id,
      title: track.title,
      artist: track.artist,
      album: track.album || '',
      duration_ms: track.durationMs || 0,
      cover_url: track.coverUrl || null,
      url: track.audioUrl || '',
      source: inferTrackSource(track.id),
    }
  }

  function getNeteaseSongId(track: TrackInfo) {
    if (!track.id.startsWith('netease:')) return null
    const id = Number.parseInt(track.id.replace('netease:', ''), 10)
    return Number.isFinite(id) ? id : null
  }

  function setTrackLiked(trackId: string, liked: boolean) {
    const next = new Set(likedTrackIds.value)
    if (liked) {
      next.add(trackId)
    } else {
      next.delete(trackId)
    }
    likedTrackIds.value = next
  }

  async function loadLikedPlaylist() {
    if (loadPromise) return loadPromise

    loadPromise = (async () => {
      isLoading.value = true
      try {
        const playlists = await invoke<PlaylistInfo[]>('list_playlists')
        const liked = playlists.find(p => LIKED_PLAYLIST_NAMES.includes(p.name))
        if (!liked) {
          likedPlaylistId.value = null
          likedTrackIds.value = new Set()
          return
        }

        likedPlaylistId.value = liked.id
        const tracks = await invoke<Array<{ id?: string }>>('get_playlist_tracks', { id: liked.id })
        likedTrackIds.value = new Set(tracks.map(t => t.id || '').filter(Boolean))
      } catch (e) {
        console.error('loadLikedPlaylist:', e)
      } finally {
        isReady.value = true
        isLoading.value = false
        loadPromise = null
      }
    })()

    return loadPromise
  }

  async function ensureLikedPlaylist() {
    await loadLikedPlaylist()
    if (likedPlaylistId.value !== null) return likedPlaylistId.value

    const created = await invoke<PlaylistInfo>('create_playlist', { name: DEFAULT_LIKED_PLAYLIST_NAME })
    likedPlaylistId.value = created.id
    likedTrackIds.value = new Set()
    return created.id
  }

  function isTrackLiked(track?: TrackInfo | null) {
    if (!track?.id) return false
    return likedTrackIds.value.has(track.id)
  }

  async function toggleTrack(track?: TrackInfo | null) {
    if (!track?.id) return false

    await loadLikedPlaylist()
    const shouldLike = !isTrackLiked(track)
    const neteaseSongId = getNeteaseSongId(track)

    try {
      if (shouldLike) {
        const playlistId = await ensureLikedPlaylist()
        await invoke('add_to_playlist', { playlistId, track: toBackendTrack(track) })
        setTrackLiked(track.id, true)
      } else if (likedPlaylistId.value !== null) {
        await invoke('remove_from_playlist', {
          playlistId: likedPlaylistId.value,
          trackId: track.id,
        })
        setTrackLiked(track.id, false)
      }
      if (neteaseSongId !== null) {
        useRecommendStore().toggleLikeSong(neteaseSongId, shouldLike).catch(() => {})
      }
      return true
    } catch (e) {
      console.error('toggleLikedTrack:', e)
      await loadLikedPlaylist()
      return false
    }
  }

  async function start() {
    if (!unlistenPlaylistsChanged) {
      try {
        unlistenPlaylistsChanged = await listen('playlists-changed', () => {
          loadLikedPlaylist()
        })
      } catch (e) {
        console.error('listen playlists-changed for liked songs:', e)
      }
    }
    await loadLikedPlaylist()
  }

  function stop() {
    if (unlistenPlaylistsChanged) {
      unlistenPlaylistsChanged()
      unlistenPlaylistsChanged = null
    }
  }

  return {
    likedPlaylistId,
    likedTrackIds,
    isLoading,
    isReady,
    loadLikedPlaylist,
    isTrackLiked,
    toggleTrack,
    start,
    stop,
  }
})
