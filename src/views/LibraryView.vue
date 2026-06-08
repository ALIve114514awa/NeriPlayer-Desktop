<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue'
import { useRouter, useRoute } from 'vue-router'

defineOptions({ name: 'LibraryView' })
import { useI18n } from 'vue-i18n'
import { useLibraryStore } from '@/stores/library'
import { usePlayerStore, type TrackInfo } from '@/stores/player'
import { useRecommendStore } from '@/stores/recommend'
import { useAuthStore } from '@/stores/auth'
import { useDownloadStore } from '@/stores/download'
import { useToastStore } from '@/stores/toast'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { open as dialogOpen } from '@tauri-apps/plugin-dialog'
import M3Dialog from '@/components/ui/M3Dialog.vue'
import M3Input from '@/components/ui/M3Input.vue'

const router = useRouter()
const route = useRoute()
const { t } = useI18n()
const library = useLibraryStore()
const player = usePlayerStore()
const recommend = useRecommendStore()
const auth = useAuthStore()
const downloadStore = useDownloadStore()
const toast = useToastStore()

// 喜欢的歌曲计数
const likedCount = computed(() => recommend.likedSongIds.size)

const tabs = computed(() => [
  { label: t('library.tab_local'), icon: 'folder_open', key: 'local' },
  { label: t('library.tab_favorites'), icon: 'favorite', key: 'favorites' },
  { label: t('library.tab_downloads'), icon: 'download', key: 'downloads' },
  { label: t('library.tab_netease_playlists'), icon: 'queue_music', key: 'netease_playlists' },
  { label: t('library.bilibili_favorites'), icon: 'video_library', key: 'bilibili_favorites' },
  { label: 'YouTube Music', icon: 'subscriptions', key: 'youtube_playlists' },
  { label: t('library.tab_netease_albums'), icon: 'album', key: 'netease_albums' },
])
// 根据路由 query 参数设置初始标签
const tabKeyToIndex: Record<string, number> = { local: 0, favorites: 1, downloads: 2, netease_playlists: 3, bilibili_favorites: 4, youtube_playlists: 5, netease_albums: 6 }
const initialTab = typeof route.query.tab === 'string' ? (tabKeyToIndex[route.query.tab] ?? 0) : 0
const activeTab = ref(initialTab)

// 监听路由 query 变化（同页面内导航）
watch(() => route.query.tab, (tab) => {
  if (typeof tab === 'string' && tab in tabKeyToIndex) {
    activeTab.value = tabKeyToIndex[tab]
  }
})

// 真实播放列表
interface PlaylistInfo { id: number; name: string; track_count: number; modified_at: number; cover_url: string | null }
const playlists = ref<PlaylistInfo[]>([])

async function loadPlaylists() {
  try {
    const raw = await invoke<PlaylistInfo[]>('list_playlists')
    // 排序：「我喜欢的音乐」置顶，「本地音乐」置底，其余保持原序
    const liked: PlaylistInfo[] = []
    const localFiles: PlaylistInfo[] = []
    const normal: PlaylistInfo[] = []
    for (const pl of raw) {
      if (LIKED_NAMES.includes(pl.name)) liked.push(pl)
      else if (LOCAL_NAMES.includes(pl.name)) localFiles.push(pl)
      else normal.push(pl)
    }
    playlists.value = [...liked, ...normal, ...localFiles]
  } catch (e) {
    console.error('Load playlists failed:', e)
  }
}

function inferTrackSource(track: TrackInfo) {
  if (track.id.startsWith('netease:')) return 'netease'
  if (track.id.startsWith('qq:')) return 'qq'
  if (track.id.startsWith('bilibili:')) return 'bilibili'
  if (track.id.startsWith('youtube:')) return 'youtube'
  return 'local'
}

function toBackendTrack(track: TrackInfo) {
  return {
    id: track.id,
    title: track.title,
    artist: track.artist,
    album: track.album || '',
    duration_ms: track.durationMs || 0,
    source: inferTrackSource(track),
    url: track.audioUrl || '',
    cover_url: track.coverUrl || null,
  }
}

function platformLabel(source?: string) {
  switch ((source || '').toLowerCase()) {
    case 'netease': return t('player.source_netease')
    case 'qq': return t('player.source_qq')
    case 'bilibili': return t('player.source_bilibili')
    case 'youtube': return t('player.source_youtube')
    case 'local': return t('player.source_local')
    default: return source || '—'
  }
}

// M3 Dialog 创建播放列表
const showCreateDialog = ref(false)
const newPlaylistName = ref('')
const inputRef = ref<InstanceType<typeof M3Input>>()

function openCreateDialog() {
  newPlaylistName.value = ''
  showCreateDialog.value = true
  nextTick(() => inputRef.value?.focus())
}

async function confirmCreate() {
  if (!newPlaylistName.value.trim()) return
  try {
    await invoke('create_playlist', { name: newPlaylistName.value.trim() })
    showCreateDialog.value = false
    await loadPlaylists()
  } catch (e) {
    console.error('Create playlist failed:', e)
  }
}

// 上下文菜单
const contextMenu = ref<{ show: boolean; x: number; y: number; playlist: PlaylistInfo | null }>({
  show: false, x: 0, y: 0, playlist: null,
})

