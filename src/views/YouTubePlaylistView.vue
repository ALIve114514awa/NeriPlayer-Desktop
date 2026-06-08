<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { usePlayerStore, type TrackInfo } from '@/stores/player'
import { useDownloadStore } from '@/stores/download'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import AddToPlaylistDialog from '@/components/AddToPlaylistDialog.vue'

const route = useRoute()
const router = useRouter()
const player = usePlayerStore()
const downloadStore = useDownloadStore()
const { t } = useI18n()

const isLoading = ref(true)
const error = ref<string | null>(null)
const playlistName = ref('')
const subtitle = ref('')
const coverUrl = ref('')
const searchQuery = ref('')

const tracks = ref<TrackInfo[]>([])

const filteredTracks = computed(() => {
  if (!searchQuery.value) return tracks.value
  const q = searchQuery.value.toLowerCase()
  return tracks.value.filter(t =>
    t.title.toLowerCase().includes(q) || t.artist.toLowerCase().includes(q)
  )
})

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  return `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`
}

// 解析 InnerTube browse 响应中的歌曲列表
function parsePlaylistTracks(data: any): TrackInfo[] {
  const result: TrackInfo[] = []
  try {
    const tabs = data?.contents?.singleColumnBrowseResultsRenderer?.tabs ||
                 data?.contents?.twoColumnBrowseResultsRenderer?.tabs || []

    for (const tab of tabs) {
      const contents = tab?.tabRenderer?.content?.sectionListRenderer?.contents || []
      for (const section of contents) {
        const items = section?.musicShelfRenderer?.contents ||
                      section?.musicPlaylistShelfRenderer?.contents || []
        for (const item of items) {
          const renderer = item?.musicResponsiveListItemRenderer
          if (!renderer) continue

          // 提取 videoId
          const overlay = renderer?.overlay?.musicItemThumbnailOverlayRenderer
          const videoId = overlay?.content?.musicPlayButtonRenderer?.playNavigationEndpoint?.watchEndpoint?.videoId
          if (!videoId) continue

          // 提取标题
          const titleRuns = renderer?.flexColumns?.[0]?.musicResponsiveListItemFlexColumnRenderer?.text?.runs || []
          const title = titleRuns.map((r: any) => r.text).join('')

          // 提取艺术家
          const artistRuns = renderer?.flexColumns?.[1]?.musicResponsiveListItemFlexColumnRenderer?.text?.runs || []
          const artist = artistRuns.map((r: any) => r.text).join('')

          // 提取封面
          const thumbnails = renderer?.thumbnail?.musicThumbnailRenderer?.thumbnail?.thumbnails || []
          const cover = thumbnails[thumbnails.length - 1]?.url || ''

          // 提取时长
          const durationText = renderer?.fixedColumns?.[0]?.musicResponsiveListItemFixedColumnRenderer?.text?.runs?.[0]?.text || ''
          const durationMs = parseDuration(durationText)

          result.push({
            id: `youtube:${videoId}`,
            title,
            artist,
            album: '',
            durationMs,
            coverUrl: cover,
            audioUrl: '',
          })
        }
      }
    }
  } catch {
    // 解析失败返回空
  }

  // 也尝试解析 header 信息
  try {
    const header = data?.header?.musicImmersiveHeaderRenderer ||
                   data?.header?.musicDetailHeaderRenderer ||
                   data?.header?.musicEditablePlaylistDetailHeaderRenderer?.header?.musicDetailHeaderRenderer
    if (header) {
      playlistName.value = header?.title?.runs?.[0]?.text || playlistName.value
      subtitle.value = header?.subtitle?.runs?.map((r: any) => r.text).join('') || ''
      const thumbs = header?.thumbnail?.musicThumbnailRenderer?.thumbnail?.thumbnails ||
                     header?.thumbnail?.croppedSquareThumbnailRenderer?.thumbnail?.thumbnails || []
      if (thumbs.length > 0) coverUrl.value = thumbs[thumbs.length - 1]?.url || coverUrl.value
    }
  } catch {
    // 忽略
  }

  return result
}

function parseDuration(text: string): number {
  if (!text) return 0
  const parts = text.split(':').map(Number)
  if (parts.length === 2) return (parts[0] * 60 + parts[1]) * 1000
  if (parts.length === 3) return (parts[0] * 3600 + parts[1] * 60 + parts[2]) * 1000
  return 0
}

