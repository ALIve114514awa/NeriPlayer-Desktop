<script setup lang="ts">
import { ref, computed, nextTick, onMounted, onUnmounted } from 'vue'
import { usePlayerStore, displayAlbum } from '@/stores/player'
import { useSyncStore } from '@/stores/sync'
import { useAuthStore } from '@/stores/auth'
import { useSettingsStore } from '@/stores/settings'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { convertFileSrc } from '@tauri-apps/api/core'
import MiniPlayer from '@/components/MiniPlayer.vue'
import NowPlaying from '@/components/NowPlaying.vue'
import SideNav from '@/components/SideNav.vue'
import AppToast from '@/components/AppToast.vue'
import TitleBar from '@/components/TitleBar.vue'

type CoverSnapshot = {
  rect: { left: number; top: number; width: number; height: number }
  borderRadius: string
  src: string
}

const player = usePlayerStore()
const settingsStore = useSettingsStore()
const isNowPlayingOpen = ref(false)
const miniPlayerRef = ref<InstanceType<typeof MiniPlayer> | null>(null)
const nowPlayingRef = ref<InstanceType<typeof NowPlaying> | null>(null)
const playerTransitionPulse = ref(0)
const flipOverlay = ref<CoverSnapshot | null>(null)
const flipOverlayStyle = ref<Record<string, string> | null>(null)
const flipTransitionMode = ref<'opening' | 'closing' | null>(null)
const isFlipAnimating = ref(false)
const nowPlayingMotionState = ref<'opening' | 'closing' | null>(null)

const hasMiniPlayer = computed(() => !!player.currentTrack)
const transitionTrackKey = computed(() => player.currentTrack?.id || 'empty')
const hideMiniCoverForFlip = computed(() => isFlipAnimating.value)
const isNowPlayingMotionActive = computed(() => nowPlayingMotionState.value !== null)
let flipAnimationFrame = 0
let flipCleanupTimer: ReturnType<typeof setTimeout> | null = null
let nowPlayingMotionTimer: ReturnType<typeof setTimeout> | null = null

function cancelFlipAnimation() {
  if (flipAnimationFrame) {
    cancelAnimationFrame(flipAnimationFrame)
    flipAnimationFrame = 0
  }
  if (flipCleanupTimer) {
    clearTimeout(flipCleanupTimer)
    flipCleanupTimer = null
  }
}

function resetFlipState() {
  cancelFlipAnimation()
  isFlipAnimating.value = false
  flipTransitionMode.value = null
  flipOverlay.value = null
  flipOverlayStyle.value = null
}

function scheduleNowPlayingMotionCleanup(delay = 520) {
  if (nowPlayingMotionTimer) {
    clearTimeout(nowPlayingMotionTimer)
  }
  nowPlayingMotionTimer = setTimeout(() => {
    nowPlayingMotionTimer = null
    nowPlayingMotionState.value = null
  }, delay)
}

function setFlipStyle(snapshot: CoverSnapshot) {
  flipOverlayStyle.value = {
    left: `${snapshot.rect.left}px`,
    top: `${snapshot.rect.top}px`,
    width: `${snapshot.rect.width}px`,
    height: `${snapshot.rect.height}px`,
    borderRadius: snapshot.borderRadius,
  }
}

function runFlipTransition(from: CoverSnapshot, to: CoverSnapshot, mode: 'opening' | 'closing') {
  cancelFlipAnimation()
  flipTransitionMode.value = mode
  flipOverlay.value = from
  setFlipStyle(from)
  isFlipAnimating.value = true

  flipAnimationFrame = requestAnimationFrame(() => {
    flipAnimationFrame = requestAnimationFrame(() => {
      flipOverlay.value = { ...to, src: from.src || to.src }
      flipOverlayStyle.value = {
        left: `${to.rect.left}px`,
        top: `${to.rect.top}px`,
        width: `${to.rect.width}px`,
        height: `${to.rect.height}px`,
        borderRadius: to.borderRadius,
      }
    })
  })

  flipCleanupTimer = window.setTimeout(() => {
    flipCleanupTimer = null
    resetFlipState()
  }, mode === 'opening' ? 420 : 520)
}

async function openNowPlaying() {
  resetFlipState()
  nowPlayingMotionState.value = null
  playerTransitionPulse.value = Date.now()
  isNowPlayingOpen.value = true
}

