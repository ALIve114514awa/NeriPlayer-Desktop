<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { useRouter } from 'vue-router'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { usePlayerStore } from '@/stores/player'
import { useSyncStore } from '@/stores/sync'
import { useSettingsStore } from '@/stores/settings'
import { useAuthStore } from '@/stores/auth'
import { useToastStore } from '@/stores/toast'
import { createLogger } from '@/utils/logger'

const log = createLogger('debug-view')

const router = useRouter()
const { t } = useI18n()
const player = usePlayerStore()
const syncStore = useSyncStore()
const settingsStore = useSettingsStore()
const authStore = useAuthStore()
const toast = useToastStore()

const buildInfo = ref<{
  app_version?: string
  version?: string
  uuid: string
  timestamp: string
} | null>(null)
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
  probes.value.netease = await probePlatform('netease')
}

async function testBilibili() {
  probes.value.bilibili = 'testing'
  probes.value.bilibili = await probePlatform('bilibili')
}

async function testYouTube() {
  probes.value.youtube = 'testing'
  probes.value.youtube = await probePlatform('youtube')
}

// 探针语义：收到任何 HTTP 响应都算连通，只有传输层失败才算不通。
// 之前调「取具体视频播放地址」，视频下架/缺参这类业务失败被误报成不通
async function probePlatform(platform: string): Promise<'success' | 'failed'> {
  try {
    const reachable = await invoke<boolean>('probe_platform_connectivity', { platform })
    return reachable ? 'success' : 'failed'
  } catch {
    return 'failed'
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
    log.error('Failed to clear debug cookies:', error)
    toast.error(t('settings.debug_cookie_clear_failed'))
  } finally {
    clearingDebugCookies.value = false
  }
}

async function copyBuildValue(value: string | undefined | null) {
  const text = value?.trim()
  if (!text) return
  try {
    await writeText(text)
    toast.success(t('settings.build_info_copied'))
  } catch (error) {
    log.error('Failed to copy build info:', error)
    toast.error(t('settings.build_info_copy_failed'))
  }
}

onMounted(async () => {
  try {
    const info = await invoke<{
      app_version?: string
      build_uuid: string
      build_timestamp: string
      version: string
    }>('get_build_info')
    buildInfo.value = {
      app_version: info.app_version,
      version: info.version,
      uuid: info.build_uuid,
      timestamp: info.build_timestamp,
    }
  } catch { /* 命令不存在时忽略 */ }
  try {
    appDataDir.value = await invoke('get_app_data_dir')
  } catch { /* 忽略 */ }
  await loadDebugCookieStorageStatus()
  await refreshLogs()
  await refreshCrashes()
  logsTimer = window.setInterval(() => {
    if (!logsPaused.value) void refreshLogs()
  }, 1500)
})

onUnmounted(() => {
  if (logsTimer) window.clearInterval(logsTimer)
})

// ===== 运行日志 =====
interface RecentLogEntry { timestamp_ms: number; level: string; target: string; message: string }
const logs = ref<RecentLogEntry[]>([])
const logsPaused = ref(false)
const logLevelFilter = ref('')
let logsTimer: number | null = null

async function refreshLogs() {
  try {
    logs.value = await invoke<RecentLogEntry[]>('get_recent_logs', {
      limit: 300,
      minLevel: logLevelFilter.value || null,
    })
  } catch { /* 命令不可用时静默 */ }
}

