<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import type { UnlistenFn } from '@tauri-apps/api/event'

const props = defineProps<{
  forceLight?: boolean
  nowPlaying?: boolean
  trackName?: string
  albumName?: string
  transitioning?: boolean
  transitionState?: 'opening' | 'closing' | null
}>()

const emit = defineEmits<{
  collapse: []
  toggleMore: []
}>()

const { t } = useI18n()
const titleBarTrackText = computed(() => props.albumName || props.trackName || '')
const titleBarTrackKey = computed(() => `${props.nowPlaying ? 'np' : 'base'}:${titleBarTrackText.value || 'empty'}`)
const titleBarSideKey = computed(() => props.nowPlaying ? 'np' : 'brand')

const isMaximized = ref(false)
let unlistenResize: UnlistenFn | null = null

// macOS 使用系统原生红绿灯，隐藏自绘控制并在左侧留出安全区。
// 通过 navigator 判定平台（reqwest 层的 UA spoof 不影响 webview 的 navigator）。
const isMac = /Mac|iPhone|iPad/.test(
  (navigator as any).userAgentData?.platform || navigator.platform || navigator.userAgent
)

const appWindow = getCurrentWindow()

async function refreshMaximized() {
  try {
    isMaximized.value = await appWindow.isMaximized()
  } catch {
    isMaximized.value = false
  }
}

function minimize() {
  appWindow.minimize().catch(() => {})
}

function toggleMaximize() {
  appWindow.toggleMaximize().then(refreshMaximized).catch(() => {})
}

function close() {
  appWindow.close().catch(() => {})
}

onMounted(async () => {
  await refreshMaximized()
  try {
    unlistenResize = await appWindow.onResized(() => refreshMaximized())
  } catch {}
})

onUnmounted(() => {
  if (unlistenResize) unlistenResize()
})
</script>

<template>
  <header
    class="title-bar"
    :class="{
      'tb-force-light': forceLight,
      'tb-np-mode': nowPlaying,
      'tb-transitioning': transitioning,
      'tb-opening': transitionState === 'opening',
      'tb-closing': transitionState === 'closing',
      'tb-mac': isMac,
    }"
    data-tauri-drag-region
  >
    <!-- macOS 原生红绿灯安全区（约 78px），避免内容压住系统交通灯 -->
    <div v-if="isMac" class="tb-traffic-safe-area" data-tauri-drag-region></div>

    <div class="tb-left-slot">
      <transition name="tb-side-swap" mode="out-in">
        <!-- 普通模式：logo + 名称 -->
        <div v-if="!nowPlaying" :key="titleBarSideKey" class="tb-brand" data-tauri-drag-region>
          <img src="/app-icon.png" alt="logo" class="tb-icon" />
          <span class="tb-title">NeriPlayer</span>
        </div>

        <!-- 播放器模式：折叠按钮（覆盖 logo 区域） -->
        <div v-else :key="titleBarSideKey" class="tb-np-left">
          <button class="tb-np-btn tb-np-collapse" type="button" data-tauri-drag-region="false" @click="emit('collapse')">
            <span class="material-symbols-rounded">keyboard_arrow_down</span>
          </button>
        </div>
      </transition>
    </div>

    <!-- 播放器模式：居中播放信息 -->
    <transition name="tb-center-fade">
      <div v-if="nowPlaying" class="tb-np-center" data-tauri-drag-region>
        <span class="tb-np-label">{{ t('player.now_playing') }}</span>
        <transition name="tb-track-swap" mode="out-in">
          <span :key="titleBarTrackKey" class="tb-np-track">{{ titleBarTrackText }}</span>
        </transition>
      </div>
    </transition>

    <!-- 拖拽占位 -->
    <div class="tb-drag" data-tauri-drag-region></div>
    <div v-if="isMac" class="tb-drag" data-tauri-drag-region></div>

    <div class="tb-right-slot">
      <transition name="tb-side-swap" mode="out-in">
        <!-- 播放器模式：更多按钮 -->
        <button v-if="nowPlaying" :key="`more:${titleBarSideKey}`" class="tb-np-btn tb-np-more" type="button" data-tauri-drag-region="false" @click="emit('toggleMore')">
          <span class="material-symbols-rounded">more_vert</span>
        </button>
        <div v-else :key="`spacer:${titleBarSideKey}`" class="tb-np-more-spacer" aria-hidden="true"></div>
      </transition>
    </div>

    <!-- 窗口控制（macOS 使用系统原生红绿灯，此处隐藏） -->
    <div v-if="!isMac" class="tb-controls">
      <button class="tb-ctrl" type="button" @click="minimize" title="最小化">
        <svg width="12" height="12" viewBox="0 0 12 12">
          <rect x="2" y="5.5" width="8" height="1" fill="currentColor" />
        </svg>
      </button>
      <button class="tb-ctrl" type="button" @click="toggleMaximize" :title="isMaximized ? '还原' : '最大化'">
        <svg v-if="!isMaximized" width="12" height="12" viewBox="0 0 12 12">
          <rect x="2.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1" />
        </svg>
        <svg v-else width="12" height="12" viewBox="0 0 12 12">
          <rect x="3.5" y="2" width="6.5" height="6.5" fill="none" stroke="currentColor" stroke-width="1" />
          <rect x="2" y="3.5" width="6.5" height="6.5" fill="var(--md-background)" stroke="currentColor" stroke-width="1" />
        </svg>
      </button>
      <button class="tb-ctrl tb-close" type="button" @click="close" title="关闭">
        <svg width="12" height="12" viewBox="0 0 12 12">
          <line x1="2.5" y1="2.5" x2="9.5" y2="9.5" stroke="currentColor" stroke-width="1" />
          <line x1="9.5" y1="2.5" x2="2.5" y2="9.5" stroke="currentColor" stroke-width="1" />
        </svg>
      </button>
    </div>
  </header>
