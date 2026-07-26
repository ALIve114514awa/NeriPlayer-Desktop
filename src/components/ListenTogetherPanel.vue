<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { useListenTogetherStore } from '@/stores/listenTogether'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{ 'update:open': [value: boolean] }>()

const lt = useListenTogetherStore()
const { t } = useI18n()

function close() {
  emit('update:open', false)
}

function handleKeydown(event: KeyboardEvent) {
  if (!props.open || event.key !== 'Escape' || event.defaultPrevented) return
  // 消费掉 ESC, 阻止全局快捷键继续关闭下层 (对齐 Android 返回语义)
  event.preventDefault()
  close()
}

const joinRoomId = ref('')
const nowTick = ref(Date.now())
let nowTimer: ReturnType<typeof setInterval> | null = null
const CONTROLLER_GRACE_PERIOD_MS = 10 * 60 * 1000

const statusColor = computed(() => {
  switch (lt.connectionState) {
    case 'connected': return 'var(--md-primary)'
    case 'connecting': return 'var(--md-tertiary)'
    default: return 'var(--md-outline)'
  }
})

const statusText = computed(() => {
  switch (lt.connectionState) {
    case 'connected': return t('listen_together.connected')
    case 'connecting': return lt.roomId ? t('listen_together.reconnecting') : t('listen_together.connecting')
    default: return t('listen_together.disconnected')
  }
})

const roomStatusText = computed(() => {
  switch (lt.roomState?.roomStatus) {
    case 'controller_offline': return t('listen_together.room_status_controller_offline')
    case 'closed': return t('listen_together.room_status_closed')
    default: return t('listen_together.room_status_active')
  }
})

const roomStatusTone = computed(() => {
  switch (lt.roomState?.roomStatus) {
    case 'controller_offline': return 'warning'
    case 'closed': return 'danger'
    default: return 'success'
  }
})

const controllerOfflineHint = computed(() => {
  const offlineSince = lt.roomState?.controllerOfflineSince
  if (!offlineSince || lt.roomState?.roomStatus !== 'controller_offline') return ''

  const remainingMs = Math.max(0, offlineSince + CONTROLLER_GRACE_PERIOD_MS - nowTick.value)
  const remainingMinutes = Math.max(1, Math.ceil(remainingMs / 60_000))
  return t('listen_together.controller_offline_detail', { minutes: remainingMinutes })
})

const latestSyncLabel = computed(() => {
  const eventType = lt.lastSyncEventType
  switch (eventType) {
    case 'PLAY': return t('listen_together.sync_play')
    case 'PAUSE': return t('listen_together.sync_pause')
    case 'SEEK': return t('listen_together.sync_seek')
    case 'SET_TRACK': return t('listen_together.sync_track')
    case 'UPDATE_SETTINGS': return t('listen_together.sync_settings')
    case 'HEARTBEAT': return t('listen_together.sync_heartbeat')
    case 'ROOM_RESUMED':
    case 'RECONNECTED':
      return t('listen_together.sync_reconnected')
    case 'ROOM_SUSPENDED':
      return t('listen_together.sync_suspended')
    case 'INITIAL_STATE':
    case 'WELCOME':
    case 'STATE_SYNC':
      return t('listen_together.sync_state')
    default:
      return eventType || t('listen_together.sync_unknown')
  }
})

const latestSyncTime = computed(() => formatRelativeTime(lt.lastSyncAt))
const latestRoomUpdateTime = computed(() => formatRelativeTime(lt.roomState?.updatedAt))
const controllerHeartbeatTime = computed(() => formatRelativeTime(lt.roomState?.controllerHeartbeatAt))

const connectedBanner = computed(() => {
  if (lt.roomState?.roomStatus === 'controller_offline') {
    return {
      tone: 'warning',
      icon: 'wifi_off',
      title: t('listen_together.controller_offline'),
      desc: controllerOfflineHint.value,
    }
  }

  if (lt.lastSyncEventType === 'ROOM_RESUMED' || lt.lastSyncEventType === 'RECONNECTED') {
    return {
      tone: 'success',
      icon: 'sync',
      title: t('listen_together.sync_reconnected'),
      desc: lt.lastReconnectAt ? formatRelativeTime(lt.lastReconnectAt) : '',
    }
  }

  return null
})

