<script setup lang="ts">
/**
 * LyricsView — Web Animation API 驱动的高性能歌词组件
 * 逐字动画由 KaraokeLine 类管理（移植自 AMLL），Vue 只做行级调度
 */
import { ref, computed, watch, nextTick, onMounted, onUnmounted } from 'vue'
import type { LyricLine } from '@/stores/player'
import { useSettingsStore } from '@/stores/settings'
import { KaraokeLine } from '@/utils/karaokeLine'
import { Spring } from '@/utils/spring'

const settings = useSettingsStore()

const props = withDefaults(defineProps<{
  lyrics: LyricLine[]
  currentTimeMs: number
  previewTimeMs?: number | null
  isPlaying: boolean
  lyricOffsetMs?: number
}>(), {
  currentTimeMs: 0,
  previewTimeMs: null,
  isPlaying: false,
  lyricOffsetMs: undefined,
})

const emit = defineEmits<{ seek: [timeMs: number] }>()
const containerRef = ref<HTMLDivElement>()
const isLyricsSwitching = ref(false)
let lyricsSwitchTimer: ReturnType<typeof setTimeout> | null = null

// --- KaraokeLine 实例管理 ---
const karaokeLines = ref<Map<number, KaraokeLine>>(new Map())
let lastActiveIndex = -1
const KARAOKE_PREFETCH_RANGE = 2
const KARAOKE_KEEP_RANGE = 6

function hasWordTiming(line: LyricLine): boolean {
  return line.words && line.words.length > 0 && line.words.some(w => w.durationMs > 0)
}

function lyricTextFromWords(line: LyricLine): string {
  if (line.text && line.text.trim()) return line.text
  return (line.words || []).map(w => w.text).join('')
}

function lineElAt(index: number): HTMLElement | null {
  if (!containerRef.value) return null
  const cached = lineAnims[index]?.el
  if (cached?.isConnected) return cached
  return containerRef.value.querySelectorAll('.lyric-line')[index] as HTMLElement | null
}

function buildKaraokeLine(index: number): void {
  const line = props.lyrics[index]
  if (!line || !hasWordTiming(line) || karaokeLines.value.has(index)) return

  const lineEl = lineElAt(index)
  if (!lineEl) return

  const wordContainer = lineEl.querySelector('.kw-container') as HTMLElement
  if (!wordContainer) return

  const kl = new KaraokeLine()
  const lineEnd = line.startMs + line.durationMs
  kl.build(wordContainer, line.words, line.startMs, lineEnd, lyricTextFromWords(line))
  karaokeLines.value.set(index, kl)
}

function syncKaraokeWindow(centerIndex: number): void {
  for (const [index, kl] of karaokeLines.value) {
    if (centerIndex < 0 || Math.abs(index - centerIndex) > KARAOKE_KEEP_RANGE) {
      kl.dispose()
      karaokeLines.value.delete(index)
    }
  }

  if (centerIndex < 0) return
  const start = Math.max(0, centerIndex - KARAOKE_PREFETCH_RANGE)
  const end = Math.min(props.lyrics.length - 1, centerIndex + KARAOKE_PREFETCH_RANGE)
  for (let i = start; i <= end; i++) buildKaraokeLine(i)
}

function resetKaraokeLines(): void {
  for (const kl of karaokeLines.value.values()) kl.dispose()
  karaokeLines.value.clear()
}

// --- 手动滚动检测 ---
const isUserScrolling = ref(false)
const clearTextHoldIndex = ref<number | null>(null)
let scrollEndTimer: ReturnType<typeof setTimeout> | null = null

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

function findActiveLyricIndex(lines: LyricLine[], timeMs: number): number {
  let lo = 0
  let hi = lines.length - 1
  let ans = -1
  while (lo <= hi) {
    const mid = (lo + hi) >> 1
    if (timeMs >= lines[mid].startMs) {
      ans = mid
      lo = mid + 1
    } else {
      hi = mid - 1
    }
  }
  return ans
}

const activeIndex = computed(() => {
  if (!props.lyrics.length) return -1
  return findActiveLyricIndex(props.lyrics, adjustedTimeMs.value)
})