async function closeNowPlaying() {
  const from = nowPlayingRef.value?.getCoverSnapshot?.() as CoverSnapshot | null
  resetFlipState()
  nowPlayingMotionState.value = 'closing'
  scheduleNowPlayingMotionCleanup()
  playerTransitionPulse.value = Date.now()
  isNowPlayingOpen.value = false
  if (!from) return
  await nextTick()
  await new Promise(resolve => requestAnimationFrame(() => resolve(null)))
  const to = miniPlayerRef.value?.getCoverSnapshot?.() as CoverSnapshot | null
  if (!to) return
  runFlipTransition(from, to, 'closing')
}

// 背景图片
const bgImageStyle = computed(() => {
  const uri = settingsStore.backgroundImageUri
  if (!uri) return null
  const src = uri.startsWith('http') ? uri : convertFileSrc(uri)
  return {
    backgroundImage: `url("${src}")`,
    filter: `blur(${settingsStore.backgroundImageBlur}px)`,
    opacity: settingsStore.backgroundImageAlpha,
  }
})

// 防抖自动同步（歌单变更后 30s 触发，批量合并快速操作）
const DEBOUNCE_SYNC_MS = 30_000
let debounceSyncTimer: ReturnType<typeof setTimeout> | null = null
let unlistenPlaylistChanged: UnlistenFn | null = null

function scheduleDebouncedSync() {
  const syncStore = useSyncStore()
  // 同步进行中不调度
  if (syncStore.isSyncing) return

  if (debounceSyncTimer) clearTimeout(debounceSyncTimer)
  debounceSyncTimer = setTimeout(() => {
    debounceSyncTimer = null
    if (syncStore.github.configured && syncStore.github.autoSync) {
      syncStore.syncGitHub(true)
    } else if (syncStore.webdav.configured && syncStore.webdav.autoSync) {
      syncStore.syncWebDav(true)
    }
  }, DEBOUNCE_SYNC_MS)
}

// 启动时初始化：加载同步配置 + 检查登录状态 + 自动同步
onMounted(async () => {
  const syncStore = useSyncStore()
  const authStore = useAuthStore()

  // 并行加载
  await Promise.allSettled([
    syncStore.loadConfigs(),
    authStore.checkStatus(),
  ])

  // 自动同步（配置开启且已配置），静默模式
  if (syncStore.github.configured && syncStore.github.autoSync) {
    syncStore.syncGitHub(true)
  } else if (syncStore.webdav.configured && syncStore.webdav.autoSync) {
    syncStore.syncWebDav(true)
  }

  // 监听后端 playlists-changed 事件，防抖触发自动同步
  unlistenPlaylistChanged = await listen('playlists-changed', () => {
    scheduleDebouncedSync()
  })
})

onUnmounted(() => {
  resetFlipState()
  if (nowPlayingMotionTimer) clearTimeout(nowPlayingMotionTimer)
  if (debounceSyncTimer) clearTimeout(debounceSyncTimer)
  if (unlistenPlaylistChanged) unlistenPlaylistChanged()
})
</script>

<template>
  <div
    class="app-layout"
    :class="{
      'app-layout--np-active': isNowPlayingOpen || isNowPlayingMotionActive,
      'app-layout--np-opening': nowPlayingMotionState === 'opening',
      'app-layout--np-closing': nowPlayingMotionState === 'closing',
    }"
  >
    <!-- 自定义背景图 -->
    <div v-if="bgImageStyle" class="app-bg-image" :style="bgImageStyle"></div>
    <SideNav class="app-side-nav" />
    <main
      class="content"
      :class="{
        'has-mini-player': hasMiniPlayer,
        'content--np-dimmed': isNowPlayingOpen || isNowPlayingMotionActive,
      }"
    >
      <router-view v-slot="{ Component, route }">
        <transition name="fade" mode="out-in">
          <keep-alive :include="['HomeView', 'ExploreView', 'LibraryView']">
            <component :is="Component" :key="route.path" />
          </keep-alive>
        </transition>
      </router-view>
    </main>

    <!-- MiniPlayer 动画 -->
    <transition name="mini-enter">
      <MiniPlayer
        v-if="hasMiniPlayer && !isNowPlayingOpen"
        ref="miniPlayerRef"
        :cover-hidden-by-flip="hideMiniCoverForFlip"
        :key="`mini:${transitionTrackKey}:${playerTransitionPulse}`"
        @expand="openNowPlaying()"
      />
    </transition>

    <!-- NowPlaying 全屏覆盖 -->
    <transition name="slide-up">
      <NowPlaying
        v-if="isNowPlayingOpen"
        ref="nowPlayingRef"
        hide-header
        :transition-state="nowPlayingMotionState"
        @collapse="closeNowPlaying()"
      />
    </transition>

    <!-- 顶栏浮于所有内容之上，与播放器融合 -->
    <TitleBar
      :force-light="isNowPlayingOpen"
      :now-playing="isNowPlayingOpen"
      :track-name="player.currentTrack?.title"
      :album-name="player.currentTrack?.album ? displayAlbum(player.currentTrack.album) : ''"
      :transitioning="isNowPlayingOpen || isNowPlayingMotionActive"
      :transition-state="nowPlayingMotionState"
      @collapse="closeNowPlaying()"
      @toggle-more="nowPlayingRef?.toggleMore?.()"
    />

    <Transition name="flip-cover-fade">
      <div
        v-if="flipOverlay && flipOverlayStyle"
        class="flip-cover-overlay"
        :class="{
          'flip-cover-overlay--opening': flipTransitionMode === 'opening',
          'flip-cover-overlay--closing': flipTransitionMode === 'closing',
        }"
        :style="flipOverlayStyle"
      >
        <img :src="flipOverlay.src" class="flip-cover-img" referrerpolicy="no-referrer" />
      </div>
    </Transition>

    <AppToast />
  </div>