// 特殊歌单：跨语言匹配（同步数据可能是任何语言的名称）
const LIKED_NAMES = ['我喜欢的音乐', '我喜歡的音樂', 'お気に入りの曲', 'Liked Songs']
const LOCAL_NAMES = ['本地音乐', '本機音樂', 'ローカル音楽', 'Local Music']
const ALL_PROTECTED = [...LIKED_NAMES, ...LOCAL_NAMES]

async function ensureLocalPlaylistId(): Promise<number> {
  const raw = await invoke<PlaylistInfo[]>('list_playlists')
  const localPlaylist = raw.find((pl) => LOCAL_NAMES.includes(pl.name))
  if (localPlaylist) return localPlaylist.id

  const created = await invoke<PlaylistInfo>('create_playlist', { name: t('home.local_music') })
  return created.id
}

async function syncScannedTracksToLocalPlaylist(scannedTracks: TrackInfo[]) {
  const localPlaylistId = await ensureLocalPlaylistId()

  const existingTracks = await invoke<any[]>('get_playlist_tracks', { id: localPlaylistId })
  const existingIds = (existingTracks || [])
    .map((track: any) => String(track?.id || ''))
    .filter(Boolean)

  if (existingIds.length > 0) {
    await invoke('remove_tracks_from_playlist', {
      playlistId: localPlaylistId,
      trackIds: existingIds,
    })
  }

  if (scannedTracks.length > 0) {
    await invoke('add_tracks_to_playlist', {
      playlistId: localPlaylistId,
      tracks: scannedTracks.map(toBackendTrack),
    })
  }

  await loadPlaylists()
}

async function selectAndScanLocalMusic() {
  if (library.isScanning) return
  try {
    const result = await dialogOpen({ directory: true, multiple: false })
    if (!result) return

    const dir = typeof result === 'string' ? result : (result as any).path || String(result)
    if (!dir || dir === '[object Object]') return

    await library.scanDirectory(dir)
    if (library.scanError) {
      toast.error(t('library.scan_failed'))
      return
    }

    await syncScannedTracksToLocalPlaylist(library.tracks)
    toast.success(t('library.scan_success', { count: library.tracks.length }))
  } catch (e) {
    console.error('Scan local music failed:', e)
    toast.error(t('library.scan_failed'))
  }
}

function isProtectedPlaylist(pl: PlaylistInfo) {
  return ALL_PROTECTED.includes(pl.name)
}

// 显示名：特殊歌单用当前语言翻译，其他原样
function displayName(pl: PlaylistInfo): string {
  if (LIKED_NAMES.includes(pl.name)) return t('library.liked_songs')
  if (LOCAL_NAMES.includes(pl.name)) return t('library.local_files')
  return pl.name
}

function openContextMenu(e: MouseEvent, pl: PlaylistInfo) {
  // 基于按钮位置定位，并确保不超出视口
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 100

  // 默认：按钮左侧弹出
  let x = rect.left - menuWidth - 4
  let y = rect.top

  // 左侧空间不足时改为右侧
  if (x < 8) x = rect.right + 4
  // 右侧仍然超出时贴左
  if (x + menuWidth > window.innerWidth - 8) x = window.innerWidth - menuWidth - 8
  // 底部超出时上移
  if (y + menuHeight > window.innerHeight - 8) y = window.innerHeight - menuHeight - 8

  contextMenu.value = { show: true, x, y, playlist: pl }
}

function closeContextMenu() {
  contextMenu.value.show = false
}

// 删除确认
const showDeleteDialog = ref(false)
const deleteTarget = ref<PlaylistInfo | null>(null)

function requestDelete(pl: PlaylistInfo) {
  closeContextMenu()
  deleteTarget.value = pl
  showDeleteDialog.value = true
}

async function confirmDelete() {
  if (!deleteTarget.value) return
  try {
    await invoke('delete_playlist', { id: deleteTarget.value.id })
    showDeleteDialog.value = false
    deleteTarget.value = null
    await loadPlaylists()
  } catch (e) {
    console.error('Delete playlist failed:', e)
  }
}

// 重命名
const showRenameDialog = ref(false)
const renameTarget = ref<PlaylistInfo | null>(null)
const renameValue = ref('')
const renameInputRef = ref<InstanceType<typeof M3Input>>()

function requestRename(pl: PlaylistInfo) {
  closeContextMenu()
  renameTarget.value = pl
  renameValue.value = pl.name
  showRenameDialog.value = true
  nextTick(() => renameInputRef.value?.focus())
}

async function confirmRename() {
  if (!renameTarget.value || !renameValue.value.trim()) return
  try {
    await invoke('rename_playlist', { id: renameTarget.value.id, name: renameValue.value.trim() })
    showRenameDialog.value = false
    renameTarget.value = null
    await loadPlaylists()
  } catch (e) {
    console.error('Rename playlist failed:', e)
  }
}

// 网易云用户歌单
const neteasePlaylists = computed(() => recommend.userPlaylists['netease'] || [])
// 哔哩哔哩收藏夹
const biliPlaylists = computed(() => recommend.userPlaylists['bilibili'] || [])
// YouTube Music 资料库歌单
const youtubePlaylists = computed(() => recommend.userPlaylists['youtube'] || [])

