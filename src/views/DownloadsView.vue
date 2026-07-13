<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useDownloadStore, type ActiveDownloadTask, type DownloadedTrack } from '@/stores/download'
import { usePlayerStore, type TrackInfo } from '@/stores/player'
import { useToastStore } from '@/stores/toast'
import M3Dialog from '@/components/ui/M3Dialog.vue'

const fallbackMessages: Record<string, string> = {
  'common.back': '返回',
  'common.refresh': '刷新',
  'download.manager_title': '下载管理',
  'download.manager_desc': '集中管理下载任务、本地文件与离线播放内容',
  'download.active_count': '进行中',
  'download.completed_count': '已完成',
  'download.total_size': '本地占用',
  'download.active_tasks_desc': '下载任务会实时更新进度，可随时取消',
  'download.downloaded_desc': '当前显示 {count} 首本地歌曲',
  'download.search_placeholder': '搜索歌曲、歌手、专辑或来源',
  'download.select': '选择',
  'download.select_all': '全选当前',
  'download.deselect_all': '取消全选',
  'download.delete_selected': '删除 {count} 项',
  'download.batch_delete_confirm': '批量删除下载',
  'download.batch_delete_msg': '确定要删除选中的 {count} 个下载文件吗？此操作不可撤销。',
  'download.batch_deleted': '已删除 {count} 个下载',
  'download.search_empty_title': '没有匹配的下载',
  'download.search_empty_desc': '换个关键词试试，支持歌曲、歌手、专辑和来源',
  'download.reveal_failed': '打开所在目录失败',
  'download.in_use_hint': '当前歌曲正在使用本地下载文件，切歌或暂停后再操作',
  'player.source_netease': '网易云',
  'player.source_qq': 'QQ 音乐',
  'player.source_bilibili': '哔哩哔哩',
  'player.source_youtube': 'YouTube',
  'player.source_local': '本地',
  'download.resolving': '正在解析音频链接...',
  'download.cancelling': '正在取消...',
  'download.cancelled': '已取消下载',
  'download.download_failed': '下载失败',
  'download.already_exists': '该歌曲已下载',
  'download.downloading': '正在下载...',
  'download.delete_confirm': '删除下载',
}

function t(key: string, params?: Record<string, unknown>) {
  const text = fallbackMessages[key] || key
  if (!params) return text
  return Object.entries(params).reduce((acc, [name, value]) => acc.replaceAll(`{${name}}`, String(value)), text)
}const downloadStore = useDownloadStore()
const player = usePlayerStore()
const toast = useToastStore()

const cancelLabel = '取消'
const deleteLabel = '删除'
const cancelAllLabel = '取消全部'
const downloadedItemsLabel = '已下载文件'
const openFolderLabel = '打开所在目录'
const redownloadLabel = '重新下载'
const downloadsEmptyTitle = '还没有下载的歌曲'
const downloadsEmptyDesc = '在播放页点击更多选项下载歌曲到本地'

function activeDownloadTasksLabel(count: number) {
  return `进行中任务 (${count})`
}

function deleteConfirmMsg(name: string) {
  return `确定要删除「${name}」吗？此操作不可撤销。`
}

const searchQuery = ref('')
const selectionMode = ref(false)
const selectedIds = ref<Set<string>>(new Set())
const showDeleteDialog = ref(false)
const deleteTarget = ref<DownloadedTrack | null>(null)
const batchDeleting = ref(false)

onMounted(() => {
  downloadStore.initEvents()
  downloadStore.loadDownloads()
})

const sortedDownloads = computed(() => {
  return [...downloadStore.downloads].sort((a, b) => (b.downloadedAt || 0) - (a.downloadedAt || 0))
})

const filteredDownloads = computed(() => {
  const keyword = searchQuery.value.trim().toLowerCase()
  if (!keyword) return sortedDownloads.value
  return sortedDownloads.value.filter((track) => {
    return [track.title, track.artist, track.album, track.source, track.filePath]
      .filter(Boolean)
      .some(value => String(value).toLowerCase().includes(keyword))
  })
})

