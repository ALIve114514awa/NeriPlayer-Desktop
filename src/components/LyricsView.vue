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
    // L9: 30% 顶部占比 + lineHeight*0.42 视觉补偿（对齐 AdvancedLyricsView focusedLineVisualCompensation）
    const lineHeight = el.offsetHeight
    const target = Math.max(
      0,
      el.offsetTop - containerRef.value!.clientHeight * 0.30 + lineHeight * 0.42,
    )
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

// 对齐 Android AdvancedLyricsView / ModernKaraokeLyricsView 预设参数
// L2: blurRadius = distanceWeight * blurDelta（线性），blurDelta = lyricBlurAmount * 0.45，clamp 0..4
function blurForDist(d: number): number {
  if (!settings.lyricBlur || d === 0) return 0
  const blurDelta = settings.lyricBlurAmount * 0.45
  return Math.min(4, d * blurDelta)
}

// L1: Android focusedLineScale = 1.015 / unfocusedLineScale = 0.965，远距再缓降
function scaleForDist(d: number): number {
  if (d <= 0) return 1.015
  if (d === 1) return 0.965
  return Math.max(0.92, 0.955 - (d - 1) * 0.01)
}

// Android inactive alpha ≈ 0.28（focused 1.0）
function alphaForDist(d: number): number {
  if (d === 0) return 1
  if (d === 1) return 0.28
  return Math.max(0.16, 0.28 - 0.04 * (d - 1))
}

// L4: 近似 Android 逐行弹簧 spring(damping 0.95, stiffness 120-20*distance, floor 20)
// 用距离驱动的 transition 时长模拟「越近越跟手、远行软着陆」的级联手感
function settleDurForDist(d: number): number {
  const stiffness = Math.max(20, 120 - 20 * d)
  // 刚度越高 settle 越快：以 120 对应 ~360ms 为基准反比缩放
  return Math.round(360 * (120 / stiffness))
}

function seekToLine(line: LyricLine) {
  clearTextHoldIndex.value = null
  emit('seek', line.startMs)
}

// L7: 间奏/前奏「呼吸点」——对齐 Android KaraokeBreathingDots（gap > 5000ms）
const INTERLUDE_GAP_MS = 5000

// 返回第 i 行之前的间奏区间 [start, end)，无则 null
function interludeBefore(i: number): { start: number; end: number } | null {
  const line = props.lyrics[i]
  if (!line) return null
  if (i === 0) {
    return line.startMs > INTERLUDE_GAP_MS ? { start: 0, end: line.startMs } : null
  }
  const prev = props.lyrics[i - 1]
  const prevEnd = prev.startMs + prev.durationMs
  return line.startMs - prevEnd > INTERLUDE_GAP_MS
    ? { start: prevEnd, end: line.startMs }
    : null
}

// 当前播放时间是否落在该间奏内（决定呼吸点是否高亮）
function isInterludeActive(gap: { start: number; end: number } | null): boolean {
  if (!gap) return false
  const t = adjustedTimeMs.value
  return t >= gap.start && t < gap.end
}

// 呼吸点整体进度 0..1，用于收尾时的缩放/淡出
function interludeProgress(gap: { start: number; end: number } | null): number {
  if (!gap) return 0
  const t = adjustedTimeMs.value
  return Math.max(0, Math.min(1, (t - gap.start) / Math.max(1, gap.end - gap.start)))
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

    <template v-for="(line, i) in lyrics" :key="`${i}:${line.startMs}`">
      <!-- L7: 间奏呼吸点 -->
      <div
        v-if="interludeBefore(i)"
        class="interlude-dots"
        :class="{ 'interlude-dots--active': isInterludeActive(interludeBefore(i)) }"
        :style="{ '--interlude-progress': String(interludeProgress(interludeBefore(i))) }"
        @click="emit('seek', interludeBefore(i)!.start)"
      >
        <span class="interlude-dot" />
        <span class="interlude-dot" />
        <span class="interlude-dot" />
      </div>

      <div
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
          '--settle': `${settleDurForDist(dist(i))}ms`,
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
    </template>

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

// L7: 间奏呼吸点
.interlude-dots {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 14px 8px 14px 0;
  max-width: 720px;
  cursor: pointer;
  opacity: 0.32;
  transform: scale(0.9);
  transform-origin: left center;
  transition: opacity 320ms ease, transform 320ms cubic-bezier(0.22, 1, 0.36, 1);

  .interlude-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--lyric-accent, rgba(255, 255, 255, 0.92));
  }

  &.interlude-dots--active {
    opacity: calc(0.85 - var(--interlude-progress, 0) * 0.35);
    transform: scale(1);

    .interlude-dot {
      animation: interlude-breathe 1400ms ease-in-out infinite;
      box-shadow: 0 0 10px color-mix(in srgb, var(--lyric-accent) 45%, transparent);
    }
    .interlude-dot:nth-child(2) { animation-delay: 200ms; }
    .interlude-dot:nth-child(3) { animation-delay: 400ms; }
  }
}

@keyframes interlude-breathe {
  0%, 100% { transform: scale(0.7); opacity: 0.5; }
  50% { transform: scale(1.15); opacity: 1; }
}

.lyric-line {
  position: relative;
  width: 100%;
  max-width: 720px;
  margin: 0;
  padding: 10px 8px 10px 0;
  text-align: left;
  // L3: Android TransformOrigin(0f, 1f) 左下角锚定，缩放时基线稳定
  transform-origin: left bottom;
  transform: scale(var(--scale, 1));
  opacity: var(--alpha, 1);
  filter: blur(var(--blur, 0px));
  // L4: 弹簧近似——位移/缩放用带回弹的 spring easing，时长随距离（--settle）级联
  transition:
    transform var(--settle, 380ms) cubic-bezier(0.34, 1.3, 0.5, 1),
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
    transform-origin: left bottom;
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
    // L5: 加色辉光——对齐 Android BlendMode.Plus + 10px shadow
    text-shadow:
      0 0 10px color-mix(in srgb, var(--lyric-accent) 55%, transparent),
      0 0 22px color-mix(in srgb, var(--lyric-accent-soft) 40%, transparent);
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
  // L5: 加色混合，让高亮字在底层文本上叠加发光，近似 BlendMode.Plus
  mix-blend-mode: plus-lighter;
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
  color: rgba(255, 255, 255, 0.4);
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