// 收藏歌单（从同步数据中获取）
interface FavoritePlaylist {
  id: string; name: string; coverUrl: string; trackCount: number; source: string;
  songs: any[]; addedTime: number; modifiedAt: number; isDeleted: boolean;
}
const favoritePlaylists = ref<FavoritePlaylist[]>([])

async function loadFavorites() {
  try {
    const raw = await invoke<any[]>('list_favorite_playlists')
    favoritePlaylists.value = (raw || []).map((f: any) => ({
      id: f.id ?? '',
      name: f.name ?? '',
      coverUrl: f.cover_url ?? '',
      trackCount: f.track_count ?? f.songs?.length ?? 0,
      source: f.source ?? '',
      songs: f.songs ?? [],
      addedTime: f.added_time ?? 0,
      modifiedAt: f.modified_at ?? 0,
      isDeleted: f.is_deleted ?? false,
    }))
  } catch (e) {
    console.error('Load favorites failed:', e)
  }
}

onMounted(loadPlaylists)
onMounted(loadFavorites)
onMounted(() => downloadStore.loadDownloads())

// 下载相关
function formatFileSize(bytes: number): string {
  if (bytes < 1024) return bytes + ' B'
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB'
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB'
}

function playDownloadedTrack(dl: any) {
  player.play({
    id: dl.id,
    title: dl.title,
    artist: dl.artist,
    album: dl.album,
    durationMs: dl.durationMs,
    coverUrl: dl.coverUrl || '',
    audioUrl: dl.filePath,
  })
}

// 下载列表右键菜单
const dlContextMenu = ref<{ show: boolean; x: number; y: number; track: any | null }>({
  show: false, x: 0, y: 0, track: null,
})

function openDlContextMenu(e: MouseEvent, track: any) {
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 96
  let x = rect.left - menuWidth - 4
  let y = rect.top
  if (x < 8) x = rect.right + 4
  if (x + menuWidth > window.innerWidth - 8) x = window.innerWidth - menuWidth - 8
  if (y + menuHeight > window.innerHeight - 8) y = window.innerHeight - menuHeight - 8
  dlContextMenu.value = { show: true, x, y, track }
}

function closeDlContextMenu() {
  dlContextMenu.value.show = false
}

// 删除下载确认
const showDlDeleteDialog = ref(false)
const dlDeleteTarget = ref<any>(null)

async function cancelActiveDownload(trackId: string) {
  await downloadStore.cancelDownload(trackId)
}

function activeDownloadStatusText(status: string) {
  switch (status) {
    case 'resolving': return t('download.resolving')
    case 'cancelling': return t('download.cancelling')
    case 'cancelled': return t('download.cancelled')
    case 'error': return t('download.download_failed')
    case 'already_exists': return t('download.already_exists')
    default: return t('download.downloading')
  }
}

function activeDownloadProgressText(task: {
  status: string
  progress?: number
  downloadedBytes?: number
  totalBytes?: number
  message?: string
}) {
  if (task.status === 'resolving' || task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'already_exists') {
    return activeDownloadStatusText(task.status)
  }
  if (task.status === 'error') {
    return task.message
      ? `${activeDownloadStatusText(task.status)} · ${task.message}`
      : activeDownloadStatusText(task.status)
  }

  const downloaded = typeof task.downloadedBytes === 'number' ? formatFileSize(task.downloadedBytes) : null
  const total = typeof task.totalBytes === 'number' && task.totalBytes > 0 ? formatFileSize(task.totalBytes) : null
  const percent = typeof task.progress === 'number' ? `${task.progress}%` : null

  if (downloaded && total && percent) return `${activeDownloadStatusText(task.status)} · ${downloaded} / ${total} · ${percent}`
  if (downloaded && total) return `${activeDownloadStatusText(task.status)} · ${downloaded} / ${total}`
  if (downloaded && percent) return `${activeDownloadStatusText(task.status)} · ${downloaded} · ${percent}`
  return activeDownloadStatusText(task.status)
}

function requestDlDelete(track: any) {
  if (isDownloadedTrackInUse(track)) return
  closeDlContextMenu()
  dlDeleteTarget.value = track
  showDlDeleteDialog.value = true
}

async function revealDownloadFile(track: any) {
  closeDlContextMenu()
  try {
    await invoke('reveal_file', { path: track.filePath })
  } catch (e) {
    console.error('Failed to reveal file:', e)
  }
}

async function confirmDlDelete() {
  if (!dlDeleteTarget.value) return
  const deleted = dlDeleteTarget.value
  await downloadStore.deleteDownload(deleted.id)
  player.handleDownloadedFileRemoved(deleted.id, deleted.filePath)
  showDlDeleteDialog.value = false
  dlDeleteTarget.value = null
}

function isDownloadedTrackInUse(track: any) {
  return player.isPlayingFromDownload && player.currentTrack?.id === track?.id
}

async function redownloadDownloadedTrack(track: any) {
  if (isDownloadedTrackInUse(track)) return
  closeDlContextMenu()
  player.handleDownloadedFileRemoved(track.id, track.filePath)
  await downloadStore.redownloadTrack({
    id: track.id,
    title: track.title,
    artist: track.artist,
    album: track.album || '',
    durationMs: track.durationMs || 0,
    coverUrl: track.coverUrl || '',
    audioUrl: '',
  })
}