const activeTasks = computed(() => downloadStore.activeDownloads)
const downloadedCount = computed(() => downloadStore.downloads.length)
const activeCount = computed(() => activeTasks.value.length)
const totalSize = computed(() => downloadStore.downloads.reduce((sum, item) => sum + (item.fileSize || 0), 0))
const selectedCount = computed(() => selectedIds.value.size)
const selectableVisibleDownloads = computed(() => filteredDownloads.value.filter(item => !isTrackInUse(item)))
const visibleSelectedCount = computed(() => selectableVisibleDownloads.value.filter(item => selectedIds.value.has(item.id)).length)
const allVisibleSelected = computed(() => selectableVisibleDownloads.value.length > 0 && visibleSelectedCount.value === selectableVisibleDownloads.value.length)

watch(filteredDownloads, (items) => {
  const visible = new Set(items.map(item => item.id))
  selectedIds.value = new Set([...selectedIds.value].filter(id => visible.has(id)))
  if (selectionMode.value && selectedIds.value.size === 0 && searchQuery.value.trim()) return
  if (selectionMode.value && downloadStore.downloads.length === 0) selectionMode.value = false
})

function formatFileSize(bytes?: number): string {
  if (!bytes || bytes <= 0) return '0 B'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

function formatDate(ts?: number): string {
  if (!ts) return '—'
  const ms = ts < 10_000_000_000 ? ts * 1000 : ts
  return new Date(ms).toLocaleString(undefined, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function sourceLabel(source?: string): string {
  switch ((source || '').toLowerCase()) {
    case 'netease': return t('player.source_netease')
    case 'qq': return t('player.source_qq')
    case 'bilibili': return t('player.source_bilibili')
    case 'youtube': return t('player.source_youtube')
    case 'local': return t('player.source_local')
    default: return source || '—'
  }
}

function statusIcon(task: ActiveDownloadTask): string {
  switch (task.status) {
    case 'resolving': return 'network_node'
    case 'cancelling': return 'hourglass_top'
    case 'cancelled': return 'cancel'
    case 'error': return 'error'
    case 'already_exists': return 'download_done'
    default: return 'downloading'
  }
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

function activeDownloadProgressText(task: ActiveDownloadTask) {
  if (task.status === 'resolving' || task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'already_exists') {
    return activeDownloadStatusText(task.status)
  }
  if (task.status === 'error') {
    return task.message
      ? `${activeDownloadStatusText(task.status)} · ${task.message}`
      : activeDownloadStatusText(task.status)
  }

  const downloaded = typeof task.downloadedBytes === 'number' ? formatFileSize(task.downloadedBytes) : ''
  const total = typeof task.totalBytes === 'number' && task.totalBytes > 0 ? formatFileSize(task.totalBytes) : ''
  const percent = typeof task.progress === 'number' ? `${task.progress}%` : ''

  if (downloaded && total && percent) return `${downloaded} / ${total} · ${percent}`
  if (downloaded && total) return `${downloaded} / ${total}`
  if (downloaded && percent) return `${downloaded} · ${percent}`
  return activeDownloadStatusText(task.status)
}

function downloadToTrack(track: DownloadedTrack): TrackInfo {
  return {
    id: track.id,
    title: track.title,
    artist: track.artist,
    album: track.album || '',
    durationMs: track.durationMs || 0,
    coverUrl: track.coverUrl || '',
    audioUrl: track.filePath,
  }
}

function isTrackInUse(track: DownloadedTrack | null | undefined) {
  return !!track && player.isPlayingFromDownload && player.currentTrack?.id === track.id
}

function playDownloadedTrack(track: DownloadedTrack) {
  if (selectionMode.value) {
    toggleSelected(track.id)
    return
  }
  player.play(downloadToTrack(track))
}

function enterSelectionMode(track?: DownloadedTrack) {
  if (track && isTrackInUse(track)) {
    toast.show(t('download.in_use_hint'), 'info')
    return
  }
  selectionMode.value = true
  if (track) selectedIds.value = new Set(selectedIds.value).add(track.id)
}

function leaveSelectionMode() {
  selectionMode.value = false
  selectedIds.value = new Set()
}

function toggleSelected(id: string) {
  const track = downloadStore.downloads.find(item => item.id === id)
  if (isTrackInUse(track)) {
    toast.show(t('download.in_use_hint'), 'info')
    return
  }
  const next = new Set(selectedIds.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  selectedIds.value = next
  if (selectionMode.value && next.size === 0) selectionMode.value = false
}

function toggleSelectAllVisible() {
  if (allVisibleSelected.value) {
    const next = new Set(selectedIds.value)
    for (const item of filteredDownloads.value) next.delete(item.id)
    selectedIds.value = next
    if (next.size === 0) selectionMode.value = false
    return
  }

  const next = new Set(selectedIds.value)
  for (const item of filteredDownloads.value) {
    if (!isTrackInUse(item)) next.add(item.id)
  }
  selectedIds.value = next
  if (next.size > 0) selectionMode.value = true
}

function requestDelete(track: DownloadedTrack) {
  if (isTrackInUse(track)) {
    toast.show(t('download.in_use_hint'), 'info')
    return
  }
  deleteTarget.value = track
  showDeleteDialog.value = true
}

function requestBatchDelete() {
  if (selectedIds.value.size === 0) return
  deleteTarget.value = null
  showDeleteDialog.value = true
}

async function confirmDelete() {
  if (batchDeleting.value) return
  batchDeleting.value = true
  try {
    if (deleteTarget.value) {
      const target = deleteTarget.value
      await downloadStore.deleteDownload(target.id)
      player.handleDownloadedFileRemoved(target.id, target.filePath)
    } else {
      const targets = downloadStore.downloads.filter(track => selectedIds.value.has(track.id) && !isTrackInUse(track))
      for (const target of targets) {
        await downloadStore.deleteDownload(target.id, { silent: true })
        player.handleDownloadedFileRemoved(target.id, target.filePath)
      }
      if (targets.length > 0) toast.success(t('download.batch_deleted', { count: targets.length }))
      leaveSelectionMode()
    }
    showDeleteDialog.value = false
    deleteTarget.value = null
  } finally {
    batchDeleting.value = false
  }
}

async function redownloadTrack(track: DownloadedTrack) {
  if (isTrackInUse(track)) {
    toast.show(t('download.in_use_hint'), 'info')
    return
  }
  player.handleDownloadedFileRemoved(track.id, track.filePath)
  await downloadStore.redownloadTrack(downloadToTrack(track))
}

async function revealDownloadFile(track: DownloadedTrack) {
  try {
    await invoke('reveal_file', { path: track.filePath })
  } catch (e) {
    console.error('Failed to reveal file:', e)
    toast.error(t('download.reveal_failed'))
  }
}

async function refreshDownloads() {
  await downloadStore.loadDownloads()
}

function progressWidth(task: ActiveDownloadTask) {
  if (task.status === 'cancelled' || task.status === 'already_exists') return '100%'
  return `${Math.max(4, task.progress ?? 0)}%`
}
</script>

<template>
  <div class="downloads-view">
    <header class="downloads-hero">
      <div class="hero-copy">
        <button class="back-chip" @click="$router.back()">
          <span class="material-symbols-rounded">arrow_back</span>
          <span>{{ t('common.back') }}</span>
        </button>
        <h1>{{ t('download.manager_title') }}</h1>
        <p>{{ t('download.manager_desc') }}</p>
      </div>
      <div class="hero-actions">
        <button class="tonal-btn" @click="refreshDownloads">
          <span class="material-symbols-rounded">refresh</span>
          <span>{{ t('common.refresh') }}</span>
        </button>
        <button
          v-if="activeCount > 0"
          class="tonal-btn danger"
          @click="downloadStore.cancelAllDownloads()"
        >
          <span class="material-symbols-rounded">close</span>
          <span>{{ cancelAllLabel }}</span>
        </button>
      </div>
    </header>

    <section class="summary-grid">
      <div class="summary-card active">
        <span class="material-symbols-rounded filled">downloading</span>
        <div>
          <strong>{{ activeCount }}</strong>
          <small>{{ t('download.active_count') }}</small>
        </div>
      </div>
      <div class="summary-card done">
        <span class="material-symbols-rounded filled">download_done</span>
        <div>
          <strong>{{ downloadedCount }}</strong>
          <small>{{ t('download.completed_count') }}</small>
        </div>
      </div>
      <div class="summary-card storage">
        <span class="material-symbols-rounded filled">hard_drive</span>
        <div>
          <strong>{{ formatFileSize(totalSize) }}</strong>
          <small>{{ t('download.total_size') }}</small>
        </div>
      </div>
    </section>

    <section v-if="activeTasks.length > 0" class="panel active-panel">
      <div class="panel-header">
        <div>
          <h2>{{ activeDownloadTasksLabel(activeTasks.length) }}</h2>
          <p>{{ t('download.active_tasks_desc') }}</p>
        </div>
      </div>
      <div class="task-list">
        <div v-for="task in activeTasks" :key="task.trackId" class="task-card" :class="`status-${task.status}`">
          <div class="task-icon">
            <span class="material-symbols-rounded">{{ statusIcon(task) }}</span>
          </div>
          <div class="task-main">
            <div class="task-topline">
              <div class="task-title">{{ task.title }}</div>
              <div class="task-source">{{ sourceLabel(task.source) }}</div>
            </div>
            <div class="task-subtitle">{{ task.artist || '—' }} · {{ activeDownloadProgressText(task) }}</div>
            <div
              class="progress-track"
              :class="{
                indeterminate: task.status === 'downloading' && !task.totalBytes,
                error: task.status === 'error',
                muted: task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'already_exists',
              }"
            >
              <div class="progress-fill" :style="{ width: progressWidth(task) }" />
            </div>
          </div>
          <button
            class="icon-action danger"
            :disabled="task.status === 'cancelling' || task.status === 'cancelled' || task.status === 'error' || task.status === 'already_exists'"
            @click="downloadStore.cancelDownload(task.trackId)"
          >
            <span class="material-symbols-rounded">close</span>
          </button>
        </div>
      </div>
    </section>

    <section class="panel downloaded-panel">
      <div class="panel-header sticky-tools">
        <div>
          <h2>{{ downloadedItemsLabel }}</h2>
          <p>{{ t('download.downloaded_desc', { count: filteredDownloads.length }) }}</p>
        </div>
        <div class="toolbar-actions">
          <div class="search-box">
            <span class="material-symbols-rounded">search</span>
            <input v-model="searchQuery" :placeholder="t('download.search_placeholder')" />
            <button v-if="searchQuery" @click="searchQuery = ''">
              <span class="material-symbols-rounded">close</span>
            </button>
          </div>
          <button
            v-if="downloadStore.downloads.length > 0 && !selectionMode"
            class="tonal-btn"
            @click="enterSelectionMode()"
          >
            <span class="material-symbols-rounded">checklist</span>
            <span>{{ t('download.select') }}</span>
          </button>
          <template v-else-if="selectionMode">
            <button class="tonal-btn" @click="toggleSelectAllVisible">
              <span class="material-symbols-rounded">{{ allVisibleSelected ? 'deselect' : 'select_all' }}</span>
              <span>{{ allVisibleSelected ? t('download.deselect_all') : t('download.select_all') }}</span>
            </button>
            <button class="tonal-btn danger" :disabled="selectedCount === 0" @click="requestBatchDelete">
              <span class="material-symbols-rounded">delete</span>
              <span>{{ t('download.delete_selected', { count: selectedCount }) }}</span>
            </button>
            <button class="ghost-btn" @click="leaveSelectionMode">{{ cancelLabel }}</button>
          </template>
        </div>
      </div>

      <div v-if="filteredDownloads.length > 0" class="download-list">
        <div
          v-for="track in filteredDownloads"
          :key="track.id"
          class="download-row"
          :class="{
            selected: selectedIds.has(track.id),
            playing: player.currentTrack?.id === track.id,
            disabled: isTrackInUse(track),
          }"
          @click="playDownloadedTrack(track)"
          @contextmenu.prevent="enterSelectionMode(track)"
        >
          <button v-if="selectionMode" class="select-dot" :disabled="isTrackInUse(track)" @click.stop="toggleSelected(track.id)">
            <span class="material-symbols-rounded filled">{{ selectedIds.has(track.id) ? 'check_circle' : 'radio_button_unchecked' }}</span>
          </button>

          <div class="cover-box">
            <img v-if="track.coverUrl" :src="track.coverUrl" referrerpolicy="no-referrer" loading="lazy" />
            <span v-else class="material-symbols-rounded filled">music_note</span>
            <div class="play-overlay"><span class="material-symbols-rounded">play_arrow</span></div>
          </div>

          <div class="track-info">
            <div class="track-title">{{ track.title }}</div>
            <div class="track-meta">{{ track.artist || '—' }}<template v-if="track.album"> · {{ track.album }}</template></div>
          </div>

          <div class="track-source">{{ sourceLabel(track.source) }}</div>
          <div class="track-size">{{ formatFileSize(track.fileSize) }}</div>
          <div class="track-date">{{ formatDate(track.downloadedAt) }}</div>

          <div class="row-actions" @click.stop>
            <button class="icon-action" :title="openFolderLabel" @click="revealDownloadFile(track)">
              <span class="material-symbols-rounded">folder_open</span>
            </button>
            <button class="icon-action" :title="redownloadLabel" :disabled="isTrackInUse(track)" @click="redownloadTrack(track)">
              <span class="material-symbols-rounded">refresh</span>
            </button>
            <button class="icon-action danger" :title="deleteLabel" :disabled="isTrackInUse(track)" @click="requestDelete(track)">
              <span class="material-symbols-rounded">delete</span>
            </button>
          </div>
        </div>
      </div>

      <div v-else class="empty-state">
        <div class="empty-orb"><span class="material-symbols-rounded filled">download</span></div>
        <h3>{{ searchQuery ? t('download.search_empty_title') : downloadsEmptyTitle }}</h3>
        <p>{{ searchQuery ? t('download.search_empty_desc') : downloadsEmptyDesc }}</p>
      </div>
    </section>

    <M3Dialog
      v-model:open="showDeleteDialog"
      :title="deleteTarget ? t('download.delete_confirm') : t('download.batch_delete_confirm')"
      icon="delete"
      :confirm-text="deleteLabel"
      confirm-danger
      :confirm-disabled="batchDeleting"
      @confirm="confirmDelete"
    >
      <p class="dialog-msg">
        {{ deleteTarget
          ? deleteConfirmMsg(deleteTarget.title)
          : t('download.batch_delete_msg', { count: selectedCount }) }}
      </p>
    </M3Dialog>
  </div>
</template>

<style scoped lang="scss">
.downloads-view {
  padding: 20px 28px 36px;
  max-width: 1180px;
}

.downloads-hero {
  min-height: 148px;
  border-radius: 28px;
  padding: 22px 24px;
  margin-bottom: 16px;
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 18px;
  position: relative;
  overflow: hidden;
  background:
    radial-gradient(circle at 14% 18%, color-mix(in srgb, var(--md-primary) 30%, transparent), transparent 30%),
    radial-gradient(circle at 86% 8%, color-mix(in srgb, var(--md-tertiary) 26%, transparent), transparent 30%),
    linear-gradient(135deg, var(--md-surface-container-high), var(--md-surface-container-low));
  border: 1px solid color-mix(in srgb, var(--md-outline-variant) 60%, transparent);

  &::after {
    content: '';
    position: absolute;
    inset: auto -80px -110px auto;
    width: 300px;
    height: 300px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--md-primary) 12%, transparent);
    pointer-events: none;
  }
}

.hero-copy,
.hero-actions {
  position: relative;
  z-index: 1;
}

.back-chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 34px;
  padding: 0 12px;
  margin-bottom: 16px;
  border-radius: var(--radius-full);
  background: color-mix(in srgb, var(--md-surface-container-highest) 80%, transparent);
  color: var(--md-on-surface-variant);
  font-size: 13px;
  font-weight: 600;

  .material-symbols-rounded { font-size: 18px; }
  &:hover { background: var(--md-surface-container-highest); }
}

