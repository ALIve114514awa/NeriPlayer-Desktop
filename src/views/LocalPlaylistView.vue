<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  usePlayerStore,
  type TrackInfo,
  normalizeTrack,
  displayAlbum,
  tracePlaybackUi,
} from '@/stores/player'
import { useDownloadStore } from '@/stores/download'
import { useToastStore } from '@/stores/toast'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import M3Dialog from '@/components/ui/M3Dialog.vue'
import AddToPlaylistDialog from '@/components/AddToPlaylistDialog.vue'
import BilibiliCoverImage from '@/components/BilibiliCoverImage.vue'
import ContextMenu from '@/components/ui/ContextMenu.vue'
import {
  createContextMenuItem,
  type ContextMenuActionItem,
  type ContextMenuItem,
} from '@/utils/contextMenu'

const route = useRoute()
const router = useRouter()
const player = usePlayerStore()
const downloadStore = useDownloadStore()
const toast = useToastStore()
const { t } = useI18n()

const isLoading = ref(true)
const error = ref<string | null>(null)
const playlistName = ref('')
const searchQuery = ref('')
const selectionMode = ref(false)
const selectedIds = ref<Set<string>>(new Set())
const isBatchRemoving = ref(false)

const tracks = ref<TrackInfo[]>([])

function trackSelectionKey(track: TrackInfo): string {
  return track.playlistKey || `${track.id}|${track.album}`
}

const filteredTracks = computed(() => {
  if (!searchQuery.value) return tracks.value
  const q = searchQuery.value.toLowerCase()
  return tracks.value.filter(t =>
    t.title.toLowerCase().includes(q)
    || t.artist.toLowerCase().includes(q)
    || displayAlbum(t.album || '').toLowerCase().includes(q)
  )
})

const selectedTracks = computed(() => tracks.value.filter(t => selectedIds.value.has(trackSelectionKey(t))))
const selectedCount = computed(() => selectedIds.value.size)
const visibleSelectedCount = computed(() => filteredTracks.value.filter(t => selectedIds.value.has(trackSelectionKey(t))).length)
const allVisibleSelected = computed(() => filteredTracks.value.length > 0 && visibleSelectedCount.value === filteredTracks.value.length)

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

  isLoading.value = true
  error.value = null

  try {
    const playlists = await invoke<{ id: number; name: string }[]>('list_playlists')
    const pl = playlists.find(p => p.id === id)
    playlistName.value = pl?.name || ''

    const trackList = await invoke<any[]>('get_playlist_tracks', { id })
    tracks.value = trackList.map(normalizeTrack)
  } catch (e: any) {
    error.value = e?.toString() || t('player.load_failed')
  } finally {
    isLoading.value = false
  }
}

// 右键/三点菜单
const trackMenu = ref<{ show: boolean; x: number; y: number; track: TrackInfo | null; index: number }>({
  show: false, x: 0, y: 0, track: null, index: -1,
})
const showAddToPlaylist = ref(false)
const addToPlaylistTarget = ref<TrackInfo | null>(null)
const addToPlaylistTargets = ref<TrackInfo[]>([])

function openTrackMenu(e: MouseEvent, track: TrackInfo, index: number) {
  if (selectionMode.value) {
    toggleSelected(trackSelectionKey(track))
    return
  }
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  let x = rect.left - 204
  if (x < 8) x = rect.right + 4
  trackMenu.value = { show: true, x, y: rect.top, track, index }
}

function openTrackContextMenu(e: MouseEvent, track: TrackInfo, index: number) {
  if (selectionMode.value) {
    toggleSelected(trackSelectionKey(track))
    return
  }
  trackMenu.value = {
    show: true,
    x: e.clientX,
    y: e.clientY,
    track,
    index,
  }
}

function closeTrackMenu() {
  trackMenu.value.show = false
}

function openAddToPlaylist(track: TrackInfo) {
  closeTrackMenu()
  addToPlaylistTarget.value = track
  addToPlaylistTargets.value = []
  showAddToPlaylist.value = true
}

function openBatchAddToPlaylist() {
  if (selectedTracks.value.length === 0) return
  closeTrackMenu()
  addToPlaylistTarget.value = null
  addToPlaylistTargets.value = selectedTracks.value
  showAddToPlaylist.value = true
}

// 删除确认
const showRemoveDialog = ref(false)
const removeTarget = ref<TrackInfo | null>(null)
const removeMode = ref<'single' | 'batch'>('single')