</template>

<style scoped lang="scss">
.app-layout {
  display: flex;
  width: 100%;
  height: 100%;
  overflow: hidden;
  position: relative;
  padding-top: 36px; /* 让出顶栏高度 */
  isolation: isolate;
}

.app-bg-image {
  position: fixed;
  inset: 0;
  z-index: 0;
  background-size: cover;
  background-position: center;
  background-repeat: no-repeat;
  pointer-events: none;
  /* scale slightly to hide blur edges */
  transform: scale(1.1);
}

.app-side-nav {
  position: relative;
  z-index: 2;
  transition:
    transform 420ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 300ms ease,
    filter 420ms cubic-bezier(0.22, 1, 0.36, 1);
}

.content {
  flex: 1;
  overflow-y: auto;
  overflow-x: hidden;
  position: relative;
  z-index: 2;
  transform-origin: center top;
  transition:
    padding-bottom 300ms var(--ease-standard),
    transform 460ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 320ms ease,
    filter 420ms cubic-bezier(0.22, 1, 0.36, 1);

  &.has-mini-player { padding-bottom: 76px; }
}

/* MiniPlayer 入场退场 */
.mini-enter-enter-active {
  transition: transform 350ms var(--ease-emphasized-decel),
              opacity 250ms var(--ease-decelerate),
              filter 350ms var(--ease-emphasized-decel);
}
.mini-enter-leave-active {
  transition: transform 200ms var(--ease-emphasized-accel),
              opacity 150ms var(--ease-accelerate),
              filter 200ms var(--ease-emphasized-accel);
}
.mini-enter-enter-from {
  transform: translateY(100%) scale(0.98);
  opacity: 0;
  filter: blur(12px);
}
.mini-enter-leave-to {
  transform: translateY(100%);
  opacity: 0;
  filter: blur(10px);
}

.flip-cover-overlay {
  position: fixed;
  z-index: 350;
  overflow: hidden;
  pointer-events: none;
  box-shadow: 0 20px 56px rgba(0, 0, 0, 0.36);
  transition:
    left 520ms cubic-bezier(0.22, 1, 0.36, 1),
    top 520ms cubic-bezier(0.22, 1, 0.36, 1),
    width 520ms cubic-bezier(0.22, 1, 0.36, 1),
    height 520ms cubic-bezier(0.22, 1, 0.36, 1),
    border-radius 520ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 180ms ease;
}

.flip-cover-overlay--opening {
  box-shadow: none;
  transition:
    left 360ms cubic-bezier(0.22, 1, 0.36, 1),
    top 360ms cubic-bezier(0.22, 1, 0.36, 1),
    width 360ms cubic-bezier(0.22, 1, 0.36, 1),
    height 360ms cubic-bezier(0.22, 1, 0.36, 1),
    border-radius 360ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 120ms ease;
}

.flip-cover-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.flip-cover-fade-enter-active,
.flip-cover-fade-leave-active {
  transition: opacity 160ms ease;
}

.flip-cover-fade-enter-from,
.flip-cover-fade-leave-to {
  opacity: 0;
}
</style>
