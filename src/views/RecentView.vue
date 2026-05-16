<script setup lang="ts">
import { ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import { usePlayerStore, type TrackInfo } from '@/stores/player'
import { useHistoryStore } from '@/stores/history'
import { useI18n } from 'vue-i18n'
import AddToPlaylistDialog from '@/components/AddToPlaylistDialog.vue'

const router = useRouter()
const player = usePlayerStore()
const history = useHistoryStore()
const { t } = useI18n()

const searchQuery = ref('')
const showClearConfirm = ref(false)
const trackMenu = ref<{ show: boolean; x: number; y: number; track: TrackInfo | null }>({
  show: false, x: 0, y: 0, track: null,
})
const showAddToPlaylist = ref(false)
const addToPlaylistTarget = ref<TrackInfo | null>(null)

const filteredEntries = computed(() => {
  if (!searchQuery.value) return history.entries
  const q = searchQuery.value.toLowerCase()
  return history.entries.filter(e =>
    e.track.title.toLowerCase().includes(q) ||
    e.track.artist.toLowerCase().includes(q) ||
    e.track.album.toLowerCase().includes(q)
  )
})

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  return `${Math.floor(s / 60)}:${(s % 60).toString().padStart(2, '0')}`
}

function formatRelativeTime(timestamp: number): string {
  const diff = Date.now() - timestamp
  const minutes = Math.floor(diff / 60000)
  if (minutes < 1) return t('recent.just_now')
  if (minutes < 60) return t('recent.minutes_ago', { count: minutes })
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return t('recent.hours_ago', { count: hours })
  const days = Math.floor(hours / 24)
  if (days < 7) return t('recent.days_ago', { count: days })
  return new Date(timestamp).toLocaleDateString()
}

function playAll() {
  const tracks = filteredEntries.value.map(e => e.track)
  if (tracks.length === 0) return
  player.playAll(tracks)
}

function shufflePlay() {
  const tracks = filteredEntries.value.map(e => e.track)
  if (tracks.length === 0) return
  player.shufflePlay(tracks)
}

function playEntry(index: number) {
  const tracks = filteredEntries.value.map(e => e.track)
  player.playAll(tracks)
  player.play(tracks[index])
}

function clearHistory() {
  history.clear()
  showClearConfirm.value = false
}

function clampMenuPosition(x: number, y: number, menuWidth = 200, menuHeight = 136) {
  return {
    x: Math.max(8, Math.min(x, window.innerWidth - menuWidth - 8)),
    y: Math.max(8, Math.min(y, window.innerHeight - menuHeight - 8)),
  }
}

