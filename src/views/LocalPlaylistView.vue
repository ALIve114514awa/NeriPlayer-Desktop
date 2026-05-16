<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { usePlayerStore, type TrackInfo, normalizeTrack, displayAlbum } from '@/stores/player'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import M3Dialog from '@/components/ui/M3Dialog.vue'
import AddToPlaylistDialog from '@/components/AddToPlaylistDialog.vue'

const route = useRoute()
const router = useRouter()
const player = usePlayerStore()
const { t } = useI18n()

const isLoading = ref(true)
const error = ref<string | null>(null)
const playlistName = ref('')
const searchQuery = ref('')

const tracks = ref<TrackInfo[]>([])

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

  isLoading.value = true
  error.value = null

  try {
    const playlists = await invoke<{ id: number; name: string }[]>('list_playlists')
    const pl = playlists.find(p => p.id === id)
    playlistName.value = pl?.name || ''

    const trackList = await invoke<any[]>('get_playlist_tracks', { id })
    // 倒序显示（对齐 Android：最新添加的在最前面）
    tracks.value = trackList.map(normalizeTrack).reverse()
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

function openTrackMenu(e: MouseEvent, track: TrackInfo, index: number) {
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 184
  let x = rect.left - menuWidth - 4
  let y = rect.top
  if (x < 8) x = rect.right + 4
  if (x + menuWidth > window.innerWidth - 8) x = window.innerWidth - menuWidth - 8
  if (y + menuHeight > window.innerHeight - 8) y = window.innerHeight - menuHeight - 8

  trackMenu.value = { show: true, x, y, track, index }
}

function openTrackContextMenu(e: MouseEvent, track: TrackInfo, index: number) {
  const menuWidth = 200
  const menuHeight = 184
  trackMenu.value = {
    show: true,
    x: Math.max(8, Math.min(e.clientX, window.innerWidth - menuWidth - 8)),
    y: Math.max(8, Math.min(e.clientY, window.innerHeight - menuHeight - 8)),
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
  showAddToPlaylist.value = true
}

// 删除确认
const showRemoveDialog = ref(false)
const removeTarget = ref<TrackInfo | null>(null)

function requestRemove(track: TrackInfo) {
  closeTrackMenu()
  removeTarget.value = track
  showRemoveDialog.value = true
}

async function confirmRemove() {
  if (!removeTarget.value) return
  const id = Number(route.params.id)
  try {
    await invoke('remove_from_playlist', { playlistId: id, trackId: removeTarget.value.id })
    tracks.value = tracks.value.filter(t => t.id !== removeTarget.value!.id)
  } catch (e) {
    console.error('Remove failed:', e)
  }
  showRemoveDialog.value = false
  removeTarget.value = null
}

function addToQueueNext(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueNext(track)
}

function addToQueueEnd(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueEnd(track)
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
  player.clearQueue()
  for (const t of filteredTracks.value) {
    player.addToQueueEnd(t)
  }
  player.play(track)
}

// 歌单封面：取第一首有 cover 的曲目
const playlistCover = computed(() => {
  for (const t of tracks.value) {
    if (t.coverUrl) return t.coverUrl
  }
  return ''
})

onMounted(loadDetail)
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
          <img v-if="playlistCover" :src="playlistCover" referrerpolicy="no-referrer" />
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
          @contextmenu.prevent.stop="openTrackContextMenu($event, track, index)"
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
            <div class="track-meta">{{ track.artist }}<template v-if="track.album"> · {{ displayAlbum(track.album) }}</template></div>
          </div>
          <div class="track-duration">{{ formatDuration(track.durationMs) }}</div>
          <button class="track-more" @click.stop="openTrackMenu($event, track, index)">
            <span class="material-symbols-rounded">more_vert</span>
          </button>
        </div>
      </div>
    </template>

    <!-- 曲目右键菜单 -->
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
          <button class="ctx-item danger" @click="requestRemove(trackMenu.track!)">
            <span class="material-symbols-rounded" style="font-size: 20px">delete</span>
            <span>{{ t('library.remove_from_playlist') }}</span>
          </button>
        </div>
      </div>
    </Teleport>

    <!-- 删除确认对话框 -->
    <M3Dialog
      v-model:open="showRemoveDialog"
      :title="t('library.remove_from_playlist')"
      icon="delete"
      :confirm-text="t('library.remove_from_playlist')"
      confirm-danger
      @confirm="confirmRemove"
    >
      <p class="dialog-msg">{{ t('library.remove_confirm_msg', { name: removeTarget?.title || '' }) }}</p>
    </M3Dialog>

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

.dialog-msg {
  font-size: 14px;
  color: var(--md-on-surface-variant);
  line-height: 1.5;
}
</style>

<!-- Teleport 菜单样式 -->
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