function handleCreate() {
  lt.createRoom()
}

function handleJoin() {
  if (!joinRoomId.value.trim()) return
  lt.joinRoom(joinRoomId.value.trim())
}

function handleLeave() {
  lt.leaveRoom()
}

// 剪贴板检测
async function checkClipboard() {
  const invite = await lt.checkClipboardInvite()
  if (invite) {
    joinRoomId.value = invite.roomId
    if (invite.baseUrl) {
      lt.baseUrl = invite.baseUrl
    }
  }
}

function formatRelativeTime(timestamp?: number | null) {
  if (!timestamp) return t('listen_together.not_available')

  const diff = Math.max(0, nowTick.value - timestamp)
  const minutes = Math.floor(diff / 60_000)
  const hours = Math.floor(diff / 3_600_000)
  const days = Math.floor(diff / 86_400_000)

  if (minutes < 1) return t('recent.just_now')
  if (hours < 1) return t('recent.minutes_ago', { count: minutes })
  if (days < 1) return t('recent.hours_ago', { count: hours })
  return t('recent.days_ago', { count: days })
}

// 组件常驻挂载, 剪贴板检测跟随弹窗打开时机
watch(() => props.open, (open) => {
  if (open) checkClipboard()
})

function checkClipboardWhenOpen() {
  if (props.open) checkClipboard()
}

onMounted(() => {
  nowTimer = setInterval(() => {
    nowTick.value = Date.now()
  }, 15_000)
  if (props.open) checkClipboard()
  window.addEventListener('focus', checkClipboardWhenOpen)
  document.addEventListener('keydown', handleKeydown)
})

onUnmounted(() => {
  if (nowTimer) clearInterval(nowTimer)
  window.removeEventListener('focus', checkClipboardWhenOpen)
  document.removeEventListener('keydown', handleKeydown)
})
</script>