function requestRemove(track: TrackInfo) {
  closeTrackMenu()
  removeMode.value = 'single'
  removeTarget.value = track
  showRemoveDialog.value = true
}

function requestBatchRemove() {
  if (selectedCount.value === 0) return
  closeTrackMenu()
  removeMode.value = 'batch'
  removeTarget.value = null
  showRemoveDialog.value = true
}

async function confirmRemove() {
  const id = Number(route.params.id)
  try {
    if (removeMode.value === 'batch') {
      isBatchRemoving.value = true
      const ids = [...selectedIds.value]
      await invoke('remove_tracks_from_playlist', { playlistId: id, trackIds: ids })
      tracks.value = tracks.value.filter(t => !selectedIds.value.has(trackSelectionKey(t)))
      toast.success(`已移除 ${ids.length} 首歌曲`)
      leaveSelectionMode()
    } else if (removeTarget.value) {
      const trackKey = trackSelectionKey(removeTarget.value)
      await invoke('remove_from_playlist', { playlistId: id, trackId: trackKey })
      tracks.value = tracks.value.filter(t => trackSelectionKey(t) !== trackKey)
    }
  } catch (e) {
    console.error('Remove failed:', e)
  } finally {
    isBatchRemoving.value = false
    showRemoveDialog.value = false
    removeTarget.value = null
  }
}

function addToQueueNext(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueNext(track)
}

function addToQueueEnd(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueEnd(track)
}

const trackMenuItems = computed<ContextMenuItem[]>(() => [
  createContextMenuItem(t('common.multi_select'), { id: 'select', icon: 'checklist' }),
  createContextMenuItem(t('player.play_next'), { id: 'play-next', icon: 'queue_play_next' }),
  createContextMenuItem(t('player.add_to_queue'), { id: 'add-to-queue', icon: 'add_to_queue' }),
  createContextMenuItem(t('player.add_to_playlist'), { id: 'add-to-playlist', icon: 'playlist_add' }),
  createContextMenuItem(t('library.remove_from_playlist'), {
    id: 'remove-from-playlist',
    icon: 'delete',
    danger: true,
  }),
])

function handleTrackMenuClick(item: ContextMenuActionItem) {
  const track = trackMenu.value.track
  if (!track) return

  switch (item.id) {
    case 'select':
      closeTrackMenu()
      enterSelectionMode(track)
      break
    case 'play-next':
      addToQueueNext(track)
      break
    case 'add-to-queue':
      addToQueueEnd(track)
      break
    case 'add-to-playlist':
      openAddToPlaylist(track)
      break
    case 'remove-from-playlist':
      requestRemove(track)
      break
  }
}

function enterSelectionMode(track?: TrackInfo) {
  selectionMode.value = true
  if (track) selectedIds.value = new Set(selectedIds.value).add(trackSelectionKey(track))
}

function leaveSelectionMode() {
  selectionMode.value = false
  selectedIds.value = new Set()
}

function toggleSelected(trackId: string) {
  const next = new Set(selectedIds.value)
  if (next.has(trackId)) next.delete(trackId)
  else next.add(trackId)
  selectedIds.value = next
  if (selectionMode.value && next.size === 0) selectionMode.value = false
}

function toggleSelectAllVisible() {
  if (allVisibleSelected.value) {
    const next = new Set(selectedIds.value)
    for (const track of filteredTracks.value) next.delete(trackSelectionKey(track))
    selectedIds.value = next
    if (next.size === 0) selectionMode.value = false
    return
  }
  const next = new Set(selectedIds.value)
  for (const track of filteredTracks.value) next.add(trackSelectionKey(track))
  selectedIds.value = next
  if (next.size > 0) selectionMode.value = true
}

function playSelected() {
  if (selectedTracks.value.length === 0) return
  player.playAll(selectedTracks.value)
}

function addSelectedToQueueEnd() {
  for (const track of selectedTracks.value) player.addToQueueEnd(track)
  toast.success(`已添加 ${selectedTracks.value.length} 首到队列`)
}

function downloadSelected() {
  for (const track of selectedTracks.value) downloadStore.downloadTrack(track)
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
  tracePlaybackUi(
    'local_playlist_click',
    track,
    `selectionMode=${selectionMode.value}, filteredTracks=${filteredTracks.value.length}`,
  )
  if (selectionMode.value) {
    toggleSelected(trackSelectionKey(track))
    return
  }
  player.playAll(filteredTracks.value, track.id, trackSelectionKey(track))
}

