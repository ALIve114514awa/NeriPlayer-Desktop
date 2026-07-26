<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import { normalizeTrack, usePlayerStore, type TrackInfo } from '@/stores/player'
import BilibiliCoverImage from '@/components/BilibiliCoverImage.vue'
import { createLogger } from '@/utils/logger'

const log = createLogger('favorite-playlist-view')

const route = useRoute()
const router = useRouter()
const player = usePlayerStore()
const { t } = useI18n()

const loading = ref(true)
const name = ref('')
const source = ref('')
const coverUrl = ref('')
const tracks = ref<TrackInfo[]>([])

const favoriteId = computed(() => String(route.params.id ?? ''))

function formatDuration(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1000)
  return `${Math.floor(seconds / 60)}:${(seconds % 60).toString().padStart(2, '0')}`
}

function platformLabel(value: string): string {
  const key = value.toLowerCase()
  if (key.includes('netease')) return t('player.source_netease')
  if (key.includes('bili')) return t('player.source_bilibili')
  if (key.includes('youtube')) return 'YouTube Music'
  if (key.includes('qq')) return t('player.source_qq')
  return value
}

async function load() {
  loading.value = true
  try {
    const raw = await invoke<any[]>('list_favorite_playlists')
    const found = (raw || []).find((item: any) => String(item?.id ?? '') === favoriteId.value)
    if (!found) {
      tracks.value = []
      return
    }
    name.value = found.name ?? ''
    source.value = found.source ?? ''
    coverUrl.value = found.cover_url ?? ''
    // 收藏项的曲目随同步数据一起落地，这里不需要再打平台接口，
    // 断网或平台接口异常时也能正常打开
    tracks.value = (found.songs || []).map(normalizeTrack).filter((track: TrackInfo) => !!track.id)
  } catch (e) {
    log.error('load favorite playlist failed:', e)
    tracks.value = []
  } finally {
    loading.value = false
  }
}

function playAll() {
  if (tracks.value.length) player.playAll(tracks.value)
}

function shufflePlay() {
  if (tracks.value.length) player.shufflePlay(tracks.value)
}

function playTrack(index: number) {
  player.playAll(tracks.value, tracks.value[index]?.id)
}

onMounted(load)
</script>

<template>
  <div class="detail-view">
    <div class="detail-header">
      <button class="back-btn" @click="router.back()">
        <span class="material-symbols-rounded">arrow_back</span>
      </button>
      <div class="header-title">{{ name || t('library.tab_favorites') }}</div>
    </div>

    <div v-if="loading" class="empty-state">
      <span class="material-symbols-rounded spinning">progress_activity</span>
    </div>

    <div v-else-if="tracks.length === 0" class="empty-state">
      <span class="material-symbols-rounded">bookmark</span>
      <p>{{ t('library.favorite_empty_songs') }}</p>
    </div>

    <template v-else>
      <div class="fav-summary">
        <span>{{ t('player.track_count', { count: tracks.length }) }}</span>
        <span class="dot">·</span>
        <span>{{ platformLabel(source) }}</span>
      </div>

      <div class="fav-actions">
        <button class="primary-action" @click="playAll">
          <span class="material-symbols-rounded filled" style="font-size: 20px">play_arrow</span>
          <span>{{ t('player.play_all') }}</span>
        </button>
        <button class="secondary-action" @click="shufflePlay">
          <span class="material-symbols-rounded" style="font-size: 20px">shuffle</span>
          <span>{{ t('player.shuffle_play') }}</span>
        </button>
      </div>

      <div class="track-list">
        <div
          v-for="(track, index) in tracks"
          :key="track.id + '-' + index"
          class="track-item"
          :class="{ active: player.currentTrack?.id === track.id }"
          @click="playTrack(index)"
        >
          <div class="track-index">
            <div
              v-if="player.currentTrack?.id === track.id && player.isPlaying"
              class="equalizer-bars"
            >
              <span class="bar" /><span class="bar" /><span class="bar" />
            </div>
            <span v-else class="index-num">{{ index + 1 }}</span>
          </div>
          <div class="track-cover">
            <BilibiliCoverImage v-if="track.coverUrl" :src="track.coverUrl" loading="lazy" />
            <span v-else class="material-symbols-rounded filled">music_note</span>
          </div>
          <div class="track-info">
            <div class="track-title">{{ track.title }}</div>
            <div class="track-meta">{{ track.artist }}</div>
          </div>
          <div class="track-duration">{{ formatDuration(track.durationMs) }}</div>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped lang="scss">
@use '@/styles/detail-view.scss' as *;

.header-title {
  font-size: 18px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.fav-summary {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--md-on-surface-variant);
  margin-bottom: 16px;

  .dot { opacity: 0.5; }
}

.fav-actions {
  display: flex;
  gap: 10px;
  margin-bottom: 20px;
}

.primary-action,
.secondary-action {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 40px;
  padding: 0 20px;
  border-radius: var(--radius-full);
  font-size: 14px;
  font-weight: 500;
  transition:
    background var(--duration-short) var(--easing-standard, ease),
    transform 120ms var(--easing-emphasized, cubic-bezier(0.2, 0, 0, 1));

  &:active { transform: scale(0.97); }
}

.primary-action {
  background: var(--md-primary);
  color: var(--md-on-primary);

  &:hover { filter: brightness(1.06); }
}

.secondary-action {
  background: var(--md-surface-container-high);
  color: var(--md-on-surface);

  &:hover { background: var(--md-surface-container-highest); }
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  padding: 64px 0;
  color: var(--md-on-surface-variant);

  .material-symbols-rounded { font-size: 44px; opacity: 0.4; }
  p { font-size: 14px; opacity: 0.7; }
}

.spinning { animation: fav-spin 1s linear infinite; }

@keyframes fav-spin {
  to { transform: rotate(360deg); }
}
</style>