<template>
  <Teleport to="body">
    <Transition name="dialog">
      <div v-if="open" class="lt-overlay" @click="close">
        <div
          class="lt-dialog"
          role="dialog"
          aria-modal="true"
          :aria-label="t('listen_together.title')"
          @click.stop
        >
    <header class="lt-header">
      <div class="lt-header-icon">
        <span class="material-symbols-rounded">speaker_group</span>
      </div>
      <div class="lt-header-text">
        <h3>{{ t('listen_together.title') }}</h3>
        <div class="lt-status">
          <span class="lt-status-dot" :style="{ background: statusColor }" />
          <span class="lt-status-text">{{ statusText }}</span>
        </div>
      </div>
      <button class="lt-close" @click="close">
        <span class="material-symbols-rounded">close</span>
      </button>
    </header>

    <!-- 未连接状态 -->
    <div v-if="!lt.isConnected && lt.connectionState !== 'connecting'" class="lt-body">
      <div class="lt-field">
        <label>{{ t('listen_together.nickname') }}</label>
        <input v-model="lt.nickname" type="text" class="lt-input" maxlength="20" />
      </div>

      <div class="lt-field">
        <label>{{ t('listen_together.room_id') }}</label>
        <input
          v-model="joinRoomId"
          type="text"
          class="lt-input"
          :placeholder="t('listen_together.room_id_placeholder')"
        />
      </div>

      <div class="lt-actions">
        <button class="lt-btn primary" @click="handleCreate">
          <span class="material-symbols-rounded">add</span>
          {{ t('listen_together.create_room') }}
        </button>
        <button class="lt-btn" :disabled="!joinRoomId.trim()" @click="handleJoin">
          <span class="material-symbols-rounded">login</span>
          {{ t('listen_together.join_room') }}
        </button>
      </div>

      <div v-if="lt.sessionError" class="lt-error">{{ lt.sessionError }}</div>
    </div>

    <!-- 连接中 -->
    <div v-else-if="lt.connectionState === 'connecting'" class="lt-body lt-center">
      <span class="material-symbols-rounded spinning">progress_activity</span>
      <span>{{ lt.roomId ? t('listen_together.reconnecting') : t('listen_together.connecting') }}</span>
      <small class="lt-center-desc">
        {{ lt.roomId ? t('listen_together.reconnecting_desc') : t('listen_together.connecting_desc') }}
      </small>
    </div>

    <!-- 已连接状态 -->
    <div v-else class="lt-body">
      <div class="lt-room-info">
        <div class="lt-room-id">
          <span class="lt-label">{{ t('listen_together.room_id') }}</span>
          <span class="lt-value">{{ lt.roomId }}</span>
          <button class="lt-icon-btn" @click="lt.copyInviteLink()" :title="t('listen_together.copy_invite')">
            <span class="material-symbols-rounded">content_copy</span>
          </button>
        </div>
        <div class="lt-role">
          <span class="lt-role-badge" :class="lt.role || ''">
            {{ lt.isController ? t('listen_together.role_controller') : t('listen_together.role_listener') }}
          </span>
        </div>
      </div>

      <div v-if="connectedBanner" class="lt-banner" :class="connectedBanner.tone">
        <span class="material-symbols-rounded">{{ connectedBanner.icon }}</span>
        <div class="lt-banner-content">
          <strong>{{ connectedBanner.title }}</strong>
          <small v-if="connectedBanner.desc">{{ connectedBanner.desc }}</small>
        </div>
      </div>

      <div class="lt-section">
        <h4>{{ t('listen_together.sync_status') }}</h4>
        <div class="lt-meta-grid">
          <div class="lt-meta-card">
            <span class="lt-meta-label">{{ t('listen_together.room_version') }}</span>
            <strong class="lt-meta-value">#{{ lt.roomState?.version ?? 0 }}</strong>
          </div>
          <div class="lt-meta-card">
            <span class="lt-meta-label">{{ t('listen_together.room_status') }}</span>
            <strong class="lt-status-chip" :class="roomStatusTone">{{ roomStatusText }}</strong>
          </div>
          <div class="lt-meta-card">
            <span class="lt-meta-label">{{ t('listen_together.recent_sync') }}</span>
            <strong class="lt-meta-value">{{ latestSyncLabel }}</strong>
            <small>{{ latestSyncTime }}</small>
          </div>
          <div class="lt-meta-card">
            <span class="lt-meta-label">{{ t('listen_together.last_room_update') }}</span>
            <strong class="lt-meta-value">{{ latestRoomUpdateTime }}</strong>
          </div>
          <div class="lt-meta-card">
            <span class="lt-meta-label">{{ t('listen_together.controller_heartbeat') }}</span>
            <strong class="lt-meta-value">{{ controllerHeartbeatTime }}</strong>
          </div>
        </div>
      </div>

      <!-- 成员列表 -->
      <div class="lt-section">
        <h4>{{ t('listen_together.members') }} ({{ lt.members.length }})</h4>
        <ul class="lt-member-list">
          <li v-for="m in lt.members" :key="m.userUuid" class="lt-member">
            <span class="material-symbols-rounded lt-member-icon">
              {{ m.role === 'controller' ? 'shield' : 'person' }}
            </span>
            <span class="lt-member-name">{{ m.nickname || m.userUuid.slice(0, 8) }}</span>
            <span class="lt-member-role">
              {{ m.role === 'controller' ? t('listen_together.role_controller') : t('listen_together.role_listener') }}
            </span>
          </li>
        </ul>
      </div>

      <!-- 房间设置（仅房主） -->
      <div v-if="lt.isController" class="lt-section">
        <h4>{{ t('listen_together.settings') }}</h4>
        <div class="lt-setting-row">
          <span>{{ t('listen_together.allow_member_control') }}</span>
          <input
            type="checkbox"
            :checked="lt.roomSettings.allowMemberControl"
            @change="lt.updateRoomSettings({ allowMemberControl: ($event.target as HTMLInputElement).checked })"
          />
        </div>
        <div class="lt-setting-row">
          <span>{{ t('listen_together.auto_pause_on_change') }}</span>
          <input
            type="checkbox"
            :checked="lt.roomSettings.autoPauseOnMemberChange"
            @change="lt.updateRoomSettings({ autoPauseOnMemberChange: ($event.target as HTMLInputElement).checked })"
          />
        </div>
        <div class="lt-setting-row">
          <span>{{ t('listen_together.share_audio_links') }}</span>
          <input
            type="checkbox"
            :checked="lt.roomSettings.shareAudioLinks"
            @change="lt.updateRoomSettings({ shareAudioLinks: ($event.target as HTMLInputElement).checked })"
          />
        </div>
      </div>

      <button class="lt-btn danger" @click="handleLeave">
        <span class="material-symbols-rounded">logout</span>
        {{ t('listen_together.leave_room') }}
      </button>
    </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped lang="scss">