function openTrackMenu(e: MouseEvent, track: TrackInfo) {
  const btn = e.currentTarget as HTMLElement
  const rect = btn.getBoundingClientRect()
  const menuWidth = 200
  const menuHeight = 136
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

function addToQueueNext(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueNext(track)
}

function addToQueueEnd(track: TrackInfo) {
  closeTrackMenu()
  player.addToQueueEnd(track)
}

function openAddToPlaylist(track: TrackInfo) {
  closeTrackMenu()
  addToPlaylistTarget.value = track
  showAddToPlaylist.value = true
}
</script>

<template>
  <div class="detail-view">
    <header class="detail-header">
      <button class="back-btn" @click="router.back()">
        <span class="material-symbols-rounded">arrow_back</span>
      </button>
      <h2 class="header-title">{{ t('home.recent_play') }}</h2>
      <div class="header-search" v-if="history.entries.length > 0">
        <span class="material-symbols-rounded search-icon">search</span>
        <input v-model="searchQuery" :placeholder="t('player.search_tracks')" class="search-input" />
      </div>
    </header>

    <div v-if="history.entries.length === 0" class="state-center">
      <span class="material-symbols-rounded" style="font-size: 48px; opacity: 0.2">history</span>
      <p>{{ t('recent.no_history') }}</p>
    </div>

    <template v-else>
      <div class="recent-actions">
        <button class="play-all-btn" @click="playAll">
          <span class="material-symbols-rounded filled">play_arrow</span>
          {{ t('player.play_all') }}
        </button>
        <button class="action-btn" @click="shufflePlay">
          <span class="material-symbols-rounded">shuffle</span>
        </button>
        <div style="flex:1" />
        <button class="action-btn danger" @click="showClearConfirm = true">
          <span class="material-symbols-rounded">delete_sweep</span>
        </button>
      </div>

      <div class="track-list">
        <div
          v-for="(entry, index) in filteredEntries"
          :key="entry.track.id + entry.playedAt"
          class="track-item"
          :class="{ active: player.currentTrack?.id === entry.track.id }"
          @click="playEntry(index)"
          @contextmenu.prevent.stop="openTrackContextMenu($event, entry.track)"
        >
          <div class="track-index">
            <div v-if="player.currentTrack?.id === entry.track.id && player.isPlaying" class="equalizer-bars"><span class="bar"/><span class="bar"/><span class="bar"/></div>
            <span v-else class="index-num">{{ index + 1 }}</span>
          </div>
          <div class="track-cover">
            <img v-if="entry.track.coverUrl" :src="entry.track.coverUrl" referrerpolicy="no-referrer" loading="lazy" @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <span v-else class="material-symbols-rounded filled">music_note</span>
          </div>
          <div class="track-info">
            <div class="track-title">{{ entry.track.title }}</div>
            <div class="track-meta">{{ entry.track.artist }}<template v-if="entry.track.album"> · {{ entry.track.album }}</template></div>
          </div>
          <div class="track-time-ago">{{ formatRelativeTime(entry.playedAt) }}</div>
          <div class="track-duration">{{ formatDuration(entry.track.durationMs) }}</div>
          <button class="track-more" @click.stop="openTrackMenu($event, entry.track)">
            <span class="material-symbols-rounded">more_vert</span>
          </button>
          <button class="track-remove" @click.stop="history.remove(entry.track.id)">
            <span class="material-symbols-rounded">close</span>
          </button>
        </div>
      </div>
    </template>

    <!-- 清空确认对话框 -->
    <Teleport to="body">
      <div v-if="showClearConfirm" class="dialog-overlay" @click="showClearConfirm = false">
        <div class="dialog-card" @click.stop>
          <h3>{{ t('recent.clear_history') }}</h3>
          <p>{{ t('recent.clear_confirm_msg') }}</p>
          <div class="dialog-actions">
            <button class="dialog-btn" @click="showClearConfirm = false">{{ t('common.cancel') }}</button>
            <button class="dialog-btn danger" @click="clearHistory">{{ t('recent.clear_confirm_btn') }}</button>
          </div>
        </div>
      </div>
    </Teleport>

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
        </div>
      </div>
    </Teleport>

    <AddToPlaylistDialog v-model:open="showAddToPlaylist" :track="addToPlaylistTarget" />
  </div>
</template>

<style scoped lang="scss">
@use '@/styles/detail-view.scss' as *;

.header-title {
  font-size: 18px;
  font-weight: 600;
  flex-shrink: 0;
}

.recent-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
}

.action-btn {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container-high); }
  &.danger:hover { color: var(--md-error, #FFB4AB); }
  .material-symbols-rounded { font-size: 22px; }
}

.track-time-ago {
  font-size: 11px;
  color: var(--md-on-surface-variant);
  opacity: 0.5;
  flex-shrink: 0;
  min-width: 60px;
  text-align: right;
}

.track-remove {
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

// 对话框
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 500;
}

.dialog-card {
  background: var(--md-surface-container-high);
  border-radius: var(--radius-xl, 28px);
  padding: 24px;
  min-width: 300px;
  max-width: 400px;

  h3 { font-size: 18px; font-weight: 600; margin-bottom: 12px; }
  p { font-size: 14px; color: var(--md-on-surface-variant); line-height: 1.5; }
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 20px;
}

.dialog-btn {
  padding: 8px 20px;
  border-radius: var(--radius-full);
  font-size: 14px;
  font-weight: 500;
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container-highest); }
  &.danger { color: var(--md-error, #FFB4AB); }
  &.danger:hover { background: color-mix(in srgb, var(--md-error, #FFB4AB) 12%, transparent); }
}
</style>
