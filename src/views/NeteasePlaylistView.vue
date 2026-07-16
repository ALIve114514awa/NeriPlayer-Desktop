<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { usePlayerStore, type TrackInfo } from '@/stores/player'
import { useDownloadStore } from '@/stores/download'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import AddToPlaylistDialog from '@/components/AddToPlaylistDialog.vue'
import {
  playlistDetailCacheKey,
  readPlaylistDetailCache,
  writePlaylistDetailCache,
} from '@/utils/playlistDetailCache'

const props = defineProps<{ isAlbum?: boolean }>()
const route = useRoute()
const router = useRouter()
const player = usePlayerStore()
const downloadStore = useDownloadStore()
const { t } = useI18n()

const isLoading = ref(true)
const error = ref<string | null>(null)
const playlistName = ref('')
const coverUrl = ref('')
const trackCount = ref(0)
const playCount = ref(0)
const description = ref('')
const creator = ref('')
const searchQuery = ref('')

const tracks = ref<TrackInfo[]>([])

interface NeteaseDetailCache {
  playlistName: string
  coverUrl: string
  trackCount: number
  playCount: number
  description: string
  creator: string
  tracks: TrackInfo[]
}

function applyDetailCache(cache: NeteaseDetailCache) {
  playlistName.value = cache.playlistName
  coverUrl.value = cache.coverUrl
  trackCount.value = cache.trackCount
  playCount.value = cache.playCount
  description.value = cache.description
  creator.value = cache.creator
  tracks.value = cache.tracks
}

function saveDetailCache(cacheKey: string) {
  writePlaylistDetailCache<NeteaseDetailCache>(cacheKey, {
    playlistName: playlistName.value,
    coverUrl: coverUrl.value,
    trackCount: trackCount.value,
    playCount: playCount.value,
    description: description.value,
    creator: creator.value,
    tracks: tracks.value,
  })
}

const filteredTracks = computed(() => {
  if (!searchQuery.value) return tracks.value
  const q = searchQuery.value.toLowerCase()
  return tracks.value.filter(t =>
    t.title.toLowerCase().includes(q) || t.artist.toLowerCase().includes(q)
  )
})

// 总时长
const totalDuration = computed(() => {
  const totalMs = tracks.value.reduce((sum, t) => sum + (t.durationMs || 0), 0)
  return formatTotalDuration(totalMs)
})

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  return `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`
}

function formatTotalDuration(ms: number): string {
  const totalMin = Math.floor(ms / 60000)
  if (totalMin >= 60) {
    const h = Math.floor(totalMin / 60)
    const m = totalMin % 60
    return `${h}${t('common.hour_short')} ${m}${t('common.minute_short')}`
  }
  return `${totalMin}${t('common.minute_short')}`
}

async function loadDetail() {
  const id = Number(route.params.id)
  if (!id) return

  const cacheKey = playlistDetailCacheKey(props.isAlbum ? 'netease-album' : 'netease-playlist', id)
  const cached = readPlaylistDetailCache<NeteaseDetailCache>(cacheKey)
  if (cached) {
    applyDetailCache(cached)
    isLoading.value = false
  } else {
    isLoading.value = true
  }
  error.value = null

  try {
    if (props.isAlbum) {
      const data = await invoke<any>('get_album_detail', { albumId: id })
      const album = data?.album || {}
      playlistName.value = album.name || ''
      coverUrl.value = album.picUrl || album.blurPicUrl || ''
      description.value = album.description || ''
      creator.value = album.artist?.name || ''

      const songs = data?.songs || []
      tracks.value = songs.map((s: any) => ({
        id: `netease:${s.id}`,
        title: s.name || '',
        artist: (s.ar || []).map((a: any) => a.name).join(', '),
        album: s.al?.name || album.name || '',
        durationMs: s.dt || 0,
        coverUrl: s.al?.picUrl || album.picUrl || '',
        audioUrl: '',
      }))
      trackCount.value = tracks.value.length
    } else {
      const data = await invoke<any>('get_netease_playlist_detail', { playlistId: id })
      const pl = data?.playlist || {}
      playlistName.value = pl.name || ''
      coverUrl.value = pl.coverImgUrl || ''
      trackCount.value = pl.trackCount || 0
      playCount.value = pl.playCount || 0
      description.value = pl.description || ''
      creator.value = pl.creator?.nickname || ''

      const songs = pl.tracks || []
      tracks.value = songs.map((s: any) => ({
        id: `netease:${s.id}`,
        title: s.name || '',
        artist: (s.ar || []).map((a: any) => a.name).join(', '),
        album: s.al?.name || '',
        durationMs: s.dt || 0,
        coverUrl: s.al?.picUrl || '',
        audioUrl: '',
      }))
    }
    saveDetailCache(cacheKey)
  } catch (e: any) {
    if (!cached) {
      error.value = e?.toString() || t('player.load_failed')
    }
  } finally {
    isLoading.value = false
  }
}

function playAll() {
  if (tracks.value.length === 0) return
  player.playAll(tracks.value)
}

