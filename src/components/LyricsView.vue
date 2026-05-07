<script setup lang="ts">
/**
 * LyricsView — Web Animation API 驱动的高性能歌词组件
 * 逐字动画由 KaraokeLine 类管理（移植自 AMLL），Vue 只做行级调度
 */
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from 'vue'
import type { LyricLine } from '@/stores/player'
import { useSettingsStore } from '@/stores/settings'
import { KaraokeLine } from '@/utils/karaokeLine'

const settings = useSettingsStore()

const props = withDefaults(defineProps<{
  lyrics: LyricLine[]
  currentTimeMs: number
  previewTimeMs?: number | null
  isPlaying: boolean
}>(), {
  currentTimeMs: 0,
  previewTimeMs: null,
  isPlaying: false,
})

const emit = defineEmits<{ seek: [timeMs: number] }>()
const containerRef = ref<HTMLDivElement>()

// --- KaraokeLine 实例管理 ---
const karaokeLines = ref<Map<number, KaraokeLine>>(new Map())
let lastActiveIndex = -1

function hasWordTiming(line: LyricLine): boolean {
  return line.words && line.words.length > 0 && line.words.some(w => w.durationMs > 0)
}

function buildKaraokeLines() {
  // 清理旧实例
  for (const kl of karaokeLines.value.values()) kl.dispose()
  karaokeLines.value.clear()

  if (!containerRef.value) return

  // 为有逐字数据的行创建 KaraokeLine
  const lineEls = containerRef.value.querySelectorAll('.lyric-line')
  props.lyrics.forEach((line, i) => {
    if (!hasWordTiming(line)) return
    const lineEl = lineEls[i] as HTMLElement
    if (!lineEl) return

    const wordContainer = lineEl.querySelector('.kw-container') as HTMLElement
    if (!wordContainer) return

    const kl = new KaraokeLine()
    const lineEnd = line.startMs + line.durationMs
    kl.build(wordContainer, line.words, line.startMs, lineEnd)
    karaokeLines.value.set(i, kl)
  })
}

// --- 手动滚动检测 ---
let isAutoScrolling = false
const isUserScrolling = ref(false)
const clearTextHoldIndex = ref<number | null>(null)
let scrollEndTimer: ReturnType<typeof setTimeout> | null = null

function onScroll() {
  if (isAutoScrolling) return
  isUserScrolling.value = true
  clearTextHoldIndex.value = activeIndex.value
  if (scrollEndTimer) clearTimeout(scrollEndTimer)
  scrollEndTimer = setTimeout(() => { isUserScrolling.value = false }, 150)
}

const isClearText = computed(() =>
  isUserScrolling.value || clearTextHoldIndex.value === activeIndex.value
)

// --- 时间 ---
const offsetMs = computed(() => settings.cloudMusicOffset || 0)
const effectiveTimeMs = computed(() =>
  props.previewTimeMs != null ? props.previewTimeMs : props.currentTimeMs
)
const adjustedTimeMs = computed(() => effectiveTimeMs.value + offsetMs.value)

const activeIndex = computed(() => {
  if (!props.lyrics.length) return -1
  const t = adjustedTimeMs.value
  for (let i = props.lyrics.length - 1; i >= 0; i--) {
    if (t >= props.lyrics[i].startMs) return i
  }
  return -1
})

// --- 滚动 ---
function scrollToActive(idx: number, behavior: ScrollBehavior = 'smooth') {
  if (idx < 0 || !containerRef.value) return
  isAutoScrolling = true
  nextTick(() => {
    const lineEls = containerRef.value!.querySelectorAll('.lyric-line')
    const el = lineEls[idx] as HTMLElement
    if (!el) { isAutoScrolling = false; return }
    const target = el.offsetTop - containerRef.value!.clientHeight * 0.40
    containerRef.value!.scrollTo({ top: target, behavior })
    setTimeout(() => { isAutoScrolling = false }, behavior === 'instant' ? 50 : 500)
  })
}

// --- 行级 enable/disable 调度 ---
watch(activeIndex, (idx) => {
  if (clearTextHoldIndex.value !== null && idx !== clearTextHoldIndex.value) {
    clearTextHoldIndex.value = null
  }
  if (!isUserScrolling.value) scrollToActive(idx)

  // 停用上一行
  if (lastActiveIndex >= 0 && lastActiveIndex !== idx) {
    karaokeLines.value.get(lastActiveIndex)?.disable()
  }
  // 激活新行
  if (idx >= 0) {
    karaokeLines.value.get(idx)?.enable(adjustedTimeMs.value, props.isPlaying)
  }
  lastActiveIndex = idx
})

// seek 时定位当前行
watch(adjustedTimeMs, (t) => {
  if (activeIndex.value >= 0) {
    karaokeLines.value.get(activeIndex.value)?.seek(t)
  }
})

// 播放/暂停时同步动画状态
watch(() => props.isPlaying, (playing) => {
  if (activeIndex.value >= 0) {
    const kl = karaokeLines.value.get(activeIndex.value)
    if (playing) kl?.resume()
    else kl?.pause()
  }
})

// 歌词数据变化时重建
watch(() => props.lyrics, () => {
  nextTick(() => buildKaraokeLines())
}, { deep: false })

onMounted(() => {
  containerRef.value?.addEventListener('scroll', onScroll, { passive: true })
  nextTick(() => {
    buildKaraokeLines()
    scrollToActive(activeIndex.value, 'instant')
    if (activeIndex.value >= 0) {
      karaokeLines.value.get(activeIndex.value)?.enable(adjustedTimeMs.value, props.isPlaying)
      lastActiveIndex = activeIndex.value
    }
  })
})

