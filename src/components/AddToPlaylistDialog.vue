<script setup lang="ts">
import { ref, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useI18n } from 'vue-i18n'
import type { TrackInfo } from '@/stores/player'
import { useToastStore } from '@/stores/toast'
import M3Input from '@/components/ui/M3Input.vue'

const props = defineProps<{
  open: boolean
  track?: TrackInfo | null
  tracks?: TrackInfo[]
}>()

const emit = defineEmits<{
  'update:open': [value: boolean]
}>()

const { t } = useI18n()
const toast = useToastStore()

interface PlaylistInfo { id: number; name: string; track_count: number; modified_at: number }

const playlists = ref<PlaylistInfo[]>([])
const isLoading = ref(false)
const isSubmitting = ref(false)
const showCreateInput = ref(false)
const newName = ref('')
const createInputRef = ref<InstanceType<typeof M3Input>>()
let loadRequest: Promise<void> | null = null

// 打开时加载歌单列表
watch(() => props.open, async (isOpen) => {
  if (isOpen) {
    showCreateInput.value = false
    newName.value = ''
    void loadPlaylists()
  }
})

async function loadPlaylists() {
  if (loadRequest) return loadRequest
  isLoading.value = playlists.value.length === 0
  loadRequest = invoke<PlaylistInfo[]>('list_playlists')
    .then(result => {
      playlists.value = result
    })
    .catch(e => {
      console.error('Load local playlists failed:', e)
    })
    .finally(() => {
      isLoading.value = false
      loadRequest = null
    })
  try {
    await loadRequest
  } catch { /* 错误已记录 */ }
}

function targetTracks(): TrackInfo[] {
  if (props.tracks?.length) return props.tracks
  return props.track ? [props.track] : []
}

async function addToPlaylist(playlistId: number) {
  const targets = targetTracks()
  if (targets.length === 0 || isSubmitting.value) return
  isSubmitting.value = true
  try {
    if (targets.length === 1) {
      await invoke('add_to_playlist', { playlistId, track: toBackendTrack(targets[0]) })
    } else {
      await invoke('add_tracks_to_playlist', { playlistId, tracks: targets.map(toBackendTrack) })
    }
    toast.success(t('library.added_to_playlist'))
    emit('update:open', false)
  } catch (e) {
    console.error('Add to playlist failed:', e)
  } finally {
    isSubmitting.value = false
  }
}

function toggleCreateInput() {
  showCreateInput.value = !showCreateInput.value
  if (showCreateInput.value) {
    nextTick(() => createInputRef.value?.focus())
  }
}

async function createAndAdd() {
  const targets = targetTracks()
  if (!newName.value.trim() || targets.length === 0 || isSubmitting.value) return
  isSubmitting.value = true
  try {
    const pl = await invoke<PlaylistInfo>('create_playlist', { name: newName.value.trim() })
    if (targets.length === 1) {
      await invoke('add_to_playlist', { playlistId: pl.id, track: toBackendTrack(targets[0]) })
    } else {
      await invoke('add_tracks_to_playlist', { playlistId: pl.id, tracks: targets.map(toBackendTrack) })
    }
    toast.success(t('library.added_to_playlist'))
    emit('update:open', false)
  } catch (e) {
    console.error('Create & add failed:', e)
  } finally {
    isSubmitting.value = false
  }
}

function close() {
  emit('update:open', false)
}

function handleKeydown(event: KeyboardEvent) {
  if (props.open && event.key === 'Escape') close()
}

onMounted(() => {
  document.addEventListener('keydown', handleKeydown)
  void loadPlaylists()
})

onBeforeUnmount(() => {
  document.removeEventListener('keydown', handleKeydown)
})

function inferSource(track: TrackInfo) {
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
    source: inferSource(track),
    url: track.audioUrl || '',
    cover_url: track.coverUrl || null,
    added_at: Math.max(0, Math.round(track.addedAt || 0)),
    sync_payload: track.syncPayload ?? null,
    playlist_key: track.playlistKey ?? null,
  }
}
</script>