async function loadDetail() {
  const browseId = route.params.browseId as string
  if (!browseId) return

  isLoading.value = true
  error.value = null

  try {
    const data = await invoke<any>('get_youtube_playlist_detail', { browseId })
    tracks.value = parsePlaylistTracks(data)
  } catch (e: any) {
    error.value = e?.toString() || t('player.load_failed')
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

function playTrack(track: TrackInfo) {
  player.playAll(filteredTracks.value)
  player.play(track)
}

const trackMenu = ref<{ show: boolean; x: number; y: number; track: TrackInfo | null }>({
  show: false, x: 0, y: 0, track: null,
})
const showAddToPlaylist = ref(false)
const addToPlaylistTarget = ref<TrackInfo | null>(null)

function clampMenuPosition(x: number, y: number, menuWidth = 200, menuHeight = 184) {
  return {
    x: Math.max(8, Math.min(x, window.innerWidth - menuWidth - 8)),
    y: Math.max(8, Math.min(y, window.innerHeight - menuHeight - 8)),
  }
}

function openTrackMenu(e: MouseEvent, track: TrackInfo) {
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 184
  let x = rect.left - menuWidth - 4
  let y = rect.top
  if (x < 8) x = rect.right + 4
  const pos = clampMenuPosition(x, y, menuWidth, menuHeight)
  trackMenu.value = { show: true, x: pos.x, y: pos.y, track }
}

function openTrackContextMenu(e: MouseEvent, track: TrackInfo) {
  const pos = clampMenuPosition(e.clientX, e.clientY)
  trackMenu.value = { show: true, x: pos.x, y: pos.y, track }
}

function closeTrackMenu() {
  trackMenu.value.show = false
}

function openAddToPlaylist(track: TrackInfo) {
  closeTrackMenu()
  addToPlaylistTarget.value = track
  showAddToPlaylist.value = true
}

function addToQueueNext(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueNext(track)
}

function addToQueueEnd(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueEnd(track)
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
        <input v-model="searchQuery" :placeholder="t('player.search_tracks')" class="search-input" />
      </div>
    </header>

    <div v-if="isLoading" class="state-center">
      <span class="material-symbols-rounded spinning">progress_activity</span>
      <p>{{ t('player.loading') }}</p>
    </div>

    <div v-else-if="error" class="state-center">
      <span class="material-symbols-rounded" style="font-size: 48px; opacity: 0.3">error</span>
      <p>{{ error }}</p>
      <button class="retry-btn" @click="loadDetail">{{ t('player.retry') }}</button>
    </div>

    <template v-else>
      <div class="detail-hero">
        <div class="hero-cover">
          <img v-if="coverUrl" :src="coverUrl" referrerpolicy="no-referrer" />
          <span v-else class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.3">queue_music</span>
        </div>
        <div class="hero-info">
          <h1 class="hero-title">{{ playlistName }}</h1>
          <p v-if="subtitle" class="hero-creator">{{ subtitle }}</p>
          <p class="hero-meta">{{ t('player.track_count', { count: tracks.length }) }}</p>
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

      <div v-if="filteredTracks.length === 0" class="state-center">
        <p>{{ t('player.empty_playlist') }}</p>
      </div>
      <div v-else class="track-list">
        <div
          v-for="(track, index) in filteredTracks"
          :key="track.id"
          class="track-item"
          :class="{ active: player.currentTrack?.id === track.id }"
          @click="playTrack(track)"
          @contextmenu.prevent.stop="openTrackContextMenu($event, track)"
        >
          <div class="track-index">
            <div v-if="player.currentTrack?.id === track.id && player.isPlaying" class="equalizer-bars"><span class="bar"/><span class="bar"/><span class="bar"/></div>
            <span v-else class="index-num">{{ index + 1 }}</span>
          </div>
          <div class="track-cover">
            <img v-if="track.coverUrl" :src="track.coverUrl" referrerpolicy="no-referrer" loading="lazy" @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <span v-else class="material-symbols-rounded filled">music_note</span>
          </div>
          <div class="track-info">
            <div class="track-title">{{ track.title }}</div>
            <div class="track-meta">{{ track.artist }}</div>
          </div>
          <div class="track-duration">{{ formatDuration(track.durationMs) }}</div>
          <button class="track-more" @click.stop="openTrackMenu($event, track)">
            <span class="material-symbols-rounded">more_vert</span>
          </button>
        </div>
      </div>
    </template>

    <Teleport to="body">
      <div v-if="trackMenu.show" class="context-overlay" @click="closeTrackMenu" @contextmenu.prevent="closeTrackMenu">
        <div class="context-menu" :style="{ left: trackMenu.x + 'px', top: trackMenu.y + 'px' }">
          <button class="ctx-item" @click="addToQueueNext(trackMenu.track!)">
            <span class="material-symbols-rounded" style="font-size: 20px">queue_play_next</span>
            <span>{{ t('player.play_next') }}</span>
          </button>
          <button class="ctx-item" @click="addToQueueEnd(trackMenu.track!)">
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
</style>
