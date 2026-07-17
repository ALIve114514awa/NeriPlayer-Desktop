<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import { usePlayerStore } from '@/stores/player'
import { useSyncStore } from '@/stores/sync'
import { useSettingsStore } from '@/stores/settings'
import { useAuthStore } from '@/stores/auth'
import { useToastStore } from '@/stores/toast'

const router = useRouter()
const { t } = useI18n()
const player = usePlayerStore()
const syncStore = useSyncStore()
const settingsStore = useSettingsStore()
const authStore = useAuthStore()
const toast = useToastStore()

const buildInfo = ref<{ uuid: string; timestamp: string } | null>(null)
const appDataDir = ref('')
const debugCookieStorage = ref<{ available: boolean; stored: boolean } | null>(null)
const clearingDebugCookies = ref(false)
const showClearDebugCookiesConfirm = ref(false)

const probes = ref<Record<string, 'idle' | 'testing' | 'success' | 'failed'>>({
  netease: 'idle',
  bilibili: 'idle',
  youtube: 'idle',
})

const platformName = (() => {
  const ua = navigator.userAgent
  if (/Mac/.test(ua)) return 'macOS'
  if (/Win/.test(ua)) return 'Windows'
  if (/Linux/.test(ua)) return 'Linux'
  return navigator.platform || 'Unknown'
})()

const webviewVersion = (() => {
  const match = navigator.userAgent.match(/Chrome\/([\d.]+)/)
  return match ? `Chromium ${match[1]}` : navigator.userAgent.slice(0, 60)
})()

async function testNetease() {
  probes.value.netease = 'testing'
  try {
    await invoke('get_netease_song_url', { songId: 1, quality: 'standard' })
    probes.value.netease = 'success'
  } catch {
    probes.value.netease = 'success'
  }
}

async function testBilibili() {
  probes.value.bilibili = 'testing'
  try {
    await invoke('get_bili_audio_url', { bvid: 'BV1GJ411x7h7', avid: null, cid: null })
    probes.value.bilibili = 'success'
  } catch {
    probes.value.bilibili = 'failed'
  }
}

async function testYouTube() {
  probes.value.youtube = 'testing'
  try {
    await invoke('get_youtube_audio_url', { videoId: 'dQw4w9WgXcQ' })
    probes.value.youtube = 'success'
  } catch {
    probes.value.youtube = 'failed'
  }
}

function testAll() {
  testNetease()
  testBilibili()
  testYouTube()
}

function probeStatusText(status: string): string {
  switch (status) {
    case 'testing': return t('settings.probe_testing')
    case 'success': return t('settings.probe_success')
    case 'failed': return t('settings.probe_failed')
    default: return '—'
  }
}

function probeStatusColor(status: string): string {
  switch (status) {
    case 'success': return 'var(--md-primary)'
    case 'failed': return 'var(--md-error)'
    default: return 'var(--md-on-surface-variant)'
  }
}

function formatSyncTime(ts: number): string {
  if (!ts) return t('settings.debug_never')
  return new Date(ts).toLocaleString()
}

function goBack() {
  router.push('/')
}

function hideDebugMode() {
  settingsStore.devModeEnabled = false
  router.push('/settings')
}

async function loadDebugCookieStorageStatus() {
  try {
    debugCookieStorage.value = await invoke('get_debug_cookie_storage_status')
  } catch {
    debugCookieStorage.value = null
  }
}

async function clearDebugCookies() {
  showClearDebugCookiesConfirm.value = false
  clearingDebugCookies.value = true
  try {
    await invoke('clear_debug_cookie_storage')
    await authStore.checkStatus()
    await loadDebugCookieStorageStatus()
    toast.success(t('settings.debug_cookie_clear_success'))
  } catch (error) {
    console.error('Failed to clear debug cookies:', error)
    toast.error(t('settings.debug_cookie_clear_failed'))
  } finally {
    clearingDebugCookies.value = false
  }
}

onMounted(async () => {
  try {
    buildInfo.value = await invoke('get_build_info')
  } catch { /* 命令不存在时忽略 */ }
  try {
    appDataDir.value = await invoke('get_app_data_dir')
  } catch { /* 忽略 */ }
  await loadDebugCookieStorageStatus()
})
</script>