h1 {
  font-size: 34px;
  line-height: 1.05;
  letter-spacing: -0.8px;
  margin-bottom: 8px;
}

.hero-copy p,
.panel-header p {
  color: var(--md-on-surface-variant);
  font-size: 13px;
}

.hero-actions,
.toolbar-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.tonal-btn,
.ghost-btn,
.icon-action {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  border-radius: var(--radius-full);
  font-weight: 700;
  transition: background var(--duration-short), color var(--duration-short), opacity var(--duration-short), transform var(--duration-short);

  &:active:not(:disabled) { transform: scale(0.98); }
  &:disabled { opacity: 0.42; cursor: not-allowed; }
}

.tonal-btn {
  height: 38px;
  padding: 0 14px;
  background: var(--md-secondary-container);
  color: var(--md-on-secondary-container);
  font-size: 13px;

  &:hover:not(:disabled) { background: color-mix(in srgb, var(--md-secondary-container) 82%, white); }
  &.danger {
    background: color-mix(in srgb, var(--md-error) 14%, transparent);
    color: var(--md-error);
  }
}

.ghost-btn {
  height: 38px;
  padding: 0 12px;
  color: var(--md-on-surface-variant);
  font-size: 13px;
  &:hover { background: var(--md-surface-container-high); }
}

