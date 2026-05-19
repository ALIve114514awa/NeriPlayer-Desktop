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
  lyricOffsetMs?: number
  accentColor?: string
  accentContainerColor?: string
}>(), {
  currentTimeMs: 0,
  previewTimeMs: null,
  isPlaying: false,
  lyricOffsetMs: undefined,
  accentColor: '',
  accentContainerColor: '',
})

const emit = defineEmits<{ seek: [timeMs: number] }>()
const containerRef = ref<HTMLDivElement>()
const isLyricsSwitching = ref(false)
let lyricsSwitchTimer: ReturnType<typeof setTimeout> | null = null

// --- KaraokeLine 实例管理 ---
const karaokeLines = ref<Map<number, KaraokeLine>>(new Map())
let lastActiveIndex = -1

function hasWordTiming(line: LyricLine): boolean {
  return line.words && line.words.length > 0 && line.words.some(w => w.durationMs > 0)
}

function lyricTextFromWords(line: LyricLine): string {
  if (line.text && line.text.trim()) return line.text
  return (line.words || []).map(w => w.text).join('')
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
const offsetMs = computed(() => {
  if (typeof props.lyricOffsetMs === 'number') return props.lyricOffsetMs
  return settings.cloudMusicOffset || 0
})
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
    const target = Math.max(0, el.offsetTop - containerRef.value!.clientHeight * 0.30)
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
  isLyricsSwitching.value = true
  if (lyricsSwitchTimer) clearTimeout(lyricsSwitchTimer)
  nextTick(() => {
    buildKaraokeLines()
    lastActiveIndex = -1
    if (activeIndex.value >= 0) {
      karaokeLines.value.get(activeIndex.value)?.enable(adjustedTimeMs.value, props.isPlaying)
      lastActiveIndex = activeIndex.value
      scrollToActive(activeIndex.value, 'instant')
    }
  })
  lyricsSwitchTimer = setTimeout(() => {
    isLyricsSwitching.value = false
  }, 420)
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
  if (lyricsSwitchTimer) clearTimeout(lyricsSwitchTimer)
  for (const kl of karaokeLines.value.values()) kl.dispose()
})

function dist(index: number): number {
  if (activeIndex.value < 0) return 0
  return Math.abs(index - activeIndex.value)
}

// 对齐 ModernKaraokeLyricsView 预设参数
function blurForDist(d: number): number {
  if (!settings.lyricBlur || d === 0) return 0
  const blurUnit = Math.min(4, settings.lyricBlurAmount * 0.45)
  if (d === 1) return blurUnit * 1.0
  if (d === 2) return blurUnit * 1.5
  if (d === 3) return blurUnit * 2.0
  if (d === 4) return blurUnit * 2.5
  return blurUnit * 4.0
}

function scaleForDist(d: number): number {
  if (d <= 0) return 1.1
  if (d === 1) return 0.9
  return Math.max(0.8, 0.88 - (d - 2) * 0.02)
}

function alphaForDist(d: number): number {
  if (d === 0) return 1
  if (settings.lyricBlur) {
    if (d === 1) return 0.72
    if (d === 2) return 0.4
    return Math.max(0.16, 0.4 - 0.08 * (d - 2))
  }
  if (d === 1) return 0.4
  if (d === 2) return 0.35
  return Math.max(0.16, 0.35 - 0.08 * (d - 2))
}

function seekToLine(line: LyricLine) {
  clearTextHoldIndex.value = null
  emit('seek', line.startMs)
}

</script>