// 拉取云端歌单
onMounted(() => {
  if (auth.netease.loggedIn && !neteasePlaylists.value.length) {
    recommend.fetchUserPlaylists('netease')
  }
  if (auth.bilibili.loggedIn && !biliPlaylists.value.length) {
    recommend.fetchUserPlaylists('bilibili')
  }
  if (auth.youtube.loggedIn && !youtubePlaylists.value.length) {
    recommend.fetchUserPlaylists('youtube')
  }
  // 网易云收藏专辑
  if (auth.netease.loggedIn && !recommend.userAlbums.length) {
    recommend.fetchUserAlbums()
  }
})

// 监听同步完成后的歌单变更事件
let unlistenPlaylistsChanged: UnlistenFn | null = null
onMounted(async () => {
  unlistenPlaylistsChanged = await listen('playlists-changed', () => {
    loadPlaylists()
  })
})
onUnmounted(() => {
  unlistenPlaylistsChanged?.()
})
</script>

<template>
  <div class="library-view">
    <header class="lib-header">
      <h1 class="page-title">{{ t('library.title') }}</h1>
      <button class="header-action">
        <span class="material-symbols-rounded">sort</span>
      </button>
    </header>

    <div class="tab-bar">
      <button
        v-for="(tab, i) in tabs"
        :key="tab.label"
        class="tab-chip"
        :class="{ active: activeTab === i }"
        @click="activeTab = i"
      >
        <span class="material-symbols-rounded" :class="{ filled: activeTab === i }" style="font-size: 18px">{{ tab.icon }}</span>
        <span>{{ tab.label }}</span>
      </button>
    </div>

    <!-- Tab: 本地 -->
    <div v-if="activeTab === 0" class="playlist-list">
      <div class="new-playlist-row" :class="{ disabled: library.isScanning }" @click="selectAndScanLocalMusic">
        <span class="material-symbols-rounded" :class="{ spinning: library.isScanning }" style="font-size: 20px">
          {{ library.isScanning ? 'progress_activity' : 'folder_search' }}
        </span>
        <div class="new-playlist-copy">
          <span>{{ library.isScanning ? t('library.scanning') : t('library.scan_local_music') }}</span>
          <small v-if="library.lastScanDir" class="new-playlist-sub">{{ library.lastScanDir }}</small>
        </div>
      </div>
      <!-- 新建歌单（对齐 Android：+ 新建歌单 行） -->
      <div class="new-playlist-row" @click="openCreateDialog">
        <span class="material-symbols-rounded" style="font-size: 20px">add</span>
        <span>{{ t('library.create_playlist') }}</span>
      </div>
      <div class="list-divider" />
      <p v-if="library.scanError" class="scan-error">{{ t('library.scan_failed') }}</p>

      <!-- 歌单列表 -->
      <div v-for="pl in playlists" :key="pl.id" class="playlist-item" @click="router.push({ name: 'local-playlist', params: { id: pl.id } })">
        <div class="pl-icon" :class="{ 'has-cover': pl.cover_url }">
          <img v-if="pl.cover_url" :src="pl.cover_url" referrerpolicy="no-referrer" class="pl-cover-img" @error="($event.target as HTMLImageElement).style.display = 'none'" />
          <span v-else class="material-symbols-rounded filled" style="font-size: 22px">queue_music</span>
        </div>
        <div class="pl-info">
          <div class="pl-name">{{ displayName(pl) }}</div>
          <div class="pl-count">{{ t('player.track_count', { count: pl.track_count }) }}</div>
        </div>
        <!-- 受保护歌单不显示三点菜单 -->
        <button v-if="!isProtectedPlaylist(pl)" class="pl-more" @click.stop="openContextMenu($event, pl)">
          <span class="material-symbols-rounded" style="font-size: 20px">more_vert</span>
        </button>
      </div>

      <div v-if="playlists.length === 0" class="empty-tab">
        <div class="empty-circle"><span class="material-symbols-rounded" style="font-size: 40px">queue_music</span></div>
        <p class="empty-title">{{ t('library.playlist_empty_title') }}</p>
        <p class="empty-desc">{{ t('library.playlist_empty_desc') }}</p>
      </div>
    </div>

    <!-- Tab: 收藏（同步的收藏歌单） -->
    <div v-else-if="activeTab === 1" class="playlist-list">
      <template v-if="favoritePlaylists.length > 0">
        <div
          v-for="fpl in favoritePlaylists"
          :key="'fav-' + fpl.id"
          class="playlist-item"
        >
          <div class="pl-icon has-cover" v-if="fpl.coverUrl">
            <img :src="fpl.coverUrl" referrerpolicy="no-referrer" class="pl-cover-img" />
          </div>
          <div class="pl-icon" v-else>
            <span class="material-symbols-rounded filled" style="font-size: 22px">bookmark</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ fpl.name }}</div>
            <div class="pl-count">{{ t('player.track_count', { count: fpl.trackCount }) }} · {{ platformLabel(fpl.source) }}</div>
          </div>
          <span class="material-symbols-rounded" style="font-size: 18px; opacity: 0.3">chevron_right</span>
        </div>
      </template>
      <div v-else class="empty-tab">
        <div class="empty-circle"><span class="material-symbols-rounded" style="font-size: 40px">bookmark</span></div>
        <p class="empty-title">{{ t('explore.no_playlists') }}</p>
        <p class="empty-desc">{{ t('explore.login_for_playlists') }}</p>
      </div>
    </div>

    <!-- Tab: 下载 -->
    <div v-else-if="activeTab === 2" class="playlist-list">
      <template v-if="downloadStore.activeDownloads.length > 0">
        <div class="subsection-label">
          <span class="material-symbols-rounded" style="font-size: 18px">downloading</span>
          <span>{{ t('download.active_tasks', { count: downloadStore.activeDownloads.length }) }}</span>
        </div>
        <div
          v-for="task in downloadStore.activeDownloads"
          :key="'active-' + task.trackId"
          class="playlist-item active-download-item"
        >
          <div class="pl-icon">
            <span class="material-symbols-rounded filled" style="font-size: 22px">
              {{ task.status === 'resolving' ? 'network_node' : task.status === 'error' ? 'error' : task.status === 'cancelled' ? 'cancel' : 'downloading' }}
            </span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ task.title }}</div>
            <div class="pl-count">{{ task.artist }} · {{ activeDownloadProgressText(task) }}</div>
            <div
              class="download-progress-bar"
              :class="{
                indeterminate: task.status === 'downloading' && !task.totalBytes,
                error: task.status === 'error',
                muted: task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'already_exists',
              }"
            >
              <div
                class="download-progress-fill"
                :style="{ width: `${Math.max(4, task.status === 'cancelled' || task.status === 'already_exists' ? 100 : task.progress ?? 0)}%` }"
              />
            </div>
          </div>
          <button
            class="pl-more danger"
            :disabled="task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'error' || task.status === 'already_exists'"
            @click.stop="cancelActiveDownload(task.trackId)"
          >
            <span class="material-symbols-rounded" style="font-size: 20px">close</span>
          </button>
        </div>
        <div v-if="downloadStore.downloads.length > 0" class="subsection-label" style="margin-top: 10px;">
          <span class="material-symbols-rounded" style="font-size: 18px">download_done</span>
          <span>{{ t('download.downloaded_items') }}</span>
        </div>
      </template>

      <template v-if="downloadStore.downloads.length > 0">
        <div
          v-for="dl in downloadStore.downloads"
          :key="'dl-' + dl.id"
          class="playlist-item"
          @click="playDownloadedTrack(dl)"
        >
          <div class="pl-icon has-cover" v-if="dl.coverUrl">
            <img :src="dl.coverUrl" referrerpolicy="no-referrer" class="pl-cover-img" />
          </div>
          <div class="pl-icon" v-else>
            <span class="material-symbols-rounded filled" style="font-size: 22px">music_note</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ dl.title }}</div>
            <div class="pl-count">{{ dl.artist }} · {{ formatFileSize(dl.fileSize) }} · {{ platformLabel(dl.source) }}</div>
          </div>
          <button class="pl-more" @click.stop="openDlContextMenu($event, dl)">
            <span class="material-symbols-rounded" style="font-size: 20px">more_vert</span>
          </button>
        </div>
      </template>
      <div v-else-if="downloadStore.activeDownloads.length === 0" class="empty-tab">
        <div class="empty-circle"><span class="material-symbols-rounded" style="font-size: 40px">download</span></div>
        <p class="empty-title">{{ t('library.downloads_empty_title') }}</p>
        <p class="empty-desc">{{ t('library.downloads_empty_desc') }}</p>
      </div>
    </div>

    <!-- Tab: 网易云-歌单 -->
    <div v-else-if="activeTab === 3" class="playlist-list">
      <template v-if="neteasePlaylists.length > 0">
        <div
          v-for="npl in neteasePlaylists"
          :key="'ne-' + npl.id"
          class="playlist-item"
          @click="router.push({ name: 'netease-playlist', params: { id: npl.id } })"
        >
          <div class="pl-icon netease">
            <img v-if="npl.coverUrl" :src="npl.coverUrl" referrerpolicy="no-referrer" class="pl-cover-img" />
            <span v-else class="material-symbols-rounded filled" style="font-size: 22px">queue_music</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ npl.name }}</div>
            <div class="pl-count">{{ t('library.track_count', { count: npl.trackCount || 0 }) }}</div>
          </div>
          <span class="material-symbols-rounded" style="font-size: 18px; opacity: 0.3">chevron_right</span>
        </div>
      </template>
      <div v-else class="empty-tab">
        <div class="empty-circle"><span class="material-symbols-rounded" style="font-size: 40px">cloud_queue</span></div>
        <p class="empty-title">{{ t('explore.no_playlists') }}</p>
        <p class="empty-desc">{{ t('explore.login_for_playlists') }}</p>
      </div>
    </div>

    <!-- Tab: Bili 收藏夹 -->
    <div v-else-if="activeTab === 4" class="playlist-list">
      <template v-if="biliPlaylists.length > 0">
        <div class="platform-summary bilibili">
          <span class="platform-icon-mask" style="mask-image: url('/icons/ic_bilibili.svg')"></span>
          <div>
            <div class="platform-title">{{ t('library.bilibili_favorites') }}</div>
            <div class="platform-desc">{{ t('player.video_count', { count: biliPlaylists.reduce((sum, p) => sum + (p.trackCount || 0), 0) }) }}</div>
          </div>
        </div>
        <div
          v-for="bpl in biliPlaylists"
          :key="'bili-' + bpl.id"
          class="playlist-item"
          @click="router.push({ name: 'bili-playlist', params: { mediaId: bpl.id } })"
        >
          <div class="pl-icon bilibili" :class="{ 'has-cover': bpl.coverUrl }">
            <img v-if="bpl.coverUrl" :src="bpl.coverUrl" referrerpolicy="no-referrer" class="pl-cover-img" />
            <span v-else class="material-symbols-rounded filled" style="font-size: 22px">video_library</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ bpl.name }}</div>
            <div class="pl-count">{{ t('player.video_count', { count: bpl.trackCount || 0 }) }}</div>
          </div>
          <span class="material-symbols-rounded" style="font-size: 18px; opacity: 0.3">chevron_right</span>
        </div>
      </template>
      <div v-else class="empty-tab">
        <div class="empty-circle platform-empty bilibili"><span class="platform-icon-mask" style="mask-image: url('/icons/ic_bilibili.svg')"></span></div>
        <p class="empty-title">{{ t('library.bilibili_favorites') }}</p>
        <p class="empty-desc">{{ auth.bilibili.loggedIn ? t('explore.no_playlists') : t('explore.login_for_playlists') }}</p>
      </div>
    </div>

    <!-- Tab: YouTube Music 歌单 -->
    <div v-else-if="activeTab === 5" class="playlist-list">
      <template v-if="youtubePlaylists.length > 0">
        <div class="platform-summary youtube">
          <span class="platform-icon-mask" style="mask-image: url('/icons/ic_youtube.svg')"></span>
          <div>
            <div class="platform-title">YouTube Music</div>
            <div class="platform-desc">{{ t('player.track_count', { count: youtubePlaylists.length }) }}</div>
          </div>
        </div>
        <div
          v-for="ypl in youtubePlaylists"
          :key="'yt-' + ypl.id"
          class="playlist-item"
          @click="router.push({ name: 'youtube-playlist', params: { browseId: ypl.id } })"
        >
          <div class="pl-icon youtube" :class="{ 'has-cover': ypl.coverUrl }">
            <img v-if="ypl.coverUrl" :src="ypl.coverUrl" referrerpolicy="no-referrer" class="pl-cover-img" />
            <span v-else class="material-symbols-rounded filled" style="font-size: 22px">subscriptions</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ ypl.name }}</div>
            <div class="pl-count">{{ ypl.description || 'YouTube Music' }}</div>
          </div>
          <span class="material-symbols-rounded" style="font-size: 18px; opacity: 0.3">chevron_right</span>
        </div>
      </template>
      <div v-else class="empty-tab">
        <div class="empty-circle platform-empty youtube"><span class="platform-icon-mask" style="mask-image: url('/icons/ic_youtube.svg')"></span></div>
        <p class="empty-title">YouTube Music</p>
        <p class="empty-desc">{{ auth.youtube.loggedIn ? t('explore.no_playlists') : t('explore.login_for_playlists') }}</p>
      </div>
    </div>

    <!-- Tab: 网易云-专辑 -->
    <div v-else-if="activeTab === 6" class="playlist-list">
      <div v-if="recommend.userAlbums.length > 0">
        <div
          v-for="album in recommend.userAlbums"
          :key="album.id"
          class="playlist-item"
          @click="router.push({ path: '/netease-playlist/' + album.id, query: { isAlbum: '1', name: album.name } })"
        >
          <div class="pl-icon has-cover">
            <img
              v-if="album.coverUrl"
              :src="album.coverUrl"
              class="pl-cover-img"
              loading="lazy"
              referrerpolicy="no-referrer"
            />
            <span v-else class="material-symbols-rounded filled" style="font-size: 22px">album</span>
          </div>
          <div class="pl-info">
            <div class="pl-name">{{ album.name }}</div>
            <div class="pl-count">{{ album.artist }} · {{ t('player.track_count', { count: album.trackCount }) }}</div>
          </div>
          <span class="material-symbols-rounded" style="font-size: 18px; opacity: 0.3">chevron_right</span>
        </div>
      </div>
      <div v-else class="empty-tab">
        <div class="empty-circle"><span class="material-symbols-rounded" style="font-size: 40px">album</span></div>
        <p class="empty-title">{{ t('library.empty_title', { type: t('library.albums') }) }}</p>
        <p class="empty-desc">{{ t('library.empty_desc') }}</p>
      </div>
    </div>

    <!-- 下载项上下文菜单 -->
    <Teleport to="body">
      <div v-if="dlContextMenu.show" class="context-overlay" @click="closeDlContextMenu" @contextmenu.prevent="closeDlContextMenu">
        <div class="context-menu" :style="{ left: dlContextMenu.x + 'px', top: dlContextMenu.y + 'px' }">
          <button class="ctx-item" @click="revealDownloadFile(dlContextMenu.track!)">
            <span class="material-symbols-rounded" style="font-size: 20px">folder_open</span>
            <span>{{ t('download.open_folder') }}</span>
          </button>
          <button
            class="ctx-item"
            :disabled="isDownloadedTrackInUse(dlContextMenu.track)"
            @click="redownloadDownloadedTrack(dlContextMenu.track!)"
          >
            <span class="material-symbols-rounded" style="font-size: 20px">refresh</span>
            <span>{{ t('download.redownload') }}</span>
          </button>
          <button
            class="ctx-item danger"
            :disabled="isDownloadedTrackInUse(dlContextMenu.track)"
            @click="requestDlDelete(dlContextMenu.track!)"
          >
            <span class="material-symbols-rounded" style="font-size: 20px">delete</span>
            <span>{{ t('common.delete') }}</span>
          </button>
        </div>
      </div>
    </Teleport>

    <!-- 删除下载确认对话框 -->
    <M3Dialog
      v-model:open="showDlDeleteDialog"
      :title="t('download.delete_confirm')"
      icon="delete"
      :confirm-text="t('common.delete')"
      confirm-danger
      @confirm="confirmDlDelete"
    >
      <p class="dialog-msg">{{ t('library.delete_confirm_msg', { name: dlDeleteTarget?.title || '' }) }}</p>
    </M3Dialog>

    <!-- 创建播放列表对话框 -->
    <M3Dialog
      v-model:open="showCreateDialog"
      :title="t('library.create_playlist')"
      icon="playlist_add"
      :confirm-text="t('library.create_playlist')"
      :confirm-disabled="!newPlaylistName.trim()"
      @confirm="confirmCreate"
    >
      <M3Input
        ref="inputRef"
        v-model="newPlaylistName"
        :placeholder="t('library.playlist_name_placeholder')"
        :maxlength="50"
        @enter="confirmCreate"
      />
    </M3Dialog>

    <!-- 上下文菜单 -->
    <Teleport to="body">
      <div v-if="contextMenu.show" class="context-overlay" @click="closeContextMenu" @contextmenu.prevent="closeContextMenu">
        <div class="context-menu" :style="{ left: contextMenu.x + 'px', top: contextMenu.y + 'px' }">
          <button class="ctx-item" @click="requestRename(contextMenu.playlist!)">
            <span class="material-symbols-rounded" style="font-size: 20px">edit</span>
            <span>{{ t('library.rename_playlist') }}</span>
          </button>
          <button v-if="!isProtectedPlaylist(contextMenu.playlist!)" class="ctx-item danger" @click="requestDelete(contextMenu.playlist!)">
            <span class="material-symbols-rounded" style="font-size: 20px">delete</span>
            <span>{{ t('library.delete_playlist') }}</span>
          </button>
        </div>
      </div>
    </Teleport>

    <!-- 删除确认对话框 -->
    <M3Dialog
      v-model:open="showDeleteDialog"
      :title="t('library.delete_confirm_title')"
      icon="delete"
      :confirm-text="t('library.delete_playlist')"
      confirm-danger
      @confirm="confirmDelete"
    >
      <p class="dialog-msg">{{ t('library.delete_confirm_msg', { name: deleteTarget ? displayName(deleteTarget) : '' }) }}</p>
    </M3Dialog>

    <!-- 重命名对话框 -->
    <M3Dialog
      v-model:open="showRenameDialog"
      :title="t('library.rename_playlist')"
      icon="edit"
      :confirm-text="t('common.save')"
      :confirm-disabled="!renameValue.trim() || renameValue.trim() === renameTarget?.name"
      @confirm="confirmRename"
    >
      <M3Input
        ref="renameInputRef"
        v-model="renameValue"
        :placeholder="t('library.rename_placeholder')"
        :maxlength="50"
        @enter="confirmRename"
      />
    </M3Dialog>
  </div>