// === 弹簧驱动的歌词布局引擎（移植 AMLL DOM 渲染器思路）===
// 单个 RAF 循环统一 tick：滚动位置用一个弹簧，行内视觉属性（缩放/透明度/模糊）各用一组弹簧，
// 直接写 DOM 样式绕过 Vue 响应式；全部弹簧静止时暂停循环省电
const trackRef = ref<HTMLDivElement>()

interface LineAnim {
  el: HTMLElement
  scale: Spring
  opacity: Spring
  blur: Spring
  naturalTop: number
  height: number
  lastDist: number
}

let lineAnims: LineAnim[] = []
// 滚动位置弹簧：沿用 AMLL 的自然欠阻尼手感，行切换会有轻微惯性
const scrollSpring = new Spring(0)
scrollSpring.updateParams({ mass: 1, damping: 18, stiffness: 140 })

let rafId = 0
let lastTs = 0
// 手动滚动的临时偏移目标（null 表示自动跟随 active 行）
let manualScrollTarget: number | null = null

// 只让屏幕附近的歌词参与逐帧动画，远处歌词直接落位
const VISIBLE_RANGE = 8

function collectLineEls(): HTMLElement[] {
  if (!containerRef.value) return []
  return Array.from(containerRef.value.querySelectorAll('.lyric-line')) as HTMLElement[]
}

// 构建每行的弹簧集合与自然布局位置
function buildLayout(instant = false): void {
  const els = collectLineEls()
  lineAnims = els.map((el, i) => {
    const prev = lineAnims[i]
    return {
      el,
      scale: prev?.scale ?? new Spring(1),
      opacity: prev?.opacity ?? new Spring(1),
      blur: prev?.blur ?? new Spring(0),
      naturalTop: el.offsetTop,
      height: el.offsetHeight,
      lastDist: prev?.lastDist ?? 999,
    }
  })
  // 缩放跟随滚动做真实弹簧，透明度和模糊保持柔和避免过冲发灰
  for (const la of lineAnims) {
    la.scale.updateParams({ mass: 1, damping: 18, stiffness: 150 })
    la.opacity.updateParams({ mass: 1, damping: 26, stiffness: 180, soft: true })
    la.blur.updateParams({ mass: 1, damping: 30, stiffness: 200, soft: true })
  }
  layoutTargets(instant)
}

// 计算滚动目标位置（把 active 行对齐到容器 30% + 视觉补偿，沿用原有公式）
function focusAnchorRatio(): number {
  const el = containerRef.value
  if (!el) return 0.30
  const aspect = el.clientHeight / Math.max(1, el.clientWidth)
  if (aspect > 1.38) return 0.23
  if (aspect > 1.05) return 0.27
  return 0.31
}

function scrollTargetFor(idx: number): number {
  if (idx < 0 || !containerRef.value || !lineAnims[idx]) return 0
  const la = lineAnims[idx]
  return Math.max(0, la.naturalTop - containerRef.value.clientHeight * focusAnchorRatio() + la.height * 0.42)
}

// 根据当前 active 行设置所有行的弹簧目标（含逐行错峰级联）
function layoutTargets(instant = false): void {
  const active = activeIndex.value
  const clear = isClearText.value

  // 滚动目标：手动滚动时用手动值，否则跟随 active
  const scrollTarget = manualScrollTarget != null ? manualScrollTarget : scrollTargetFor(active)
  if (instant) scrollSpring.setPosition(scrollTarget)
  else scrollSpring.setTargetPosition(scrollTarget)

  for (let i = 0; i < lineAnims.length; i++) {
    const la = lineAnims[i]
    const d = active < 0 ? 0 : Math.abs(i - active)
    const targetScale = clear ? 1 : scaleForDist(d)
    const targetOpacity = clear
      ? (i === active ? 1 : 0.72)
      : alphaForDist(d)
    const targetBlur = clear ? 0 : blurForDist(d)
    // 逐行错峰：距 active 越远，弹簧启动越晚，形成级联
    const delaySec = instant ? 0 : staggerForDist(d) / 1000

    if (instant || d > VISIBLE_RANGE) {
      la.scale.setPosition(targetScale)
      la.opacity.setPosition(targetOpacity)
      la.blur.setPosition(targetBlur)
      writeLineStyle(i, la, 0.016, true)
    } else {
      la.scale.setTargetPosition(targetScale, delaySec)
      la.opacity.setTargetPosition(targetOpacity, delaySec)
      la.blur.setTargetPosition(targetBlur, delaySec)
    }
    la.lastDist = d
  }
  ensureRaf()
}

