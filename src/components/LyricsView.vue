<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { DomLyricPlayer, type LyricLineMouseEvent } from '@amll-core/lyric-player/dom/index.ts'
import type {
  LyricLine as AmllLyricLine,
  LyricWord as AmllLyricWord,
} from '@amll-core/interfaces.ts'
import type {
  LyricLine as PlayerLyricLine,
  LyricWord as PlayerLyricWord,
} from '@/stores/player'
import { useSettingsStore } from '@/stores/settings'

const settings = useSettingsStore()

const props = withDefaults(defineProps<{
  lyrics: PlayerLyricLine[]
  currentTimeMs: number
  previewTimeMs?: number | null
  isPlaying: boolean
  lyricOffsetMs?: number
  seekSeq?: number
}>(), {
  currentTimeMs: 0,
  previewTimeMs: null,
  isPlaying: false,
  lyricOffsetMs: undefined,
  seekSeq: 0,
})

const emit = defineEmits<{ seek: [timeMs: number] }>()

const hostRef = ref<HTMLDivElement>()
const isLayoutReady = ref(false)
let lyricPlayer: DomLyricPlayer | null = null
let rafId = 0
let layoutFrameId = 0
let layoutSyncToken = 0
let lastFrameAt = 0
let lastSyncedTime = Number.NaN
let resizeObserver: ResizeObserver | null = null
let lastHostWidth = 0
let lastHostHeight = 0

const SPLIT_WHITESPACE_RE = /(\s+)/
const WHITESPACE_RE = /\s/g
const AMLL_WORD_FADE_WIDTH = 0.5
const LAYOUT_SETTLE_PASSES = 4
const LAYOUT_SETTLE_MAX_PASSES = 8
const SIZE_EPSILON = 0.5

interface PlayerRubyWord {
  startMs: number
  durationMs: number
  text: string
}

type RichPlayerLyricWord = PlayerLyricWord & {
  romanWord?: string
  obscene?: boolean
  ruby?: PlayerRubyWord[]
}

const offsetMs = computed(() => {
  if (typeof props.lyricOffsetMs === 'number') return props.lyricOffsetMs
  return settings.cloudMusicOffset || 0
})

const effectiveTimeMs = computed(() => {
  if (props.previewTimeMs != null) return props.previewTimeMs
  return props.currentTimeMs
})

const amllTimeMs = computed(() => Math.max(0, effectiveTimeMs.value + offsetMs.value))

function displayText(line: PlayerLyricLine): string {
  if (line.text) return line.text
  return (line.words || []).map(word => word.text).join('')
}

function hasWordTiming(line: PlayerLyricLine): boolean {
  return (line.words || []).some(word => word.durationMs > 0)
}

function normalizeWordTimings(line: PlayerLyricLine): PlayerLyricWord[] {
  const words = line.words || []
  const timedWords = words.filter(word => word.durationMs > 0)
  if (timedWords.length === 0) return words

  const lineStart = line.startMs
  const lineDuration = Math.max(0, line.durationMs)
  const firstWordStart = Math.min(...timedWords.map(word => word.startMs))
  const lastWordEnd = Math.max(...timedWords.map(word => word.startMs + word.durationMs))
  const usesRelativeTime = firstWordStart < lineStart - 250 && lastWordEnd <= lineDuration + 500

  if (!usesRelativeTime) return words

  return words.map(word => ({
    ...word,
    startMs: lineStart + word.startMs,
  }))
}

function restoreWhitespaceFromLineText(
  words: PlayerLyricWord[],
  lineText: string,
): PlayerLyricWord[] {
  if (!lineText || !/\s/.test(lineText) || words.some(word => /\s/.test(word.text))) {
    return words
  }

  const compactWords = words.map(word => word.text).join('').replace(/\s+/g, '')
  const compactLine = lineText.replace(/\s+/g, '')
  if (compactWords !== compactLine) return words

  const restored: PlayerLyricWord[] = []
  let cursor = 0

  for (const word of words) {
    const nextIndex = lineText.indexOf(word.text, cursor)
    if (nextIndex < 0) return words

    const between = lineText.slice(cursor, nextIndex)
    if (between) {
      if (between.trim()) return words
      restored.push({
        startMs: word.startMs,
        durationMs: 0,
        text: between,
      })
    }

    restored.push({
      ...word,
      text: lineText.slice(nextIndex, nextIndex + word.text.length),
    })
    cursor = nextIndex + word.text.length
  }

  const tail = lineText.slice(cursor)
  if (tail) {
    if (tail.trim()) return words
    const lastWord = words[words.length - 1]
    restored.push({
      startMs: lastWord ? lastWord.startMs + lastWord.durationMs : 0,
      durationMs: 0,
      text: tail,
    })
  }

  return restored
}