.lt-overlay {
  position: fixed;
  inset: 0;
  z-index: 9000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(4px);
}

.lt-dialog {
  width: 400px;
  max-width: calc(100vw - 48px);
  max-height: min(80vh, 680px);
  background: var(--md-surface-container-high);
  border-radius: 28px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  box-shadow:
    0 8px 32px rgba(0, 0, 0, 0.35),
    0 2px 8px rgba(0, 0, 0, 0.2);
}

.lt-header {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 22px 24px 14px;

  h3 {
    font-size: 17px;
    font-weight: 700;
    margin: 0;
    letter-spacing: -0.2px;
  }
}

.lt-header-icon {
  width: 44px;
  height: 44px;
  border-radius: 14px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--md-primary-container);
  color: var(--md-on-primary-container);

  .material-symbols-rounded { font-size: 24px; }
}

.lt-header-text {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.lt-status {
  display: flex;
  align-items: center;
  gap: 6px;
}

.lt-status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  transition: background var(--duration-short, 150ms);
}

.lt-status-text {
  font-size: 12px;
  color: var(--md-on-surface-variant);
}

.lt-close {
  width: 34px;
  height: 34px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  transition: background var(--duration-short, 150ms);
  &:hover { background: var(--md-surface-container-highest); }
  .material-symbols-rounded { font-size: 20px; }
}

.lt-body {
  flex: 1;
  overflow-y: auto;
  padding: 8px 24px 24px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.lt-center {
  align-items: center;
  justify-content: center;
  gap: 8px;
  color: var(--md-on-surface-variant);
}

.lt-center-desc {
  font-size: 12px;
  color: var(--md-on-surface-variant);
}

.lt-field {
  display: flex;
  flex-direction: column;
  gap: 6px;

  label {
    font-size: 12px;
    font-weight: 600;
    color: var(--md-on-surface-variant);
    margin-left: 4px;
  }
}

.lt-input {
  height: 46px;
  padding: 0 16px;
  border-radius: 14px;
  border: 1.5px solid transparent;
  background: var(--md-surface-container);
  color: var(--md-on-surface);
  font-size: 14px;
  outline: none;
  transition: border-color 160ms, background 160ms;

  &::placeholder { color: var(--md-on-surface-variant); opacity: 0.55; }
  &:hover { background: var(--md-surface-container-highest); }
  &:focus {
    border-color: var(--md-primary);
    background: var(--md-surface-container-low);
  }
}

.lt-actions {
  display: flex;
  gap: 10px;
  margin-top: 4px;
}

.lt-btn {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 46px;
  padding: 0 16px;
  border-radius: var(--radius-full);
  font-size: 14px;
  font-weight: 600;
  background: var(--md-secondary-container);
  color: var(--md-on-secondary-container);
  transition: background 150ms, filter 150ms, transform 120ms var(--easing-emphasized, cubic-bezier(0.2, 0, 0, 1));

  .material-symbols-rounded { font-size: 19px; }

  &:hover { filter: brightness(1.05); }
  &:active { transform: scale(0.97); }
  &:disabled { opacity: 0.4; pointer-events: none; }

  &.primary {
    background: var(--md-primary);
    color: var(--md-on-primary);
    &:hover { filter: brightness(1.06); }
  }

  &.danger {
    background: var(--md-error-container);
    color: var(--md-on-error-container);
    &:hover { filter: brightness(1.04); }
  }
}

.lt-error {
  font-size: 12px;
  color: var(--md-error);
  padding: 8px;
  background: var(--md-error-container);
  border-radius: 8px;
}

.lt-room-info {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.lt-room-id {
  display: flex;
  align-items: center;
  gap: 8px;
}

.lt-label {
  font-size: 12px;
  color: var(--md-on-surface-variant);
}

.lt-value {
  font-size: 14px;
  font-weight: 600;
  font-family: monospace;
}

.lt-icon-btn {
  width: 28px;
  height: 28px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--md-on-surface-variant);
  &:hover { background: var(--md-surface-variant); }
  .material-symbols-rounded { font-size: 16px; }
}

.lt-role-badge {
  display: inline-block;
  font-size: 11px;
  font-weight: 500;
  padding: 2px 8px;
  border-radius: 10px;
  background: var(--md-secondary-container);
  color: var(--md-on-secondary-container);

  &.controller {
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
  }
}

.lt-section {
  h4 {
    font-size: 13px;
    font-weight: 600;
    margin: 0 0 6px;
  }
}

.lt-banner {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 10px 12px;
  border-radius: 12px;

  .material-symbols-rounded {
    font-size: 18px;
    margin-top: 1px;
  }

  &.warning {
    background: color-mix(in srgb, var(--md-tertiary-container) 88%, transparent);
    color: var(--md-on-tertiary-container);
  }

  &.success {
    background: color-mix(in srgb, var(--md-primary-container) 88%, transparent);
    color: var(--md-on-primary-container);
  }
}

.lt-banner-content {
  display: flex;
  flex-direction: column;
  gap: 2px;

  strong {
    font-size: 13px;
  }

  small {
    font-size: 11px;
    opacity: 0.8;
  }
}

.lt-meta-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
}