// 把某行弹簧的当前值写入 DOM（不经 Vue 响应式）
function writeLineStyle(i: number, la: LineAnim, delta = 0.016, forceMask = false): void {
  const s = la.scale.getCurrentPosition()
  la.el.style.transform = `scale(${s.toFixed(4)})`
  la.el.style.opacity = la.opacity.getCurrentPosition().toFixed(3)
  const b = la.blur.getCurrentPosition()
  la.el.style.filter = b > 0.02 ? `blur(${b.toFixed(2)}px)` : 'none'
  karaokeLines.value.get(i)?.updateMaskAlpha(s, delta, forceMask)
}

function ensureRaf(): void {
  if (rafId) return
  lastTs = performance.now()
  rafId = requestAnimationFrame(frame)
}

function frame(now: number): void {
  const dt = Math.min(0.05, (now - lastTs) / 1000)
  lastTs = now

  scrollSpring.update(dt)
  if (trackRef.value) {
    trackRef.value.style.transform = `translate3d(0, ${(-scrollSpring.getCurrentPosition()).toFixed(2)}px, 0)`
  }
  let settled = scrollSpring.arrived()

  const active = activeIndex.value
  for (let i = 0; i < lineAnims.length; i++) {
    const la = lineAnims[i]
    const d = active < 0 ? 0 : Math.abs(i - active)
    if (d > VISIBLE_RANGE) continue
    la.scale.update(dt)
    la.opacity.update(dt)
    la.blur.update(dt)
    writeLineStyle(i, la, dt)
    if (!(la.scale.arrived() && la.opacity.arrived() && la.blur.arrived())) settled = false
  }

  if (settled) {
    rafId = 0
  } else {
    rafId = requestAnimationFrame(frame)
  }
}

// --- 手动滚动（滚轮）---
function onWheel(e: WheelEvent): void {
  if (!containerRef.value) return
  e.preventDefault()
  isUserScrolling.value = true
  clearTextHoldIndex.value = activeIndex.value
  const base = manualScrollTarget != null ? manualScrollTarget : scrollSpring.getCurrentPosition()
  const maxScroll = trackRef.value
    ? Math.max(0, trackRef.value.scrollHeight - containerRef.value.clientHeight)
    : base + e.deltaY
  manualScrollTarget = Math.max(0, Math.min(maxScroll, base + e.deltaY))
  layoutTargets()
  if (scrollEndTimer) clearTimeout(scrollEndTimer)
  scrollEndTimer = setTimeout(() => {
    isUserScrolling.value = false
    manualScrollTarget = null
    layoutTargets()
  }, 900)
}

// --- 行级 enable/disable 调度 ---
watch(activeIndex, (idx) => {
  if (clearTextHoldIndex.value !== null && idx !== clearTextHoldIndex.value) {
    clearTextHoldIndex.value = null
  }

  if (lastActiveIndex >= 0 && lastActiveIndex !== idx) {
    karaokeLines.value.get(lastActiveIndex)?.disable()
  }
  syncKaraokeWindow(idx)
  if (!isUserScrolling.value) layoutTargets()

  if (idx >= 0) {
    const line = karaokeLines.value.get(idx)
    line?.enable(adjustedTimeMs.value, props.isPlaying)
    line?.updateMaskAlpha(scaleForDist(0), 0.016, true)
  }
  lastActiveIndex = idx
})