function splitWhitespaceAtoms(words: PlayerLyricWord[]): PlayerLyricWord[] {
  const result: RichPlayerLyricWord[] = []

  for (const word of words) {
    const richWord = word as RichPlayerLyricWord
    if (!word.text || !/\s/.test(word.text) || !word.text.trim() || (richWord.ruby?.length ?? 0) > 0) {
      result.push(word)
      continue
    }

    const parts = word.text.split(SPLIT_WHITESPACE_RE).filter(part => part.length > 0)
    const totalLength = word.text.replace(WHITESPACE_RE, '').length || 1
    const timePerUnit = word.durationMs / totalLength
    let currentOffset = 0

    for (const part of parts) {
      const startMs = word.startMs + currentOffset * timePerUnit
      if (!part.trim()) {
        result.push({
          startMs,
          durationMs: 0,
          text: part,
          obscene: richWord.obscene,
        })
        continue
      }

      const durationMs = part.length * timePerUnit
      result.push({
        startMs,
        durationMs,
        text: part,
        romanWord: richWord.romanWord,
        obscene: richWord.obscene,
      })
      currentOffset += part.length
    }
  }

  return result
}

function toAmllWord(word: PlayerLyricWord): AmllLyricWord {
  const richWord = word as RichPlayerLyricWord
  const startTime = Math.max(0, Math.round(word.startMs))
  const endTime = Math.max(startTime, Math.round(word.startMs + word.durationMs))
  const amllWord: AmllLyricWord = {
    word: word.text,
    startTime,
    endTime,
  }
  if (richWord.romanWord) amllWord.romanWord = richWord.romanWord
  if (richWord.obscene != null) amllWord.obscene = richWord.obscene
  if (richWord.ruby?.length) {
    amllWord.ruby = richWord.ruby.map(ruby => {
      const rubyStartTime = Math.max(0, Math.round(ruby.startMs))
      return {
        word: ruby.text,
        startTime: rubyStartTime,
        endTime: Math.max(rubyStartTime, Math.round(ruby.startMs + ruby.durationMs)),
      }
    })
  }
  return amllWord
}

function buildTimedWords(line: PlayerLyricLine): AmllLyricWord[] {
  if (!hasWordTiming(line)) return []

  const lineText = displayText(line)
  const normalizedWords = normalizeWordTimings(line)
  const restoredWords = restoreWhitespaceFromLineText(normalizedWords, lineText)
  return splitWhitespaceAtoms(restoredWords)
    .filter(word => word.text.length > 0)
    .map(toAmllWord)
}

function toAmllLine(line: PlayerLyricLine): AmllLyricLine {
  const startTime = Math.max(0, Math.round(line.startMs))
  const fallbackEndTime = Math.max(startTime + 1, Math.round(line.startMs + line.durationMs))
  const timedWords = buildTimedWords(line)
  const words = timedWords.length > 0
    ? timedWords
    : [{
        word: displayText(line),
        startTime,
        endTime: fallbackEndTime,
      }]
  const endTime = Math.max(
    fallbackEndTime,
    ...words.map(word => word.endTime),
    startTime + 1,
  )

  return {
    words,
    translatedLyric: settings.showTranslation ? (line.translation || '') : '',
    romanLyric: '',
    startTime,
    endTime,
    isBG: false,
    isDuet: false,
  }
}

function buildAmllLines(): AmllLyricLine[] {
  return props.lyrics.map(toAmllLine)
}

function syncCurrentTime(forceSeek = false): void {
  if (!lyricPlayer) return
  const time = Math.max(0, Math.round(amllTimeMs.value))
  if (!forceSeek && Math.abs(time - lastSyncedTime) < 1) return

  lyricPlayer.setCurrentTime(time, forceSeek)
  lastSyncedTime = time
}

function syncPlayState(): void {
  if (!lyricPlayer) return
  if (props.isPlaying) lyricPlayer.resume()
  else lyricPlayer.pause()
}

function syncLyricOptions(): void {
  if (!lyricPlayer) return
  lyricPlayer.setEnableBlur(settings.lyricBlur)
  lyricPlayer.setBlurAmount(settings.lyricBlurAmount)
  lyricPlayer.setWordFadeWidth(AMLL_WORD_FADE_WIDTH)
}

function reloadLyrics(): void {
  if (!lyricPlayer) return
  isLayoutReady.value = false
  const time = Math.max(0, Math.round(amllTimeMs.value))
  lyricPlayer.setLyricLines(buildAmllLines(), time)
  lyricPlayer.setCurrentTime(time, true)
  lyricPlayer.update(0)
  lastSyncedTime = time
  syncLyricOptions()
  syncPlayState()
  scheduleLayoutSync()
}