.summary-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
  margin-bottom: 16px;
}

.summary-card {
  display: flex;
  align-items: center;
  gap: 14px;
  min-height: 82px;
  padding: 16px;
  border-radius: 22px;
  background: var(--md-surface-container);
  border: 1px solid var(--md-outline-variant);

  > .material-symbols-rounded {
    width: 42px;
    height: 42px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 15px;
    background: var(--md-surface-container-highest);
    color: var(--md-primary);
  }

  strong {
    display: block;
    font-size: 24px;
    line-height: 1.05;
  }

  small {
    display: block;
    margin-top: 4px;
    font-size: 12px;
    color: var(--md-on-surface-variant);
  }

  &.active > .material-symbols-rounded { color: #64b5f6; }
  &.done > .material-symbols-rounded { color: #81c784; }
  &.storage > .material-symbols-rounded { color: var(--md-tertiary); }
}

.panel {
  border-radius: 24px;
  background: color-mix(in srgb, var(--md-surface-container-low) 80%, transparent);
  border: 1px solid color-mix(in srgb, var(--md-outline-variant) 70%, transparent);
  overflow: hidden;
  margin-bottom: 16px;
}

.panel-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  padding: 18px 18px 12px;

  h2 {
    font-size: 18px;
    letter-spacing: -0.2px;
    margin-bottom: 2px;
  }
}

.sticky-tools {
  position: sticky;
  top: -20px;
  z-index: 5;
  background: color-mix(in srgb, var(--md-surface-container-low) 94%, transparent);
  backdrop-filter: blur(16px);
}
/* Linux WebKitGTK 无 backdrop-filter 时给不透明底色，避免滚动内容穿透 */
@supports not ((backdrop-filter: blur(1px)) or (-webkit-backdrop-filter: blur(1px))) {
  .sticky-tools {
    background: var(--md-surface-container-low);
  }
}

.task-list,
.download-list {
  padding: 0 10px 10px;
}

.task-card,
.download-row {
  display: flex;
  align-items: center;
  gap: 12px;
  border-radius: 18px;
  background: transparent;
  transition: background var(--duration-short), border-color var(--duration-short);
}

.task-card {
  padding: 12px;
  border: 1px solid transparent;

  &:hover { background: var(--md-surface-container); }
}

.task-icon {
  width: 42px;
  height: 42px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 14px;
  background: var(--md-primary-container);
  color: var(--md-on-primary-container);
  flex-shrink: 0;
}

.task-main {
  flex: 1;
  min-width: 0;
}

.task-topline {
  display: flex;
  align-items: center;
  gap: 10px;
}

.task-title {
  font-size: 14px;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.task-source,
.track-source {
  flex-shrink: 0;
  border-radius: var(--radius-full);
  padding: 3px 8px;
  background: color-mix(in srgb, var(--md-primary) 10%, transparent);
  color: var(--md-primary);
  font-size: 11px;
  font-weight: 700;
}

.task-subtitle,
.track-meta,
.track-size,
.track-date {
  color: var(--md-on-surface-variant);
  font-size: 12px;
}

.progress-track {
  height: 6px;
  margin-top: 8px;
  overflow: hidden;
  border-radius: var(--radius-full);
  background: var(--md-surface-container-highest);
}

.progress-fill {
  height: 100%;
  min-width: 4px;
  border-radius: inherit;
  background: linear-gradient(90deg, var(--md-primary), color-mix(in srgb, var(--md-primary) 65%, white));
  transition: width 180ms ease;
}

.progress-track.indeterminate .progress-fill {
  width: 36% !important;
  animation: download-indeterminate 1.2s ease-in-out infinite;
}
.progress-track.error .progress-fill { background: var(--md-error); }
.progress-track.muted .progress-fill { background: var(--md-outline); }

@keyframes download-indeterminate {
  0% { transform: translateX(-100%); }
  100% { transform: translateX(280%); }
}

.search-box {
  height: 38px;
  min-width: 260px;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 10px 0 12px;
  border-radius: var(--radius-full);
  background: var(--md-surface-container);
  border: 1px solid var(--md-outline-variant);
  color: var(--md-on-surface-variant);

  .material-symbols-rounded { font-size: 18px; }

  input {
    flex: 1;
    width: 100%;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--md-on-surface);
    font: inherit;
    font-size: 13px;
    user-select: text;
  }

  button {
    width: 24px;
    height: 24px;
    border-radius: var(--radius-full);
    &:hover { background: var(--md-surface-container-highest); }
  }
}

.download-row {
  min-height: 70px;
  padding: 9px 10px;
  cursor: pointer;

  &:hover { background: var(--md-surface-container); }
  &.selected { background: color-mix(in srgb, var(--md-primary) 13%, transparent); }
  &.playing .track-title { color: var(--md-primary); }
}

.select-dot {
  width: 32px;
  height: 32px;
  border-radius: var(--radius-full);
  color: var(--md-primary);
  flex-shrink: 0;

  &:hover:not(:disabled) { background: color-mix(in srgb, var(--md-primary) 10%, transparent); }
  &:disabled { opacity: 0.35; cursor: not-allowed; }
}

.cover-box {
  width: 52px;
  height: 52px;
  border-radius: 14px;
  overflow: hidden;
  background: var(--md-surface-container-highest);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  flex-shrink: 0;
  position: relative;

  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
}

.play-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.38);
  opacity: 0;
  transition: opacity var(--duration-short);

  .material-symbols-rounded { color: white; }
  .download-row:hover & { opacity: 1; }
}