function shufflePlay() {
  if (tracks.value.length === 0) return
  player.shufflePlay(tracks.value)
}

function playTrack(track: TrackInfo, index: number) {
  player.playAll(filteredTracks.value)
  player.play(track)
}

function formatPlayCount(count: number): string {
  if (count >= 100000000) return (count / 100000000).toFixed(1) + t('common.hundred_million')
  if (count >= 10000) return (count / 10000).toFixed(1) + t('common.ten_thousand')
  return count.toString()
}

// 曲目右键菜单
const trackMenu = ref<{ show: boolean; x: number; y: number; track: TrackInfo | null }>({
  show: false, x: 0, y: 0, track: null,
})
const showAddToPlaylist = ref(false)
const addToPlaylistTarget = ref<TrackInfo | null>(null)

function openTrackMenu(e: MouseEvent, track: TrackInfo) {
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 184
  let x = rect.left - menuWidth - 4
  let y = rect.top
  if (x < 8) x = rect.right + 4
  if (x + menuWidth > window.innerWidth - 8) x = window.innerWidth - menuWidth - 8
  if (y + menuHeight > window.innerHeight - 8) y = window.innerHeight - menuHeight - 8
  trackMenu.value = { show: true, x, y, track }
}

function openTrackContextMenu(e: MouseEvent, track: TrackInfo) {
  const menuWidth = 200
  const menuHeight = 184
  trackMenu.value = {
    show: true,
    x: Math.max(8, Math.min(e.clientX, window.innerWidth - menuWidth - 8)),
    y: Math.max(8, Math.min(e.clientY, window.innerHeight - menuHeight - 8)),
    track,
  }
}

function closeTrackMenu() {
  trackMenu.value.show = false
}

function openAddToPlaylist(track: TrackInfo) {
  closeTrackMenu()
  addToPlaylistTarget.value = track
  showAddToPlaylist.value = true
}

function downloadTaskStatusText(status?: string) {
  switch (status) {
    case 'resolving': return t('download.resolving')
    case 'downloading': return t('download.downloading')
    case 'cancelling': return t('download.cancelling')
    case 'cancelled': return t('download.cancelled')
    case 'error': return t('download.download_failed')
    case 'already_exists': return t('download.already_exists')
    default: return t('download.downloading')
  }
}

function trackDownloadLabel(track: TrackInfo) {
  const task = downloadStore.downloading.get(track.id)
  if (task) return downloadTaskStatusText(task.status)
  if (downloadStore.isDownloaded(track.id)) return t('download.redownload')
  return t('download.download')
}

function isTrackDownloadDisabled(track: TrackInfo) {
  if (downloadStore.isDownloading(track.id)) return true
  return downloadStore.isDownloaded(track.id)
    && player.currentTrack?.id === track.id
    && player.isPlayingFromDownload
}

async function handleTrackDownload(track: TrackInfo) {
  closeTrackMenu()
  if (isTrackDownloadDisabled(track)) return
  if (downloadStore.isDownloaded(track.id)) {
    await downloadStore.redownloadTrack(track)
  } else {
    await downloadStore.downloadTrack(track)
  }
}

onMounted(() => {
  downloadStore.initEvents()
  void downloadStore.loadDownloads()
  void loadDetail()
})
</script>