// seek 时定位当前行
watch(adjustedTimeMs, (t) => {
  if (activeIndex.value >= 0) {
    buildKaraokeLine(activeIndex.value)
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

// clear-text 状态变化时刷新目标（手动滚动进入/退出）
watch(isClearText, () => layoutTargets())

// 歌词数据变化时重建
watch(() => props.lyrics, () => {
  isLyricsSwitching.value = true
  manualScrollTarget = null
  isUserScrolling.value = false
  if (lyricsSwitchTimer) clearTimeout(lyricsSwitchTimer)
  nextTick(() => {
    resetKaraokeLines()
    lineAnims = []
    syncKaraokeWindow(activeIndex.value)
    buildLayout(true)
    lastActiveIndex = -1
    if (activeIndex.value >= 0) {
      const line = karaokeLines.value.get(activeIndex.value)
      line?.enable(adjustedTimeMs.value, props.isPlaying)
      line?.updateMaskAlpha(scaleForDist(0), 0.016, true)
      lastActiveIndex = activeIndex.value
    }
  })
  lyricsSwitchTimer = setTimeout(() => {
    isLyricsSwitching.value = false
  }, 420)
}, { deep: false })

let resizeObserver: ResizeObserver | null = null

function refreshLayout(instant = true): void {
  for (const la of lineAnims) {
    la.naturalTop = la.el.offsetTop
    la.height = la.el.offsetHeight
  }

  const target = manualScrollTarget != null
    ? manualScrollTarget
    : scrollTargetFor(activeIndex.value)

  if (instant) scrollSpring.setPosition(target)
  layoutTargets(instant)
}

watch(
  () => [settings.lyricFontScale, settings.showTranslation] as const,
  () => nextTick(() => refreshLayout(true)),
)

onMounted(() => {
  containerRef.value?.addEventListener('wheel', onWheel, { passive: false })
  nextTick(() => {
    syncKaraokeWindow(activeIndex.value)
    buildLayout(true)
    if (activeIndex.value >= 0) {
      const line = karaokeLines.value.get(activeIndex.value)
      line?.enable(adjustedTimeMs.value, props.isPlaying)
      line?.updateMaskAlpha(scaleForDist(0), 0.016, true)
      lastActiveIndex = activeIndex.value
    }
  })
  // 容器或内容尺寸变化时重新测量布局
  if (containerRef.value && 'ResizeObserver' in window) {
    resizeObserver = new ResizeObserver(() => refreshLayout(true))
    resizeObserver.observe(containerRef.value)
    nextTick(() => {
      if (trackRef.value) resizeObserver?.observe(trackRef.value)
    })
  }
})

onUnmounted(() => {
  containerRef.value?.removeEventListener('wheel', onWheel)
  if (scrollEndTimer) clearTimeout(scrollEndTimer)
  if (lyricsSwitchTimer) clearTimeout(lyricsSwitchTimer)
  if (rafId) cancelAnimationFrame(rafId)
  resizeObserver?.disconnect()
  resetKaraokeLines()
})

// 对齐手机端观感：保留轻微景深，但别让非活跃行糊成一团
function blurForDist(d: number): number {
  if (!settings.lyricBlur || d === 0 || d > 2) return 0
  const blurDelta = Math.min(0.42, settings.lyricBlurAmount * 0.14)
  return Math.min(0.9, d * blurDelta)
}

// 手机端 active 行更有重量，非 active 行轻微后退
function scaleForDist(d: number): number {
  if (d <= 0) return 1.04
  if (d === 1) return 0.955
  if (d === 2) return 0.925
  return 0.9
}

// 非活跃行像手机端一样退到背景里，但仍保留上下文可读性
function alphaForDist(d: number): number {
  if (d === 0) return 1
  if (d === 1) return 0.42
  if (d === 2) return 0.28
  return Math.max(0.14, 0.24 - 0.03 * (d - 2))
}

// 行级错开延迟，保留一点级联感但不要拖泥带水
function staggerForDist(d: number): number {
  return d * 14
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
    }"
  >
    <!-- 由 scrollSpring 驱动 translate3d 的滚动轨道；行内视觉属性由 RAF 直接写 DOM -->
    <div class="lyrics-track" ref="trackRef">
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
          @click="seekToLine(line)"
        >
          <!-- 逐字歌词：同一组 word 同时承载已唱白和未唱灰，避免双层文本错位 -->
          <span v-if="hasWordTiming(line)" class="line-text line-text--karaoke kw-container">
            {{ lyricTextFromWords(line) }}
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
  </div>
</template>

<style scoped lang="scss">
// 对齐 accompanist-lyrics-ui ModernKaraokeLyricsView 预设

.lyrics-scroll {
  width: 100%;
  height: 100%;
  overflow: hidden;
  position: relative;
  box-sizing: border-box;
  padding: 0 clamp(18px, 3vw, 48px) 0 clamp(28px, 4vw, 72px);
  mask-image: linear-gradient(to bottom, transparent 0%, black 48px, black calc(100% - 80px), transparent 100%);
  -webkit-mask-image: linear-gradient(to bottom, transparent 0%, black 48px, black calc(100% - 80px), transparent 100%);
  text-align: left;
  transition: opacity 260ms ease;
  contain: layout style;
}

// 滚动轨道：Y 位移由 scrollSpring 通过 translate3d 驱动
.lyrics-track {
  position: relative;
  width: 100%;
  height: 100%;
  min-height: 100%;
  will-change: transform;
  transform: translate3d(0, 0, 0);
}

.lyrics-scroll-switching {
  opacity: 0.78;
}

.lyrics-pad-top { height: 34%; }
.lyrics-pad-bottom { height: 44%; }

// L7: 间奏呼吸点
.interlude-dots {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 14px 8px 14px 0;
  max-width: 860px;
  cursor: pointer;
  opacity: 0.32;
  transform: scale(0.9);
  transform-origin: left center;
  transition: opacity 320ms ease, transform 320ms cubic-bezier(0.22, 1, 0.36, 1);

  .interlude-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.84);
  }

  &.interlude-dots--active {
    opacity: calc(0.85 - var(--interlude-progress, 0) * 0.35);
    transform: scale(1);

    .interlude-dot {
      animation: interlude-breathe 1400ms ease-in-out infinite;
      box-shadow: none;
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
  max-width: min(980px, 100%);
  box-sizing: border-box;
  margin: 0;
  padding: 18px 8px 20px 0;
  text-align: left;
  // L3: Android TransformOrigin(0f, 1f) 左下角锚定，缩放时基线稳定
  transform-origin: left bottom;
  // transform / opacity / filter 由弹簧 RAF 循环直接写入，不用 CSS transition
  cursor: pointer;
  will-change: transform, opacity;
  backface-visibility: hidden;
  contain: layout style;

  &.active {
    transform-origin: left bottom;
  }
}

.line-text {
  display: block;
  font-size: calc(clamp(28px, 2.05vw, 42px) * var(--lyric-font-scale, 1));
  font-weight: 800;
  line-height: 1.32;
  letter-spacing: -0.015em;
  color: rgba(255, 255, 255, 0.46);
  white-space: pre-wrap;
  text-wrap: pretty;
  overflow-wrap: anywhere;
  position: relative;
  z-index: 1;
  .past & { color: rgba(255, 255, 255, 0.42); }
  .clear-text & { color: rgba(255, 255, 255, 0.62); }
  .clear-text.active & { color: rgba(255, 255, 255, 0.94); }
}

.lyric-line.active .line-text {
  color: rgba(255, 255, 255, 0.94);
  text-shadow: none;
}

.lyric-line.active .line-text--karaoke {
  color: rgba(255, 255, 255, 0.96);
}

.line-text--karaoke {
  --bright-mask-alpha: 1;
  --dark-mask-alpha: 0.2;
  display: block;
}

// KaraokeLine 创建的逐字 <span> 样式
:deep(.kw-wrapper) {
  display: inline-block;
  margin: -1em;
  padding: 1em;
  white-space: pre-wrap;
  vertical-align: bottom;
  contain: layout style;
}

:deep(.kw) {
  display: inline-block;
  margin: -1em;
  padding: 1em;
  color: inherit;
  text-shadow: none;
  font-weight: inherit;
  white-space: pre-wrap;
  vertical-align: bottom;
  transform-origin: center bottom;
  will-change: transform, text-shadow;
  backface-visibility: hidden;
}

:deep(.kw.emphasize > span) {
  display: inline-block;
  margin: -1em;
  padding: 1em;
  transform-origin: center bottom;
  will-change: transform, text-shadow;
  backface-visibility: hidden;
}

:deep(.line-text--active.kw-container .kw),
:deep(.lyric-line.active .kw-container .kw) {
  color: rgba(255, 255, 255, 0.96);
}

.line-tl {
  display: block;
  font-size: calc(clamp(17px, 1.25vw, 24px) * var(--lyric-font-scale, 1));
  font-weight: 650;
  color: rgba(255, 255, 255, 0.48);
  margin-top: 6px;
  line-height: 1.35;
  position: relative;
  z-index: 1;

  .active & {
    color: rgba(255, 255, 255, 0.74);
    text-shadow: none;
  }
  .past & { color: rgba(255, 255, 255, 0.40); }
}
</style>