.track-info {
  flex: 1;
  min-width: 0;
}

.track-title {
  font-size: 14px;
  font-weight: 700;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.track-meta {
  margin-top: 2px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.track-size { width: 84px; text-align: right; flex-shrink: 0; }
.track-date { width: 116px; text-align: right; flex-shrink: 0; }

.row-actions {
  display: flex;
  align-items: center;
  gap: 2px;
  opacity: 0;
  transition: opacity var(--duration-short);
  flex-shrink: 0;

  .download-row:hover &,
  .download-row.selected & { opacity: 1; }
}

.icon-action {
  width: 34px;
  height: 34px;
  color: var(--md-on-surface-variant);

  .material-symbols-rounded { font-size: 19px; }
  &:hover:not(:disabled) { background: var(--md-surface-container-highest); color: var(--md-on-surface); }
  &.danger { color: var(--md-error); }
  &.danger:hover:not(:disabled) { background: color-mix(in srgb, var(--md-error) 12%, transparent); }
}

.empty-state {
  min-height: 300px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  text-align: center;
  padding: 40px 20px;
  color: var(--md-on-surface-variant);

  h3 {
    margin-top: 16px;
    font-size: 17px;
    color: var(--md-on-surface);
  }

  p {
    margin-top: 6px;
    max-width: 320px;
    font-size: 13px;
    opacity: 0.72;
  }
}

.empty-orb {
  width: 86px;
  height: 86px;
  border-radius: 30px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: radial-gradient(circle at 30% 20%, color-mix(in srgb, var(--md-primary) 28%, transparent), var(--md-surface-container));

  .material-symbols-rounded { font-size: 40px; color: var(--md-primary); }
}

.dialog-msg {
  color: var(--md-on-surface-variant);
  font-size: 14px;
  line-height: 1.55;
}

@media (max-width: 980px) {
  .downloads-hero,
  .panel-header {
    align-items: flex-start;
    flex-direction: column;
  }

  .summary-grid { grid-template-columns: 1fr; }
  .toolbar-actions { justify-content: flex-start; width: 100%; }
  .search-box { width: 100%; min-width: 0; }
  .track-source,
  .track-size,
  .track-date { display: none; }
  .row-actions { opacity: 1; }
}
</style>