<template>
  <div class="detail-view">
    <header class="detail-header">
      <button class="back-btn" @click="router.back()">
        <span class="material-symbols-rounded">arrow_back</span>
      </button>
      <div class="header-search" v-if="!isLoading && tracks.length > 0">
        <span class="material-symbols-rounded search-icon">search</span>
        <input
          v-model="searchQuery"
          :placeholder="t('player.search_tracks')"
          class="search-input"
        />
      </div>
    </header>

    <!-- 加载状态 -->
    <div v-if="isLoading" class="state-center">
      <span class="material-symbols-rounded spinning">progress_activity</span>
      <p>{{ t('player.loading') }}</p>
    </div>

    <!-- 错误状态 -->
    <div v-else-if="error" class="state-center">
      <span class="material-symbols-rounded" style="font-size: 48px; opacity: 0.3">error</span>
      <p>{{ error }}</p>
      <button class="retry-btn" @click="loadDetail">{{ t('player.retry') }}</button>
    </div>

    <template v-else>
      <!-- 歌单 / 专辑 信息头 -->
      <div class="detail-hero">
        <div class="hero-cover">
          <img v-if="coverUrl" :src="coverUrl" referrerpolicy="no-referrer" />
          <span v-else class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.3">queue_music</span>
        </div>
        <div class="hero-info">
          <h1 class="hero-title">{{ playlistName }}</h1>
          <p v-if="creator" class="hero-creator">{{ creator }}</p>
          <p class="hero-meta">
            {{ t('player.track_count', { count: trackCount }) }} · {{ totalDuration }}
            <span v-if="playCount"> · {{ formatPlayCount(playCount) }}</span>
          </p>
          <p v-if="description" class="hero-desc">{{ description }}</p>
          <div class="hero-actions">
            <button class="play-all-btn" @click="playAll">
              <span class="material-symbols-rounded filled">play_arrow</span>
              {{ t('player.play_all') }}
            </button>
            <button class="hero-icon-btn" :title="t('player.shuffle_play')" @click="shufflePlay">
              <span class="material-symbols-rounded">shuffle</span>
            </button>
          </div>
        </div>
      </div>

      <!-- 歌曲列表 -->
      <div v-if="filteredTracks.length === 0" class="state-center">
        <p>{{ t('player.empty_playlist') }}</p>
      </div>
      <div v-else class="track-list">
        <div
          v-for="(track, index) in filteredTracks"
          :key="track.id"
          class="track-item"
          :class="{ active: player.currentTrack?.id === track.id }"
          @click="playTrack(track, index)"
          @contextmenu.prevent.stop="openTrackContextMenu($event, track)"
        >
          <div class="track-index">
            <div v-if="player.currentTrack?.id === track.id && player.isPlaying" class="equalizer-bars"><span class="bar"/><span class="bar"/><span class="bar"/></div>
            <span v-else class="index-num">{{ index + 1 }}</span>
          </div>
          <div class="track-cover">
            <img v-if="track.coverUrl && !props.isAlbum" :src="track.coverUrl" referrerpolicy="no-referrer" loading="lazy" @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <span v-else class="material-symbols-rounded filled">music_note</span>
          </div>
          <div class="track-info">
            <div class="track-title">{{ track.title }}</div>
            <div class="track-meta">{{ track.artist }}<template v-if="track.album"> · {{ track.album }}</template></div>
          </div>
          <div class="track-duration">{{ formatDuration(track.durationMs) }}</div>
          <button class="track-more" @click.stop="openTrackMenu($event, track)">
            <span class="material-symbols-rounded">more_vert</span>
          </button>
        </div>
      </div>
    </template>

    <!-- 曲目右键菜单 -->
    <Teleport to="body">
      <div v-if="trackMenu.show" class="context-overlay" @click="closeTrackMenu" @contextmenu.prevent="closeTrackMenu">
        <div class="context-menu" :style="{ left: trackMenu.x + 'px', top: trackMenu.y + 'px' }">
          <button class="ctx-item" @click="player.addToQueueNext(trackMenu.track!); closeTrackMenu()">
            <span class="material-symbols-rounded" style="font-size: 20px">queue_play_next</span>
            <span>{{ t('player.play_next') }}</span>
          </button>
          <button class="ctx-item" @click="player.addToQueueEnd(trackMenu.track!); closeTrackMenu()">
            <span class="material-symbols-rounded" style="font-size: 20px">add_to_queue</span>
            <span>{{ t('player.add_to_queue') }}</span>
          </button>
          <button class="ctx-item" @click="openAddToPlaylist(trackMenu.track!)">
            <span class="material-symbols-rounded" style="font-size: 20px">playlist_add</span>
            <span>{{ t('player.add_to_playlist') }}</span>
          </button>
          <button
            class="ctx-item"
            :disabled="isTrackDownloadDisabled(trackMenu.track!)"
            @click="handleTrackDownload(trackMenu.track!)"
          >
            <span class="material-symbols-rounded" style="font-size: 20px">download</span>
            <span>{{ trackDownloadLabel(trackMenu.track!) }}</span>
          </button>
        </div>
      </div>
    </Teleport>

    <AddToPlaylistDialog v-model:open="showAddToPlaylist" :track="addToPlaylistTarget" />
  </div>
</template>

<style scoped lang="scss">
@use '@/styles/detail-view.scss' as *;

.track-more {
  width: 32px;
  height: 32px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  opacity: 0;
  transition: opacity var(--duration-short), background var(--duration-short);

  .track-item:hover & { opacity: 0.6; }
  &:hover { opacity: 1 !important; background: var(--md-surface-container-high); }
  .material-symbols-rounded { font-size: 18px; }
}
</style>

<style lang="scss">
.context-overlay {
  position: fixed;
  inset: 0;
  z-index: 500;
}

.context-menu {
  position: fixed;
  min-width: 200px;
  background: var(--md-surface-container-high);
  border-radius: 12px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.28), 0 2px 8px rgba(0, 0, 0, 0.15);
  border: 1px solid var(--md-outline-variant);
  padding: 4px 0;
  z-index: 501;
  animation: ctx-in 120ms ease-out;
}

@keyframes ctx-in {
  from { opacity: 0; transform: scale(0.95) translateY(-4px); }
  to   { opacity: 1; transform: scale(1) translateY(0); }
}

.ctx-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 10px 16px;
  font-size: 14px;
  font-weight: 500;
  color: var(--md-on-surface);
  background: none;
  border: none;
  cursor: pointer;
  transition: background 150ms;
  font-family: inherit;

  &:hover { background: var(--md-surface-container-highest); }
  &.danger { color: var(--md-error); }
  &.danger:hover { background: color-mix(in srgb, var(--md-error) 8%, transparent); }
  &:disabled { cursor: default; opacity: 0.45; }
  &:disabled:hover { background: transparent; }
}
</style>