<template>
  <div
    class="lyrics-scroll"
    ref="containerRef"
    :class="{
      'lyrics-scroll-switching': isLyricsSwitching,
    }"
    :style="{
      '--lyric-font-scale': settings.lyricFontScale,
      '--lyric-accent': props.accentColor || 'rgba(255,255,255,0.92)',
      '--lyric-accent-soft': props.accentContainerColor || 'rgba(255,255,255,0.22)',
    }"
  >
    <div class="lyrics-pad-top" />

    <div
      v-for="(line, i) in lyrics"
      :key="`${line.startMs}:${line.text}:${i}`"
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
      <!-- 逐字歌词：底层完整文本常驻，上层 karaoke mask 只负责当前高亮，避免未唱部分被 mask 透明掉 -->
      <span v-if="hasWordTiming(line)" class="line-text line-text--karaoke">
        <span class="line-text-base">{{ lyricTextFromWords(line) }}</span>
        <span class="line-text-highlight kw-container" aria-hidden="true" />
      </span>
      <span v-else class="line-text">{{ lyricTextFromWords(line) }}</span>

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
  padding: 0 28px 0 40px;
  mask-image: linear-gradient(to bottom, transparent 0%, black 112px, black calc(100% - 196px), transparent 100%);
  -webkit-mask-image: linear-gradient(to bottom, transparent 0%, black 112px, black calc(100% - 196px), transparent 100%);
  text-align: left;
  transition: opacity 260ms ease, transform 420ms cubic-bezier(0.22, 1, 0.36, 1);
  &::-webkit-scrollbar { display: none; }
  scrollbar-width: none;
}

.lyrics-scroll-switching {
  opacity: 0.78;
  transform: translateY(8px);
}

.lyrics-pad-top { height: 34%; }
.lyrics-pad-bottom { height: 44%; }

.lyric-line {
  position: relative;
  width: 100%;
  max-width: 720px;
  margin: 0;
  padding: 10px 8px 10px 0;
  text-align: left;
  transform-origin: left center;
  transform: scale(var(--scale, 1));
  opacity: var(--alpha, 1);
  filter: blur(var(--blur, 0px));
  transition:
    transform 380ms cubic-bezier(0.22, 1, 0.36, 1),
    opacity 260ms ease,
    filter 260ms ease,
    letter-spacing 260ms ease;
  cursor: pointer;
  will-change: transform, opacity, filter;

  &:hover {
    opacity: min(1, calc(var(--alpha, 1) + 0.06)) !important;
    filter: blur(0px) !important;
  }
  &.active {
    filter: none;
    transform-origin: left center;
  }
  &.clear-text {
    transform: none;
    opacity: 0.56;
    filter: none;
    transition: opacity 0.15s;
    &.active { opacity: 1; }
  }
}

.line-text {
  display: block;
  font-size: calc(18px * var(--lyric-font-scale, 1));
  font-weight: 700;
  line-height: calc(21.24px * var(--lyric-font-scale, 1));
  letter-spacing: 0;
  color: rgba(255, 255, 255, 0.22);
  white-space: pre-wrap;
  text-wrap: pretty;
  transition: color 0.22s ease, text-shadow 0.22s ease, transform 0.22s ease, opacity 0.22s ease;
  position: relative;
  z-index: 1;
  .active & {
    color: white;
    text-shadow: 0 0 12px color-mix(in srgb, var(--lyric-accent-soft) 10%, transparent);
  }
  .past & { color: rgba(255, 255, 255, 0.28); }
  .clear-text & { color: rgba(255, 255, 255, 0.42); }
  .clear-text.active & { color: white; }
}

.line-text--karaoke {
  display: grid;
  grid-template-areas: 'text';
}

.line-text-base,
.line-text-highlight {
  grid-area: text;
  min-width: 0;
  white-space: pre-wrap;
}

.line-text-base {
  color: inherit;
}

.line-text-highlight {
  color: white;
  opacity: 0;
  pointer-events: none;
}

.lyric-line.active .line-text-highlight {
  opacity: 1;
}

.lyric-line:not(.active) .line-text-highlight {
  display: none;
}

// KaraokeLine 创建的逐字 <span> 样式
:deep(.kw) {
  display: inline;
  color: inherit;
  text-shadow: inherit;
  font-weight: inherit;
}

:deep(.line-text--active.kw-container .kw),
:deep(.lyric-line.active .kw-container .kw) {
  color: white;
}

.line-tl {
  display: block;
  font-size: calc(11.16px * var(--lyric-font-scale, 1));
  font-weight: 600;
  color: rgba(255, 255, 255, 0.24);
  margin-top: 4px;
  line-height: calc(12.5px * var(--lyric-font-scale, 1));
  position: relative;
  z-index: 1;

  .active & {
    color: rgba(255, 255, 255, 0.72);
    text-shadow: none;
  }
  .past & { color: rgba(255, 255, 255, 0.18); }
}
</style>