</template>

<style scoped lang="scss">
.library-view { padding: 20px 28px 32px; }

.lib-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 20px;
}

.page-title {
  font-size: 28px;
  font-weight: 700;
  letter-spacing: -0.5px;
}

.header-action {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container-high); }
}

/* M3 Filter Chips */
.tab-bar {
  display: flex;
  gap: 8px;
  margin-bottom: 20px;
}

.tab-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 7px 16px;
  border-radius: var(--radius-full);
  font-size: 13px;
  font-weight: 500;
  color: var(--md-on-surface-variant);
  background: transparent;
  border: 1px solid var(--md-outline-variant);
  transition: all var(--duration-short) var(--ease-standard);

  &:hover:not(.active) {
    background: var(--md-surface-container);
  }

  &.active {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
    border-color: transparent;
    font-weight: 600;
  }
}

/* 新建歌单行（对齐 Android） */
.new-playlist-row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 12px;
  font-size: 14px;
  font-weight: 600;
  color: var(--md-on-surface-variant);
  cursor: pointer;
  border-radius: var(--radius-md);
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container); }

  &.disabled {
    opacity: 0.6;
    cursor: progress;
    pointer-events: none;
  }
}

.new-playlist-copy {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.new-playlist-sub {
  max-width: 100%;
  font-size: 11px;
  line-height: 1.2;
  color: var(--md-on-surface-variant);
  opacity: 0.75;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.scan-error {
  margin: 6px 12px 2px;
  font-size: 12px;
  color: var(--md-error);
}

.spinning {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.list-divider {
  height: 1px;
  background: var(--md-outline-variant);
  opacity: 0.3;
  margin: 4px 12px;
}

/* 播放列表 */
.playlist-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.subsection-label {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 12px 6px;
  font-size: 12px;
  font-weight: 600;
  color: var(--md-on-surface-variant);
}

.playlist-item {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 10px 12px;
  border-radius: var(--radius-md);
  cursor: pointer;
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container); }
}

.new-playlist {
  margin-bottom: 4px;

  .pl-name {
    color: var(--md-primary);
    font-weight: 600;
  }
}

.system-playlist {
  .pl-name { font-weight: 600; }
}

.pl-icon {
  width: 48px;
  height: 48px;
  border-radius: var(--radius-md);
  background: var(--md-surface-container-high);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  color: var(--md-on-surface-variant);

  &.create {
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
  }

  &.favorite {
    background: var(--md-tertiary-container);
    color: var(--md-on-tertiary-container);
  }

  &.local-files {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }

  &.has-cover {
    overflow: hidden;
    background: var(--md-surface-container-highest);
  }
}

.pl-info { flex: 1; min-width: 0; }

.pl-name {
  font-size: 14px;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pl-count {
  font-size: 12px;
  color: var(--md-on-surface-variant);
  margin-top: 2px;
}

/* 歌单封面图 */
.pl-cover-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: inherit;
}

.pl-icon.netease {
  background: #e74c3c20;
  overflow: hidden;
}

.pl-icon.bilibili {
  background: #00a1d620;
  overflow: hidden;
}

.pl-icon.youtube {
  background: #ff003320;
  overflow: hidden;
}


.platform-summary {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 16px;
  margin-bottom: 8px;
  border-radius: 20px;
  border: 1px solid var(--md-outline-variant);
  background:
    radial-gradient(circle at 12% 20%, color-mix(in srgb, var(--platform-color) 22%, transparent), transparent 34%),
    var(--md-surface-container);

  &.bilibili { --platform-color: #00a1d6; }
  &.youtube { --platform-color: #ff0033; }
}

.platform-icon-mask {
  display: block;
  width: 26px;
  height: 26px;
  background: var(--platform-color, var(--md-primary));
  mask-size: contain;
  mask-repeat: no-repeat;
  mask-position: center;
  flex-shrink: 0;
}

.platform-title {
  font-size: 14px;
  font-weight: 800;
}

.platform-desc {
  margin-top: 2px;
  font-size: 12px;
  color: var(--md-on-surface-variant);
}

.empty-circle.platform-empty {
  opacity: 0.85;

  &.bilibili { --platform-color: #00a1d6; }
  &.youtube { --platform-color: #ff0033; }

  .platform-icon-mask {
    width: 40px;
    height: 40px;
  }
}


/* 分组分割线 */
.section-divider {
  display: flex;
  align-items: center;
  padding: 16px 12px 8px;
  gap: 10px;
}

.divider-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--md-on-surface-variant);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  white-space: nowrap;
}

.section-divider::after {
  content: '';
  flex: 1;
  height: 1px;
  background: var(--md-outline-variant);
  opacity: 0.4;
}

.pl-more {
  width: 36px;
  height: 36px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  opacity: 0;
  transition: opacity var(--duration-short), background var(--duration-short);

  .playlist-item:hover & { opacity: 1; }
  &:hover { background: var(--md-surface-container-high); }

  &.danger {
    opacity: 1;
    color: var(--md-error);
  }
}

.active-download-item {
  cursor: default;
}

.download-progress-bar {
  position: relative;
  height: 6px;
  margin-top: 8px;
  border-radius: 999px;
  overflow: hidden;
  background: var(--md-surface-container-highest);
}

.download-progress-fill {
  height: 100%;
  min-width: 4px;
  border-radius: inherit;
  background: linear-gradient(90deg, var(--md-primary), color-mix(in srgb, var(--md-primary) 70%, white));
  transition: width 180ms ease;
}

.download-progress-bar.indeterminate .download-progress-fill {
  width: 36% !important;
  animation: download-indeterminate 1.2s ease-in-out infinite;
}

.download-progress-bar.error .download-progress-fill {
  background: var(--md-error);
}

.download-progress-bar.muted .download-progress-fill {
  background: var(--md-outline);
}

@keyframes download-indeterminate {
  0% {
    transform: translateX(-100%);
  }
  100% {
    transform: translateX(280%);
  }
}

/* 空状态 */
.empty-tab {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 80px 0;
}

.empty-circle {
  width: 80px;
  height: 80px;
  border-radius: var(--radius-full);
  background: var(--md-surface-container);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  margin-bottom: 20px;
  opacity: 0.5;
}

.empty-title {
  font-size: 16px;
  font-weight: 600;
  color: var(--md-on-surface-variant);
  margin-bottom: 4px;
}

.empty-desc {
  font-size: 13px;
  color: var(--md-on-surface-variant);
  opacity: 0.5;
}

/* 上下文菜单 — 样式移至 non-scoped block（Teleport to body） */

/* 对话框描述文本 */
.dialog-msg {
  font-size: 14px;
  color: var(--md-on-surface-variant);
  line-height: 1.5;
}
</style>

<!-- 非 scoped 样式：Teleport 渲染到 body 的上下文菜单 -->
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
  &:disabled {
    opacity: 0.45;
    cursor: not-allowed;
    pointer-events: none;
  }

  &.danger { color: var(--md-error); }
  &.danger:hover { background: color-mix(in srgb, var(--md-error) 8%, transparent); }
}
</style>