<template>
  <div class="debug-view">
    <header class="debug-header">
      <button class="debug-back-btn" @click="goBack">
        <span class="material-symbols-rounded">arrow_back</span>
      </button>
      <h1 class="page-title">{{ t('settings.debug_title') }}</h1>
    </header>

    <!-- 构建信息 -->
    <div v-if="buildInfo" class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">info</span>
      <span>{{ t('settings.build_uuid').split(' ')[0] }}</span>
    </div>
    <div v-if="buildInfo" class="setting-card">
      <div class="setting-info">
        <div class="state-grid">
          <div class="state-item">
            <span class="state-label">{{ t('settings.build_uuid') }}</span>
            <span class="state-value mono">{{ buildInfo.uuid }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">{{ t('settings.build_time') }}</span>
            <span class="state-value">{{ buildInfo.timestamp }}</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 系统信息 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">computer</span>
      <span>{{ t('settings.debug_system_info') }}</span>
    </div>
    <div class="setting-card">
      <div class="setting-info">
        <div class="state-grid">
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_platform') }}</span>
            <span class="state-value">{{ platformName }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_webview') }}</span>
            <span class="state-value mono">{{ webviewVersion }}</span>
          </div>
          <div v-if="appDataDir" class="state-item full-width">
            <span class="state-label">{{ t('settings.debug_data_dir') }}</span>
            <span class="state-value mono">{{ appDataDir }}</span>
          </div>
        </div>
      </div>
    </div>

    <!-- Debug Cookie 存储 -->
    <template v-if="debugCookieStorage?.available">
      <div class="section-label">
        <span class="material-symbols-rounded" style="font-size: 18px">cookie</span>
        <span>{{ t('settings.debug_cookie_storage') }}</span>
      </div>
      <div class="setting-card">
        <div class="setting-icon-wrap">
          <span class="material-symbols-rounded">delete_sweep</span>
        </div>
        <div class="setting-info">
          <div class="setting-title">{{ t('settings.debug_cookie_storage') }}</div>
          <div class="setting-desc">
            {{ debugCookieStorage.stored
              ? t('settings.debug_cookie_storage_present')
              : t('settings.debug_cookie_storage_empty') }}
          </div>
        </div>
        <button
          class="debug-cookie-clear-btn"
          :disabled="clearingDebugCookies"
          @click="showClearDebugCookiesConfirm = true"
        >
          <span v-if="clearingDebugCookies" class="material-symbols-rounded spinning">progress_activity</span>
          <span v-else class="material-symbols-rounded">delete</span>
          {{ t('settings.debug_cookie_clear') }}
        </button>
      </div>
    </template>

    <!-- API 探针 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">sensors</span>
      <span>{{ t('settings.api_probe') }}</span>
    </div>

    <div class="setting-card" style="cursor: pointer" @click="testAll">
      <div class="setting-icon-wrap"><span class="material-symbols-rounded">play_arrow</span></div>
      <div class="setting-info">
        <div class="setting-title">{{ t('settings.api_probe') }}</div>
        <div class="setting-desc">{{ t('settings.api_probe_desc') }}</div>
      </div>
      <span class="material-symbols-rounded" style="font-size: 20px; opacity: 0.5">chevron_right</span>
    </div>

    <div class="setting-card sub-card">
      <div class="setting-icon-wrap"><span class="material-symbols-rounded">cloud</span></div>
      <div class="setting-info">
        <div class="setting-title">{{ t('settings.probe_netease') }}</div>
        <div class="setting-desc" :style="{ color: probeStatusColor(probes.netease) }">
          <span v-if="probes.netease === 'testing'" class="material-symbols-rounded spinning" style="font-size: 14px; vertical-align: middle">progress_activity</span>
          {{ probeStatusText(probes.netease) }}
        </div>
      </div>
    </div>

    <div class="setting-card sub-card">
      <div class="setting-icon-wrap"><span class="material-symbols-rounded">smart_display</span></div>
      <div class="setting-info">
        <div class="setting-title">{{ t('settings.probe_bilibili') }}</div>
        <div class="setting-desc" :style="{ color: probeStatusColor(probes.bilibili) }">
          <span v-if="probes.bilibili === 'testing'" class="material-symbols-rounded spinning" style="font-size: 14px; vertical-align: middle">progress_activity</span>
          {{ probeStatusText(probes.bilibili) }}
        </div>
      </div>
    </div>

    <div class="setting-card sub-card">
      <div class="setting-icon-wrap"><span class="material-symbols-rounded">play_circle</span></div>
      <div class="setting-info">
        <div class="setting-title">{{ t('settings.probe_youtube') }}</div>
        <div class="setting-desc" :style="{ color: probeStatusColor(probes.youtube) }">
          <span v-if="probes.youtube === 'testing'" class="material-symbols-rounded spinning" style="font-size: 14px; vertical-align: middle">progress_activity</span>
          {{ probeStatusText(probes.youtube) }}
        </div>
      </div>
    </div>

    <!-- 同步状态 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">sync</span>
      <span>{{ t('settings.debug_sync_status') }}</span>
    </div>
    <div class="setting-card">
      <div class="setting-info">
        <div class="state-grid">
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_github_sync') }}</span>
            <span class="state-value" :style="{ color: syncStore.github.configured ? 'var(--md-primary)' : 'var(--md-on-surface-variant)' }">
              {{ syncStore.github.configured
                ? (syncStore.github.autoSync ? t('settings.debug_auto_sync_on') : t('settings.debug_auto_sync_off'))
                : t('settings.debug_not_configured') }}
            </span>
          </div>
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_last_sync') }} (GitHub)</span>
            <span class="state-value">{{ formatSyncTime(syncStore.github.lastSyncTime) }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_webdav_sync') }}</span>
            <span class="state-value" :style="{ color: syncStore.webdav.configured ? 'var(--md-primary)' : 'var(--md-on-surface-variant)' }">
              {{ syncStore.webdav.configured
                ? (syncStore.webdav.autoSync ? t('settings.debug_auto_sync_on') : t('settings.debug_auto_sync_off'))
                : t('settings.debug_not_configured') }}
            </span>
          </div>
          <div class="state-item">
            <span class="state-label">{{ t('settings.debug_last_sync') }} (WebDAV)</span>
            <span class="state-value">{{ formatSyncTime(syncStore.webdav.lastSyncTime) }}</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 播放器状态 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">queue_music</span>
      <span>{{ t('settings.player_state') }}</span>
    </div>
    <div class="setting-card">
      <div class="setting-info">
        <div class="state-grid">
          <div class="state-item">
            <span class="state-label">Current Track</span>
            <span class="state-value">{{ player.currentTrack?.title || '—' }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">Artist</span>
            <span class="state-value">{{ player.currentTrack?.artist || '—' }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">Source</span>
            <span class="state-value mono">{{ player.currentTrack?.id?.split(':')[0] || '—' }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">Playing</span>
            <span class="state-value">{{ player.isPlaying ? 'Yes' : 'No' }}</span>
          </div>
          <div class="state-item">
            <span class="state-label">Position</span>
            <span class="state-value mono">{{ Math.floor(player.positionMs / 1000) }}s / {{ Math.floor(player.durationMs / 1000) }}s</span>
          </div>
          <div class="state-item">
            <span class="state-label">Queue Size</span>
            <span class="state-value">{{ player.queue?.length || 0 }}</span>
          </div>
        </div>
      </div>
    </div>

    <Teleport to="body">
      <div
        v-if="showClearDebugCookiesConfirm"
        class="debug-dialog-overlay"
        @click.self="showClearDebugCookiesConfirm = false"
      >
        <div class="debug-dialog-card">
          <h3>{{ t('settings.debug_cookie_clear_confirm_title') }}</h3>
          <p>{{ t('settings.debug_cookie_clear_confirm_desc') }}</p>
          <div class="debug-dialog-actions">
            <button @click="showClearDebugCookiesConfirm = false">
              {{ t('settings.cancel') }}
            </button>
            <button class="danger" @click="clearDebugCookies">
              {{ t('settings.debug_cookie_clear') }}
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 隐藏调试模式 -->
    <div class="hide-debug-section">
      <button class="hide-debug-btn" @click="hideDebugMode">
        <span class="material-symbols-rounded">visibility_off</span>
        <span>{{ t('settings.debug_hide') }}</span>
      </button>
    </div>
  </div>
</template>

<style scoped lang="scss">
.debug-view {
  padding: 20px 28px 32px;
  max-width: 680px;
}

.debug-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 24px;
  padding-top: 8px;
}

.debug-back-btn {
  width: 40px;
  height: 40px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface);
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container-high); }
}

.page-title {
  font-size: 28px;
  font-weight: 700;
  letter-spacing: -0.5px;
}

.section-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.8px;
  color: var(--md-primary);
  margin: 24px 0 10px;
  padding: 0 4px;

  &:first-of-type { margin-top: 0; }
}

.setting-card {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 14px 16px;
  border-radius: var(--radius-lg);
  background: var(--md-surface-container);
  margin-bottom: 8px;
  transition: background var(--duration-short);

  &:hover { background: var(--md-surface-container-high); }
}

.sub-card {
  margin-left: 54px;
  background: var(--md-surface-container-low) !important;
}

.setting-icon-wrap {
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

.setting-info { flex: 1; min-width: 0; }
.setting-title { font-size: 14px; font-weight: 500; }
.setting-desc { font-size: 12px; color: var(--md-on-surface-variant); margin-top: 2px; }

.spinning {
  animation: spin 1s linear infinite;
}

@keyframes spin { to { transform: rotate(360deg); } }

.state-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px 24px;
  padding: 4px 0;
}

.state-item {
  display: flex;
  flex-direction: column;
  gap: 2px;

  &.full-width {
    grid-column: 1 / -1;
  }
}

.state-label {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  color: var(--md-on-surface-variant);
  opacity: 0.7;
}

.state-value {
  font-size: 13px;
  font-weight: 500;
  color: var(--md-on-surface);
  word-break: break-all;
  line-height: 1.45;

  &.mono {
    font-family: 'SF Mono', 'Cascadia Code', 'Fira Code', monospace;
    font-size: 12px;
  }
}

.debug-cookie-clear-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  border-radius: var(--radius-full);
  background: var(--md-error-container, rgba(255, 80, 80, 0.12));
  color: var(--md-on-error-container, #ff5050);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;

  &:disabled {
    cursor: wait;
    opacity: 0.6;
  }

  .material-symbols-rounded { font-size: 17px; }
}

.debug-dialog-overlay {
  position: fixed;
  inset: 0;
  z-index: 10000;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
  background: rgba(0, 0, 0, 0.48);
}

.debug-dialog-card {
  width: min(380px, 100%);
  padding: 22px;
  border-radius: var(--radius-xl);
  background: var(--md-surface-container-high);
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.3);

  h3 { margin: 0; font-size: 18px; }
  p {
    margin: 12px 0 20px;
    color: var(--md-on-surface-variant);
    font-size: 13px;
    line-height: 1.55;
  }
}

.debug-dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;

  button {
    padding: 8px 14px;
    border-radius: var(--radius-full);
    color: var(--md-on-surface);
    cursor: pointer;
  }

  button.danger {
    background: var(--md-error);
    color: var(--md-on-error);
  }
}

.hide-debug-section {
  margin-top: 32px;
  padding-top: 16px;
  border-top: 1px solid var(--md-outline-variant, rgba(255,255,255,0.08));
}

.hide-debug-btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 10px 20px;
  border-radius: 12px;
  background: var(--md-error-container, rgba(255, 80, 80, 0.12));
  color: var(--md-on-error-container, #ff5050);
  font-size: 14px;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.15s, transform 0.1s;

  .material-symbols-rounded {
    font-size: 20px;
  }

  &:hover {
    background: var(--md-error-container, rgba(255, 80, 80, 0.18));
  }

  &:active {
    transform: scale(0.97);
  }
}
</style>