// 歌单封面：取第一首有 cover 的曲目
const playlistCover = computed(() => {
  for (const t of tracks.value) {
    if (t.coverUrl) return t.coverUrl
  }
  return ''
})

onMounted(() => {
  loadDetail()
  downloadStore.initEvents()
  downloadStore.loadDownloads()
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
      <!-- Hero 封面 + 信息（对齐 NeteasePlaylistView） -->
      <div class="detail-hero">
        <div class="hero-cover">
          <BilibiliCoverImage v-if="playlistCover" :src="playlistCover">
            <span class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.3">queue_music</span>
          </BilibiliCoverImage>
          <span v-else class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.3">queue_music</span>
        </div>
        <div class="hero-info">
          <h1 class="hero-title">{{ playlistName }}</h1>
          <p class="hero-meta">
            {{ t('player.track_count', { count: tracks.length }) }} · {{ totalDuration }}
          </p>
          <div class="hero-actions" v-if="tracks.length > 0">
            <button class="play-all-btn" @click="playAll">
              <span class="material-symbols-rounded filled">play_arrow</span>
              {{ t('player.play_all') }}
            </button>
            <button class="hero-icon-btn" :title="t('player.shuffle_play')" @click="shufflePlay">
              <span class="material-symbols-rounded">shuffle</span>
            </button>
            <button class="hero-icon-btn" :title="t('common.multi_select')" @click="enterSelectionMode()">
              <span class="material-symbols-rounded">checklist</span>
            </button>
          </div>
        </div>
      </div>

      <div v-if="filteredTracks.length === 0" class="state-center">
        <p>{{ t('player.empty_playlist') }}</p>
      </div>
      <template v-else>
        <div v-if="selectionMode" class="selection-toolbar">
          <div class="selection-count">已选择 {{ selectedCount }} 首</div>
          <button class="selection-btn" @click="toggleSelectAllVisible">
            <span class="material-symbols-rounded">{{ allVisibleSelected ? 'deselect' : 'select_all' }}</span>
            {{ allVisibleSelected ? '取消全选' : '全选当前' }}
          </button>
          <button class="selection-btn" :disabled="selectedCount === 0" @click="playSelected">
            <span class="material-symbols-rounded filled">play_arrow</span>
            播放
          </button>
          <button class="selection-btn" :disabled="selectedCount === 0" @click="addSelectedToQueueEnd">
            <span class="material-symbols-rounded">add_to_queue</span>
            加到队尾
          </button>
          <button class="selection-btn" :disabled="selectedCount === 0" @click="openBatchAddToPlaylist">
            <span class="material-symbols-rounded">playlist_add</span>
            加到歌单
          </button>
          <button class="selection-btn" :disabled="selectedCount === 0" @click="downloadSelected">
            <span class="material-symbols-rounded">download</span>
            下载
          </button>
          <button class="selection-btn danger" :disabled="selectedCount === 0" @click="requestBatchRemove">
            <span class="material-symbols-rounded">delete</span>
            移除
          </button>
          <button class="selection-btn ghost" @click="leaveSelectionMode">取消</button>
        </div>
        <div class="track-list">
        <div
          v-for="(track, index) in filteredTracks"
          :key="trackSelectionKey(track)"
          class="track-item"
          :class="{ active: player.currentTrack && trackSelectionKey(player.currentTrack) === trackSelectionKey(track), selected: selectedIds.has(trackSelectionKey(track)), 'selection-mode': selectionMode }"
          @click="playTrack(track)"
          @contextmenu.prevent.stop="openTrackContextMenu($event, track, index)"
        >
          <button v-if="selectionMode" class="track-select" @click.stop="toggleSelected(trackSelectionKey(track))">
            <span class="material-symbols-rounded filled">{{ selectedIds.has(trackSelectionKey(track)) ? 'check_circle' : 'radio_button_unchecked' }}</span>
          </button>
          <div v-else class="track-index">
            <div v-if="player.currentTrack && trackSelectionKey(player.currentTrack) === trackSelectionKey(track) && player.isPlaying" class="equalizer-bars"><span class="bar"/><span class="bar"/><span class="bar"/></div>
            <span v-else class="index-num">{{ index + 1 }}</span>
          </div>
          <div class="track-cover">
            <BilibiliCoverImage v-if="track.coverUrl" :src="track.coverUrl" loading="lazy">
              <span class="material-symbols-rounded filled">music_note</span>
            </BilibiliCoverImage>
            <span v-else class="material-symbols-rounded filled">music_note</span>
          </div>
          <div class="track-info">
            <div class="track-title">{{ track.title }}</div>
            <div class="track-meta">{{ track.artist }}<template v-if="track.album"> · {{ displayAlbum(track.album) }}</template></div>
          </div>
          <div class="track-duration">{{ formatDuration(track.durationMs) }}</div>
          <button class="track-more" @click.stop="openTrackMenu($event, track, index)">
            <span class="material-symbols-rounded">more_vert</span>
          </button>
        </div>
        </div>
      </template>
    </template>

    <ContextMenu
      :open="trackMenu.show"
      :x="trackMenu.x"
      :y="trackMenu.y"
      :items="trackMenuItems"
      @update:open="trackMenu.show = $event"
      @click="handleTrackMenuClick"
    />

    <!-- 删除确认对话框 -->
    <M3Dialog
      v-model:open="showRemoveDialog"
      :title="removeMode === 'batch' ? '批量移除歌曲' : t('library.remove_from_playlist')"
      icon="delete"
      :confirm-text="removeMode === 'batch' ? '移除选中' : t('library.remove_from_playlist')"
      :confirm-disabled="isBatchRemoving"
      confirm-danger
      @confirm="confirmRemove"
    >
      <p class="dialog-msg">{{ removeMode === 'batch' ? `确定要从当前歌单移除选中的 ${selectedCount} 首歌曲吗？` : t('library.remove_confirm_msg', { name: removeTarget?.title || '' }) }}</p>
    </M3Dialog>

    <AddToPlaylistDialog v-model:open="showAddToPlaylist" :track="addToPlaylistTarget" :tracks="addToPlaylistTargets" />
  </div>
</template>

<style scoped lang="scss">
@use '@/styles/detail-view.scss' as *;


.selection-toolbar {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  padding: 10px 12px;
  margin: 0 0 10px;
  border-radius: 18px;
  background: color-mix(in srgb, var(--md-surface-container-high) 92%, transparent);
  border: 1px solid color-mix(in srgb, var(--md-primary) 18%, var(--md-outline-variant));
  backdrop-filter: blur(18px);
}
/* Linux WebKitGTK 无 backdrop-filter 时给不透明底色，避免列表穿透 */
@supports not ((backdrop-filter: blur(1px)) or (-webkit-backdrop-filter: blur(1px))) {
  .selection-toolbar {
    background: var(--md-surface-container-high);
  }
}

.selection-count {
  padding: 0 8px;
  margin-right: auto;
  font-size: 13px;
  font-weight: 700;
  color: var(--md-primary);
}

.selection-btn {
  height: 34px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 5px;
  padding: 0 12px;
  border-radius: var(--radius-full);
  background: var(--md-surface-container-highest);
  color: var(--md-on-surface-variant);
  font-size: 12px;
  font-weight: 700;
  transition: background var(--duration-short), color var(--duration-short), opacity var(--duration-short), transform var(--duration-short);

  .material-symbols-rounded { font-size: 18px; }
  &:hover:not(:disabled) { background: var(--md-secondary-container); color: var(--md-on-secondary-container); }
  &:active:not(:disabled) { transform: scale(0.97); }
  &:disabled { opacity: 0.42; cursor: not-allowed; }

  &.danger {
    color: var(--md-error);
    background: color-mix(in srgb, var(--md-error) 10%, transparent);
  }

  &.ghost {
    background: transparent;
  }
}

.track-select {
  width: 28px;
  height: 28px;
  border-radius: var(--radius-full);
  color: var(--md-primary);
  flex-shrink: 0;

  .material-symbols-rounded { font-size: 22px; }
  &:hover { background: color-mix(in srgb, var(--md-primary) 10%, transparent); }
}

:deep(.track-item.selected),
.track-item.selected {
  background: color-mix(in srgb, var(--md-primary) 14%, transparent);
}

.track-item.selection-mode .track-more {
  opacity: 0;
  pointer-events: none;
}

.track-item.selection-mode .track-duration {
  opacity: 0.45;
}

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

.dialog-msg {
  font-size: 14px;
  color: var(--md-on-surface-variant);
  line-height: 1.5;
}
</style>