function onLineClick(event: Event): void {
  const lineEvent = event as LyricLineMouseEvent
  if (!lyricPlayer || lineEvent.lineIndex < 0) return

  const line = lyricPlayer.getLyricLines()[lineEvent.lineIndex]
  if (!line) return

  lineEvent.preventDefault()
  lyricPlayer.resetScroll()
  emit('seek', Math.max(0, Math.round(line.startTime - offsetMs.value)))
}

function startFrameLoop(): void {
  if (rafId) return
  lastFrameAt = performance.now()
  rafId = requestAnimationFrame(function tick(now) {
    const delta = Math.min(64, now - lastFrameAt)
    lastFrameAt = now
    lyricPlayer?.update(delta)
    rafId = requestAnimationFrame(tick)
  })
}

function stopFrameLoop(): void {
  if (!rafId) return
  cancelAnimationFrame(rafId)
  rafId = 0
}

function cancelLayoutSync(): void {
  if (layoutFrameId) cancelAnimationFrame(layoutFrameId)
  layoutFrameId = 0
  layoutSyncToken += 1
}

function syncPlayerElementSize(): boolean {
  if (!lyricPlayer) return false
  const playerElement = lyricPlayer.getElement()
  const width = playerElement.clientWidth || hostRef.value?.clientWidth || 0
  const height = playerElement.clientHeight || hostRef.value?.clientHeight || 0

  if (width > 0) lyricPlayer.size[0] = width
  if (height > 0) lyricPlayer.size[1] = height

  return width > 0 && height > 0
}

function syncMountedGroupSizes(): boolean {
  if (!lyricPlayer) return false
  let changed = false

  for (const group of lyricPlayer.currentLyricGroups) {
    const element = group.element
    if (!element.parentElement) continue

    const width = element.clientWidth
    const height = element.clientHeight
    if (width <= 0 || height <= 0) continue

    const previous = lyricPlayer.lyricGroupSize.get(group)
    if (
      !previous ||
      Math.abs(previous[0] - width) > SIZE_EPSILON ||
      Math.abs(previous[1] - height) > SIZE_EPSILON
    ) {
      const nextSize: [number, number] = [width, height]
      lyricPlayer.lyricGroupSize.set(group, nextSize)
      group.onLineSizeChange(nextSize)
      changed = true
    }
  }

  return changed
}

function forceLayoutAtCurrentTime(): boolean {
  if (!lyricPlayer) return false
  const hasPlayerSize = syncPlayerElementSize()
  const time = Math.max(0, Math.round(amllTimeMs.value))

  lyricPlayer.setCurrentTime(time, true)
  const changedBeforeLayout = syncMountedGroupSizes()
  void lyricPlayer.calcLayout(true, true)
  lyricPlayer.update(0)
  const changedAfterLayout = syncMountedGroupSizes()

  if (changedBeforeLayout || changedAfterLayout) {
    void lyricPlayer.calcLayout(true, true)
    lyricPlayer.update(0)
  }

  lastSyncedTime = time
  return hasPlayerSize
}

function finishLayoutSync(token: number): void {
  if (!lyricPlayer || token !== layoutSyncToken) return
  const settledTime = Math.max(0, Math.round(amllTimeMs.value))

  lyricPlayer.setCurrentTime(settledTime, true)
  forceLayoutAtCurrentTime()
  lyricPlayer.setCurrentTime(settledTime, false)
  syncPlayState()
  lyricPlayer.update(0)
  lastSyncedTime = settledTime
  isLayoutReady.value = true
}

function scheduleFontReadyLayout(token: number): void {
  const fonts = document.fonts
  if (!fonts || fonts.status === 'loaded') return

  void fonts.ready.then(() => {
    if (!lyricPlayer || token !== layoutSyncToken) return
    scheduleLayoutSync()
  })
}

function scheduleLayoutSync(): void {
  if (!lyricPlayer) return
  if (layoutFrameId) cancelAnimationFrame(layoutFrameId)

  const token = layoutSyncToken + 1
  layoutSyncToken = token
  isLayoutReady.value = false
  let pass = 0

  const runPass = () => {
    layoutFrameId = requestAnimationFrame(() => {
      layoutFrameId = 0
      if (!lyricPlayer || token !== layoutSyncToken) return

      pass += 1
      const hasPlayerSize = forceLayoutAtCurrentTime()
      const needsMorePasses =
        pass < LAYOUT_SETTLE_PASSES ||
        (!hasPlayerSize && pass < LAYOUT_SETTLE_MAX_PASSES)

      if (needsMorePasses) {
        runPass()
        return
      }

      finishLayoutSync(token)
    })
  }

  runPass()
  scheduleFontReadyLayout(token)
}