function logTime(ms: number): string {
  const date = new Date(ms)
  const pad = (v: number, n = 2) => String(v).padStart(n, '0')
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`
}

function levelColor(level: string): string {
  switch (level) {
    case 'ERROR': return 'var(--md-error)'
    case 'WARN': return '#ffa726'
    case 'DEBUG': return 'var(--md-on-surface-variant)'
    case 'TRACE': return 'var(--md-outline)'
    default: return 'var(--md-primary)'
  }
}

async function copyLogs() {
  const text = [...logs.value].reverse()
    .map(entry => `${logTime(entry.timestamp_ms)} [${entry.target}] [${entry.level}] ${entry.message}`)
    .join('\n')
  try {
    const { writeText } = await import('@tauri-apps/plugin-clipboard-manager')
    await writeText(text)
    toast.success(t('settings.debug_logs_copied'))
  } catch (e) {
    log.error('copy logs failed:', e)
  }
}

async function exportReport() {
  try {
    const path = await invoke<string>('export_debug_report')
    toast.success(t('settings.debug_logs_exported'))
    await invoke('reveal_in_file_manager', { path })
  } catch (e) {
    log.error('export report failed:', e)
  }
}

async function openLogDir() {
  try {
    const dir = await invoke<string>('get_log_dir')
    await invoke('reveal_in_file_manager', { path: dir })
  } catch (e) {
    log.error('open log dir failed:', e)
  }
}

// ===== 崩溃报告 =====
interface CrashReportInfo { file_name: string; size_bytes: number; modified_ms: number }
const crashes = ref<CrashReportInfo[]>([])
const expandedCrash = ref('')
const crashContent = ref('')

async function refreshCrashes() {
  try {
    crashes.value = await invoke<CrashReportInfo[]>('list_crash_reports')
  } catch { /* 静默 */ }
}

async function toggleCrash(name: string) {
  if (expandedCrash.value === name) {
    expandedCrash.value = ''
    return
  }
  try {
    crashContent.value = await invoke<string>('read_crash_report', { fileName: name })
    expandedCrash.value = name
  } catch (e) {
    log.error('read crash failed:', e)
  }
}

async function clearCrashes() {
  try {
    await invoke('clear_crash_reports')
    expandedCrash.value = ''
    await refreshCrashes()
    toast.success(t('settings.debug_crashes_cleared'))
  } catch (e) {
    log.error('clear crashes failed:', e)
  }
}
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
          <div class="state-item copyable-state-item" @click="copyBuildValue(buildInfo.version)">
            <span class="state-label">{{ t('settings.build_version_label') }}</span>
            <span class="state-value mono">{{ buildInfo.version || '—' }}</span>
          </div>
          <div class="state-item copyable-state-item" @click="copyBuildValue(buildInfo.uuid)">
            <span class="state-label">{{ t('settings.build_uuid') }}</span>
            <span class="state-value mono">{{ buildInfo.uuid }}</span>
          </div>
          <div class="state-item copyable-state-item" @click="copyBuildValue(buildInfo.timestamp)">
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

    <!-- 运行日志 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">terminal</span>
      <span>{{ t('settings.debug_logs') }}</span>
    </div>
    <div class="setting-card debug-logs-card">
      <div class="debug-logs-toolbar">
        <select v-model="logLevelFilter" class="debug-logs-level" @change="refreshLogs">
          <option value="">{{ t('settings.debug_level_all') }}</option>
          <option value="error">ERROR</option>
          <option value="warn">WARN</option>
          <option value="info">INFO</option>
          <option value="debug">DEBUG</option>
        </select>
        <div class="debug-logs-actions">
          <button class="debug-log-btn" @click="logsPaused = !logsPaused">
            <span class="material-symbols-rounded">{{ logsPaused ? 'play_arrow' : 'pause' }}</span>
            {{ logsPaused ? t('settings.debug_logs_resume') : t('settings.debug_logs_pause') }}
          </button>
          <button class="debug-log-btn" @click="copyLogs">
            <span class="material-symbols-rounded">content_copy</span>
            {{ t('settings.debug_logs_copy') }}
          </button>
          <button class="debug-log-btn" @click="exportReport">
            <span class="material-symbols-rounded">ios_share</span>
            {{ t('settings.debug_logs_export') }}
          </button>
          <button class="debug-log-btn" @click="openLogDir">
            <span class="material-symbols-rounded">folder_open</span>
            {{ t('settings.debug_logs_open_dir') }}
          </button>
        </div>
      </div>
      <div v-if="logs.length === 0" class="debug-logs-empty">{{ t('settings.debug_logs_empty') }}</div>
      <div v-else class="debug-logs-list">
        <div v-for="(entry, index) in logs" :key="`${entry.timestamp_ms}-${index}`" class="debug-log-line">
          <span class="debug-log-time">{{ logTime(entry.timestamp_ms) }}</span>
          <span class="debug-log-level" :style="{ color: levelColor(entry.level) }">{{ entry.level }}</span>
          <span class="debug-log-target">[{{ entry.target }}]</span>
          <span class="debug-log-msg">{{ entry.message }}</span>
        </div>
      </div>
    </div>

    <!-- 崩溃报告 -->
    <div class="section-label">
      <span class="material-symbols-rounded" style="font-size: 18px">report</span>
      <span>{{ t('settings.debug_crashes') }}</span>
    </div>
    <div class="setting-card">
      <div class="setting-info" style="display: flex; align-items: center; gap: 12px">
        <div style="flex: 1; min-width: 0">
          <div class="setting-title">{{ t('settings.debug_crashes') }}</div>
          <div class="setting-desc">{{ t('settings.debug_crashes_desc') }}</div>
        </div>
        <button v-if="crashes.length > 0" class="debug-log-btn debug-crash-clear" @click="clearCrashes">
          <span class="material-symbols-rounded">delete</span>
          {{ t('settings.debug_crashes_clear') }}
        </button>
      </div>
      <div v-if="crashes.length === 0" class="debug-logs-empty">{{ t('settings.debug_crashes_empty') }}</div>
      <div v-else class="debug-crash-list">
        <div v-for="report in crashes" :key="report.file_name" class="debug-crash-item">
          <button class="debug-crash-row" @click="toggleCrash(report.file_name)">
            <span class="material-symbols-rounded" style="font-size: 18px">
              {{ expandedCrash === report.file_name ? 'expand_less' : 'expand_more' }}
            </span>
            <span class="debug-crash-name">{{ report.file_name }}</span>
            <span class="debug-crash-size">{{ (report.size_bytes / 1024).toFixed(1) }} KB</span>
          </button>
          <pre v-if="expandedCrash === report.file_name" class="debug-crash-body">{{ crashContent }}</pre>
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

  &.copyable-state-item {
    cursor: pointer;
    border-radius: 8px;
    padding: 4px 6px;
    margin: -4px -6px;
    transition: background var(--duration-short, 0.15s);

    &:hover {
      background: var(--md-surface-container-high, rgba(255, 255, 255, 0.06));
    }
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

.debug-logs-card { display: flex; flex-direction: column; gap: 10px; }

.debug-logs-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.debug-logs-level {
  height: 32px;
  padding: 0 10px;
  border-radius: 10px;
  background: var(--md-surface-container);
  color: var(--md-on-surface);
  border: 1px solid var(--md-outline-variant);
  font-size: 12px;
}

.debug-logs-actions { display: flex; gap: 6px; flex-wrap: wrap; margin-left: auto; }

.debug-log-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--radius-full);
  background: var(--md-surface-container);
  color: var(--md-on-surface-variant);
  font-size: 12px;
  transition: background 150ms;

  .material-symbols-rounded { font-size: 16px; }
  &:hover { background: var(--md-surface-container-highest); color: var(--md-on-surface); }
}

.debug-crash-clear { color: var(--md-error); }

.debug-logs-list {
  max-height: 320px;
  overflow-y: auto;
  display: flex;
  flex-direction: column-reverse; /* 新日志在底部，滚动锚在最新 */
  gap: 2px;
  font-family: ui-monospace, 'SF Mono', Menlo, monospace;
  font-size: 11px;
  line-height: 1.5;
  background: var(--md-surface-container-lowest, var(--md-surface));
  border-radius: 12px;
  padding: 10px 12px;
}

.debug-log-line { display: flex; gap: 8px; align-items: baseline; }
.debug-log-time { color: var(--md-outline); flex-shrink: 0; }
.debug-log-level { width: 44px; flex-shrink: 0; font-weight: 700; }
.debug-log-target { color: var(--md-on-surface-variant); flex-shrink: 0; }
.debug-log-msg { word-break: break-all; white-space: pre-wrap; }

.debug-logs-empty {
  padding: 20px;
  text-align: center;
  font-size: 13px;
  color: var(--md-on-surface-variant);
}

.debug-crash-list { display: flex; flex-direction: column; gap: 6px; margin-top: 8px; }

.debug-crash-item {
  border-radius: 12px;
  background: var(--md-surface-container);
  overflow: hidden;
}

.debug-crash-row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 10px 12px;
  color: var(--md-on-surface);
  font-size: 13px;
  transition: background 150ms;

  &:hover { background: var(--md-surface-container-high); }
}

.debug-crash-name { flex: 1; min-width: 0; text-align: left; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, monospace; }
.debug-crash-size { color: var(--md-on-surface-variant); font-size: 12px; flex-shrink: 0; }

.debug-crash-body {
  margin: 0;
  padding: 12px;
  max-height: 280px;
  overflow: auto;
  font-family: ui-monospace, 'SF Mono', Menlo, monospace;
  font-size: 11px;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-all;
  border-top: 1px solid var(--md-outline-variant);
  color: var(--md-on-surface-variant);
}
</style>