</template>

<style scoped lang="scss">
.title-bar {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  height: 36px;
  display: flex;
  align-items: stretch;
  background: transparent;
  color: var(--md-on-surface);
  user-select: none;
  z-index: 1000;
  pointer-events: none;
  transition: color 300ms var(--ease-standard),
              height 300ms var(--ease-standard),
              background 320ms var(--ease-standard),
              backdrop-filter 320ms var(--ease-standard),
              box-shadow 320ms var(--ease-standard),
              padding 320ms var(--ease-standard);

  &.tb-np-mode {
    height: 56px;
    padding-top: 8px;
  }
}

.title-bar.tb-transitioning {
  background: linear-gradient(to bottom, rgba(14, 13, 18, 0.26), rgba(14, 13, 18, 0));
  backdrop-filter: blur(14px) saturate(1.08);
  box-shadow: inset 0 -1px 0 rgba(255, 255, 255, 0.04);
}

.title-bar.tb-opening {
  animation: tb-shell-breathe-in 420ms cubic-bezier(0.22, 1, 0.36, 1);
}

.title-bar.tb-closing {
  animation: tb-shell-breathe-out 320ms cubic-bezier(0.22, 1, 0.36, 1);
}

.title-bar.tb-force-light {
  color: rgba(255, 255, 255, 0.9);
}

.tb-brand,
.tb-controls,
.tb-ctrl,
.tb-left-slot,
.tb-np-left,
.tb-np-btn,
.tb-np-more,
.tb-right-slot {
  pointer-events: auto;
}

.tb-left-slot,
.tb-right-slot {
  display: flex;
  align-items: center;
}

/* ========== 普通模式 ========== */
.tb-brand {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 0 14px;
  -webkit-app-region: drag;
}

.tb-icon {
  width: 18px;
  height: 18px;
  border-radius: 4px;
  object-fit: contain;
  pointer-events: none;
  opacity: 0.95;
}

.tb-title {
  font-size: 12px;
  font-weight: 600;
  color: inherit;
  letter-spacing: 0.2px;
  pointer-events: none;
  opacity: 0.85;
}

/* ========== 播放器模式 ========== */
.tb-np-left {
  display: flex;
  align-items: center;
  padding: 0 8px;
  -webkit-app-region: no-drag;
}

.tb-np-btn {
  width: 40px;
  height: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  border-radius: var(--radius-full);
  color: inherit;
  opacity: 0.8;
  cursor: pointer;
  transition: background var(--duration-short) var(--ease-standard),
              opacity var(--duration-short) var(--ease-standard);

  &:hover {
    background: rgba(255, 255, 255, 0.1);
    opacity: 1;
  }

  .material-symbols-rounded {
    font-size: 24px;
  }
}

.tb-np-collapse .material-symbols-rounded {
  font-size: 30px;
}

.tb-np-center {
  position: absolute;
  left: 50%;
  top: 50%;
  transform: translate(-50%, -50%);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 1px;
  pointer-events: auto;
  -webkit-app-region: drag;
  min-width: 0;
}

.tb-np-label {
  font-size: 10px;
  font-weight: 600;
  color: inherit;
  opacity: 0.45;
  text-transform: uppercase;
  letter-spacing: 1px;
  line-height: 1.2;
}

.tb-np-track {
  font-size: 12px;
  font-weight: 600;
  color: inherit;
  opacity: 0.85;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 300px;
  line-height: 1.2;
}

.tb-transitioning .tb-np-center {
  animation: tb-center-float 420ms cubic-bezier(0.22, 1, 0.36, 1);
}

.tb-transitioning .tb-np-btn,
.tb-transitioning .tb-ctrl {
  transition:
    background var(--duration-short) var(--ease-standard),
    opacity var(--duration-short) var(--ease-standard),
    transform 220ms cubic-bezier(0.22, 1, 0.36, 1),
    box-shadow 220ms cubic-bezier(0.22, 1, 0.36, 1);
}

.tb-opening .tb-np-btn,
.tb-opening .tb-ctrl {
  transform: translateY(1px) scale(0.98);
}

.tb-closing .tb-np-btn,
.tb-closing .tb-ctrl {
  transform: translateY(-1px) scale(0.985);
}