function startResizeObserver(): void {
  if (!hostRef.value || typeof ResizeObserver === 'undefined') return

  resizeObserver = new ResizeObserver(entries => {
    const entry = entries[0]
    if (!entry) return

    const width = entry.contentRect.width
    const height = entry.contentRect.height
    if (width <= 0 || height <= 0) return

    const hasSizeChanged =
      Math.abs(width - lastHostWidth) > SIZE_EPSILON ||
      Math.abs(height - lastHostHeight) > SIZE_EPSILON
    lastHostWidth = width
    lastHostHeight = height

    if (hasSizeChanged) scheduleLayoutSync()
  })
  resizeObserver.observe(hostRef.value)
}

function stopResizeObserver(): void {
  resizeObserver?.disconnect()
  resizeObserver = null
}

onMounted(() => {
  nextTick(() => {
    if (!hostRef.value || lyricPlayer) return

    lyricPlayer = new DomLyricPlayer()
    lyricPlayer.addEventListener('line-click', onLineClick as EventListener)
    hostRef.value.appendChild(lyricPlayer.getElement())

    startResizeObserver()
    reloadLyrics()
    startFrameLoop()
  })
})

onUnmounted(() => {
  stopFrameLoop()
  cancelLayoutSync()
  stopResizeObserver()
  if (!lyricPlayer) return

  lyricPlayer.removeEventListener('line-click', onLineClick as EventListener)
  lyricPlayer.dispose()
  lyricPlayer = null
})

watch(() => props.lyrics, () => {
  reloadLyrics()
}, { deep: false })

watch(() => settings.showTranslation, () => {
  reloadLyrics()
})

watch([() => settings.lyricBlur, () => settings.lyricBlurAmount], () => {
  syncLyricOptions()
})

watch(() => settings.lyricFontScale, () => {
  scheduleLayoutSync()
})

watch(() => props.isPlaying, () => {
  syncPlayState()
})

watch(amllTimeMs, (time, oldTime) => {
  const isPreviewing = props.previewTimeMs != null
  const isLargeJump = oldTime !== undefined && Math.abs(time - oldTime) > 1000
  const forceSeek = isPreviewing || isLargeJump
  syncCurrentTime(forceSeek)
})

watch(() => props.seekSeq, (seq, oldSeq) => {
  if (seq === oldSeq) return
  syncCurrentTime(true)
})
</script>

<template>
  <div
    ref="hostRef"
    class="lyrics-scroll"
    :class="{
      'lyrics-scroll--ready': isLayoutReady,
    }"
    :style="{ '--lyric-font-scale': settings.lyricFontScale }"
  />
</template>

<style scoped lang="scss">
.lyrics-scroll {
  width: 100%;
  height: 100%;
  overflow: hidden;
  position: relative;
  text-align: left;
  color: white;
  mask-image: linear-gradient(
    to bottom,
    transparent 0%,
    black 13%,
    black 78%,
    transparent 100%
  );
  -webkit-mask-image: linear-gradient(
    to bottom,
    transparent 0%,
    black 13%,
    black 78%,
    transparent 100%
  );
  --amll-lp-color: white;
  --amll-lp-font-size: calc(max(max(5vh, 2.5vw), 12px) * var(--lyric-font-scale, 1));
  --amll-lp-emphasis-glow-opacity-boost: 5.6;
  --amll-lp-emphasis-glow-min-opacity: 0.58;
  --amll-lp-emphasis-glow-max-opacity: 0.96;
  --amll-lp-emphasis-glow-radius-boost: 3;
  --amll-lp-emphasis-glow-min-radius: 0.2;
  --amll-lp-emphasis-glow-max-radius: 0.7;
}

:deep(.amll-lyric-player [class*="interludeDots"]) {
  z-index: 2;
}

.lyrics-scroll:not(.lyrics-scroll--ready) :deep(.amll-lyric-player) {
  visibility: hidden;
  opacity: 0 !important;
  pointer-events: none;
}

:deep(.amll-lyric-player) {
  text-align: left;
  font-weight: 850;
  font-variation-settings: 'wght' 850;
  -webkit-font-smoothing: antialiased;
  --amll-lp-line-width-aspect: 0.82;
}

:deep(.amll-lyric-player [class*="lyricMainLine"]) {
  font-weight: 850;
  font-variation-settings: 'wght' 850;
  letter-spacing: -0.025em;
}

:deep(.amll-lyric-player [class*="lyricSubLine"]) {
  font-weight: 650;
  font-variation-settings: 'wght' 650;
}

</style>