onUnmounted(() => {
  containerRef.value?.removeEventListener('scroll', onScroll)
  if (scrollEndTimer) clearTimeout(scrollEndTimer)
  for (const kl of karaokeLines.value.values()) kl.dispose()
})

function dist(index: number): number {
  if (activeIndex.value < 0) return 0
  return Math.abs(index - activeIndex.value)
}

// 对齐 ModernKaraokeLyricsView 预设参数
function blurForDist(d: number): number {
  if (!settings.lyricBlur || d === 0) return 0
  // 对齐 Android AdvancedLyricsView: blurDelta = lyricBlurAmount * 0.45, clamped [0, 4]
  // CSS blur 视觉比 Compose 更重，再折半
  const blurDelta = Math.min(4, settings.lyricBlurAmount * 0.3)
  return blurDelta * d
}

function scaleForDist(d: number): number {
  // focusedScale = 1.015, unfocusedScale = 0.965
  if (d === 0) return 1.015
  return 0.965
}

function alphaForDist(d: number): number {
  // activeAlpha = 1.0, inactiveAlpha = 0.28
  if (d === 0) return 1.0
  return 0.28
}

function seekToLine(line: LyricLine) {
  clearTextHoldIndex.value = null
  emit('seek', line.startMs)
}
</script>

<template>
  <div class="lyrics-scroll" ref="containerRef" :style="{ '--lyric-font-scale': settings.lyricFontScale }">
    <div class="lyrics-pad-top" />

    <div
      v-for="(line, i) in lyrics"
      :key="i"
      class="lyric-line"
      :class="{
        active: i === activeIndex,
        past: activeIndex >= 0 && i < activeIndex,
        'clear-text': isClearText,
      }"
      :style="isClearText ? {} : {
        '--blur': `${blurForDist(dist(i))}px`,
        '--scale': String(scaleForDist(dist(i))),
        '--alpha': String(alphaForDist(dist(i))),
      }"
      @click="seekToLine(line)"
    >
      <!-- 逐字模式：KaraokeLine 直接操作这个容器的 DOM -->
      <span v-if="hasWordTiming(line)" class="line-text kw-container" />

      <!-- 整行模式 -->
      <span v-else class="line-text">{{ line.text }}</span>

      <!-- 翻译 -->
      <span
        v-if="line.translation && settings.showTranslation"
        class="line-tl"
      >{{ line.translation }}</span>
    </div>

    <div class="lyrics-pad-bottom" />
  </div>
</template>

<style scoped lang="scss">
// 对齐 accompanist-lyrics-ui ModernKaraokeLyricsView 预设

.lyrics-scroll {
  width: 100%;
  height: 100%;
  overflow-y: auto;
  overflow-x: hidden;
  padding: 0 16px;
  // 上下渐隐：top 20px, bottom 100px
  mask-image: linear-gradient(to bottom, transparent 0%, black 20px, black calc(100% - 100px), transparent 100%);
  -webkit-mask-image: linear-gradient(to bottom, transparent 0%, black 20px, black calc(100% - 100px), transparent 100%);
  text-align: left;
  &::-webkit-scrollbar { display: none; }
  scrollbar-width: none;
}

.lyrics-pad-top { height: 42%; }
.lyrics-pad-bottom { height: 50%; }

.lyric-line {
  position: relative;
  padding: 8px 16px;
  // transform-origin 左下角（对齐参考项目 LTR 模式）
  transform-origin: left bottom;
  transform: scale(var(--scale, 1));
  opacity: var(--alpha, 1);
  filter: blur(var(--blur, 0px));
  // 对齐参考项目：scale 600ms, opacity 400ms, blur 300ms
  transition:
    transform 600ms cubic-bezier(0.2, 0, 0, 1),
    opacity 400ms cubic-bezier(0.4, 0, 0.2, 1),
    filter 300ms ease-out;
  cursor: pointer;
  will-change: transform, opacity, filter;
  // additive blend — 对齐参考项目 BlendMode.Plus
  mix-blend-mode: plus-lighter;

  &:hover {
    opacity: 0.6 !important;
    filter: blur(0px) !important;
  }
  &.active {
    filter: none;
    // 活跃行也保持左下角 origin
    transform-origin: left bottom;
  }
  &.clear-text {
    transform: none;
    opacity: 0.5;
    filter: none;
    mix-blend-mode: normal;
    transition: opacity 0.15s;
    &.active { opacity: 1; }
  }
}

.line-text {
  display: block;
  // 对齐参考项目 32sp Bold, lineHeight 38sp
  font-size: calc(32px * var(--lyric-font-scale, 1));
  font-weight: 700;
  line-height: calc(38px * var(--lyric-font-scale, 1));
  letter-spacing: -0.2px;
  color: rgba(255, 255, 255, 0.2);
  white-space: pre-wrap;
  transition: color 0.4s;
  position: relative;
  z-index: 1;
  .active & { color: white; }
  .clear-text & { color: rgba(255, 255, 255, 0.35); }
  .clear-text.active & { color: white; }
}

// KaraokeLine 创建的逐字 <span> 样式
:deep(.kw) {
  display: inline;
  color: inherit;
}

.line-tl {
  display: block;
  // 对齐参考项目伴唱行 19sp SemiBold, lineHeight 24sp
  font-size: calc(19px * var(--lyric-font-scale, 1));
  font-weight: 600;
  color: rgba(255, 255, 255, 0.24);
  margin-top: 4px;
  line-height: calc(24px * var(--lyric-font-scale, 1));
  position: relative;
  z-index: 1;

  // 翻译行 alpha = 0.4 * activeColor (对齐参考项目)
  .active & { color: rgba(255, 255, 255, 0.70); }
  .past & { color: rgba(255, 255, 255, 0.18); }
}
</style>