.tb-track-swap-enter-active,
.tb-track-swap-leave-active {
  transition: opacity 220ms ease, transform 280ms cubic-bezier(0.22, 1, 0.36, 1);
}

.tb-track-swap-enter-from {
  opacity: 0;
  transform: translateY(8px);
}

.tb-track-swap-leave-to {
  opacity: 0;
  transform: translateY(-8px);
}

.tb-side-swap-enter-active,
.tb-side-swap-leave-active {
  transition: opacity 220ms ease, transform 280ms cubic-bezier(0.22, 1, 0.36, 1);
}

.tb-side-swap-enter-from {
  opacity: 0;
  transform: translateX(-10px) scale(0.96);
}

.tb-side-swap-leave-to {
  opacity: 0;
  transform: translateX(10px) scale(0.96);
}

.tb-center-fade-enter-active,
.tb-center-fade-leave-active {
  transition: opacity 240ms ease, transform 300ms cubic-bezier(0.22, 1, 0.36, 1);
}

.tb-center-fade-enter-from,
.tb-center-fade-leave-to {
  opacity: 0;
  transform: translate(-50%, calc(-50% + 8px));
}

.tb-np-more-spacer {
  width: 40px;
  height: 40px;
}

.tb-np-more {
  align-self: center;
  margin-right: 4px;
}

/* 播放器模式下窗口控制按钮适配更高的顶栏 */
.tb-np-mode .tb-ctrl {
  width: 40px;
  height: 40px;
  align-self: center;
  border-radius: var(--radius-full);
}

.tb-np-mode .tb-ctrl svg {
  width: 14px;
  height: 14px;
}

.tb-np-mode .tb-controls {
  align-items: center;
  gap: 2px;
  padding-right: 8px;
}

/* ========== 公共 ========== */
.tb-drag {
  flex: 1;
  -webkit-app-region: drag;
  pointer-events: auto;
}

/* macOS 原生红绿灯安全区：系统交通灯位于左上 ~12px 起、宽约 78px */
.tb-traffic-safe-area {
  width: 78px;
  height: 100%;
  flex: 0 0 auto;
  -webkit-app-region: drag;
  pointer-events: auto;
}

/* mac 下普通模式左侧品牌区右移，避免压住系统红绿灯 */
.tb-mac.title-bar:not(.tb-np-mode) .tb-brand {
  padding-left: 4px;
}

/* macOS np 模式：56px 高 + padding-top 会把控制行下压，
   与系统固定在顶部的红绿灯错位。此处将左右控制行顶部对齐，
   使折叠/收藏/更多按钮与红绿灯同高。 */
.tb-mac.tb-np-mode .tb-left-slot,
.tb-mac.tb-np-mode .tb-right-slot,
.tb-mac.tb-np-mode .tb-controls {
  align-self: flex-start;
}

.tb-mac.tb-np-mode {
  padding-top: 0;
}

/* np 模式下 mac 安全区跟随 56px 栏高 */
.tb-mac.tb-np-mode .tb-traffic-safe-area {
  height: 56px;
}

.tb-controls {
  display: flex;
  align-items: stretch;
  -webkit-app-region: no-drag;
}

.tb-ctrl {
  width: 46px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  color: inherit;
  opacity: 0.7;
  cursor: pointer;
  transition: background var(--duration-short) var(--ease-standard),
              opacity var(--duration-short) var(--ease-standard);

  &:hover {
    background: rgba(255, 255, 255, 0.08);
    opacity: 1;
  }
}

.light-theme .title-bar:not(.tb-force-light) .tb-ctrl:hover {
  background: rgba(0, 0, 0, 0.06);
}

.light-theme .title-bar:not(.tb-force-light) .tb-np-btn:hover {
  background: rgba(0, 0, 0, 0.06);
}

.tb-close:hover {
  background: #e81123 !important;
  color: #fff;
  opacity: 1;
}

@keyframes tb-shell-breathe-in {
  0% {
    background: linear-gradient(to bottom, rgba(14, 13, 18, 0.12), rgba(14, 13, 18, 0));
    backdrop-filter: blur(6px) saturate(1.02);
  }
  100% {
    background: linear-gradient(to bottom, rgba(14, 13, 18, 0.26), rgba(14, 13, 18, 0));
    backdrop-filter: blur(14px) saturate(1.08);
  }
}

@keyframes tb-shell-breathe-out {
  0% {
    background: linear-gradient(to bottom, rgba(14, 13, 18, 0.24), rgba(14, 13, 18, 0));
    backdrop-filter: blur(14px) saturate(1.08);
  }
  100% {
    background: linear-gradient(to bottom, rgba(14, 13, 18, 0.08), rgba(14, 13, 18, 0));
    backdrop-filter: blur(4px) saturate(1.02);
  }
}

@keyframes tb-center-float {
  0% {
    opacity: 0.72;
    transform: translate(-50%, calc(-50% + 8px)) scale(0.985);
  }
  100% {
    opacity: 1;
    transform: translate(-50%, -50%) scale(1);
  }
}
</style>