<template>
  <Teleport to="body">
    <Transition name="dialog">
      <div v-if="open" class="atp-overlay" @click="close">
        <div
          class="atp-dialog"
          role="dialog"
          aria-modal="true"
          :aria-label="t('player.add_to_local_playlist')"
          @click.stop
        >
          <div class="atp-header">
            <span class="material-symbols-rounded" style="font-size: 24px; color: var(--md-primary)">playlist_add</span>
            <h3 class="atp-title">{{ t('player.add_to_local_playlist') }}</h3>
            <button class="atp-close-btn" :title="t('common.close')" @click="close">
              <span class="material-symbols-rounded">close</span>
            </button>
          </div>

          <!-- 快速新建 -->
          <div class="atp-create" @click="toggleCreateInput">
            <div class="atp-create-icon">
              <span class="material-symbols-rounded">add</span>
            </div>
            <span class="atp-create-label">{{ t('library.create_playlist') }}</span>
          </div>

          <div v-if="showCreateInput" class="atp-create-input">
            <M3Input
              ref="createInputRef"
              v-model="newName"
              :placeholder="t('library.playlist_name_placeholder')"
              :maxlength="50"
              @enter="createAndAdd"
            />
            <button
              class="atp-create-btn"
              :title="t('common.confirm')"
              :disabled="!newName.trim() || isSubmitting"
              @click="createAndAdd"
            >
              <span class="material-symbols-rounded">check</span>
            </button>
          </div>

          <!-- 歌单列表 -->
          <div v-if="playlists.length > 0" class="atp-list">
            <div
              v-for="pl in playlists"
              :key="pl.id"
              class="atp-item"
              :class="{ disabled: isSubmitting }"
              @click="addToPlaylist(pl.id)"
            >
              <div class="atp-item-icon">
                <span class="material-symbols-rounded filled" style="font-size: 20px">queue_music</span>
              </div>
              <div class="atp-item-info">
                <div class="atp-item-name">{{ pl.name }}</div>
                <div class="atp-item-count">{{ t('library.track_count', { count: pl.track_count }) }}</div>
              </div>
            </div>
          </div>

          <div v-else-if="isLoading" class="atp-loading" role="status">
            <span class="material-symbols-rounded">progress_activity</span>
          </div>

          <div v-else-if="!isLoading" class="atp-empty">
            <p>{{ t('library.playlist_empty_title') }}</p>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped lang="scss">
.atp-overlay {
  position: fixed;
  inset: 0;
  z-index: 9000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(4px);
}

.atp-dialog {
  width: 380px;
  max-width: calc(100vw - 48px);
  height: min(70vh, 640px);
  max-height: calc(100vh - 48px);
  background: var(--md-surface-container-high);
  border-radius: 28px;
  padding: 24px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  box-shadow:
    0 8px 32px rgba(0, 0, 0, 0.35),
    0 2px 8px rgba(0, 0, 0, 0.2);
}

.atp-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 20px;
}

.atp-title {
  flex: 1;
  min-width: 0;
  font-size: 20px;
  font-weight: 600;
}

.atp-close-btn {
  width: 40px;
  height: 40px;
  margin: -8px -8px -8px 0;
  border-radius: var(--radius-full);
  display: grid;
  place-items: center;
  flex: 0 0 auto;
  color: var(--md-on-surface-variant);
  transition: background 150ms;

  &:hover { background: var(--md-surface-container); }
}

.atp-create {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: var(--radius-md);
  cursor: pointer;
  transition: background 150ms;
  margin-bottom: 4px;

  &:hover { background: var(--md-surface-container); }
}

.atp-create-icon {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-md);
  background: var(--md-primary-container);
  color: var(--md-on-primary-container);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.atp-create-label {
  font-size: 14px;
  font-weight: 600;
  color: var(--md-primary);
}

.atp-create-input {
  display: flex;
  align-items: flex-end;
  gap: 8px;
  padding: 8px 12px;
  margin-bottom: 8px;

  :deep(.m3-input) {
    flex: 1;
    min-width: 0;
  }
}

.atp-create-btn {
  width: 48px;
  height: 48px;
  margin-bottom: 2px;
  border-radius: var(--radius-full);
  background: var(--md-primary);
  color: var(--md-on-primary);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  cursor: pointer;
  transition: opacity 150ms;

  &:disabled { opacity: 0.38; pointer-events: none; }
  &:hover { opacity: 0.9; }
}

.atp-list {
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: none;
  gap: 2px;

  &::-webkit-scrollbar { display: none; }
}

.atp-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: var(--radius-md);
  cursor: pointer;
  transition: background 150ms;

  &:hover { background: var(--md-surface-container); }
  &.disabled { pointer-events: none; opacity: 0.6; }
}

.atp-item-icon {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-md);
  background: var(--md-surface-container-high);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  color: var(--md-on-surface-variant);
}

.atp-item-info { flex: 1; min-width: 0; }

.atp-item-name {
  font-size: 14px;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.atp-item-count {
  font-size: 12px;
  color: var(--md-on-surface-variant);
  margin-top: 2px;
}

.atp-empty {
  text-align: center;
  padding: 24px;
  color: var(--md-on-surface-variant);
  font-size: 14px;
}

.atp-loading {
  flex: 1;
  display: grid;
  place-items: center;
  color: var(--md-primary);

  .material-symbols-rounded {
    animation: atp-spin 800ms linear infinite;
  }
}

@keyframes atp-spin {
  to { transform: rotate(360deg); }
}

// 过渡动画
.dialog-enter-active {
  transition: opacity 200ms ease-out;
  .atp-dialog { transition: transform 300ms cubic-bezier(0.05, 0.7, 0.1, 1), opacity 200ms; }
}
.dialog-leave-active {
  transition: opacity 150ms ease-in;
  .atp-dialog { transition: transform 200ms cubic-bezier(0.3, 0, 0.8, 0.15), opacity 150ms; }
}
.dialog-enter-from {
  opacity: 0;
  .atp-dialog { transform: scale(0.85); opacity: 0; }
}
.dialog-leave-to {
  opacity: 0;
  .atp-dialog { transform: scale(0.92); opacity: 0; }
}
</style>