.lt-meta-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 12px;
  background: var(--md-surface-container-low);
  border: 1px solid var(--md-outline-variant);

  small {
    font-size: 11px;
    color: var(--md-on-surface-variant);
  }
}

.lt-meta-label {
  font-size: 11px;
  color: var(--md-on-surface-variant);
}

.lt-meta-value {
  font-size: 13px;
  font-weight: 600;
}

.lt-status-chip {
  display: inline-flex;
  align-items: center;
  width: fit-content;
  padding: 3px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;

  &.success {
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
  }

  &.warning {
    background: var(--md-tertiary-container);
    color: var(--md-on-tertiary-container);
  }

  &.danger {
    background: var(--md-error-container);
    color: var(--md-on-error-container);
  }
}

.lt-member-list {
  list-style: none;
  padding: 0;
  margin: 0;
}

.lt-member {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  font-size: 13px;
}

.lt-member-icon {
  font-size: 18px;
  color: var(--md-on-surface-variant);
}

.lt-member-name {
  flex: 1;
}

.lt-member-role {
  font-size: 11px;
  color: var(--md-on-surface-variant);
}

.spinning {
  animation: spin 1s linear infinite;
}

@keyframes spin { to { transform: rotate(360deg); } }

// 过渡动画（对齐 AddToPlaylistDialog 的房屋风格）
.dialog-enter-active {
  transition: opacity 200ms ease-out;
  .lt-dialog { transition: transform 300ms cubic-bezier(0.05, 0.7, 0.1, 1), opacity 200ms; }
}
.dialog-leave-active {
  transition: opacity 150ms ease-in;
  .lt-dialog { transition: transform 200ms cubic-bezier(0.3, 0, 0.8, 0.15), opacity 150ms; }
}
.dialog-enter-from {
  opacity: 0;
  .lt-dialog { transform: scale(0.85); opacity: 0; }
}
.dialog-leave-to {
  opacity: 0;
  .lt-dialog { transform: scale(0.92); opacity: 0; }
}

@media (prefers-reduced-motion: reduce) {
  .dialog-enter-active,
  .dialog-leave-active,
  .dialog-enter-active .lt-dialog,
  .dialog-leave-active .lt-dialog { transition: none; }
}
</style>
