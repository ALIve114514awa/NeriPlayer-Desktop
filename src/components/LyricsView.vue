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
let layoutSettleFrameId = 0
let lastFrameAt = 0
let lastSyncedTime = Number.NaN

const SPLIT_WHITESPACE_RE = /(\s+)/
const WHITESPACE_RE = /\s/g

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
  const result: PlayerLyricWord[] = []

  for (const word of words) {
    if (!word.text || !/\s/.test(word.text) || !word.text.trim()) {
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
        })
        continue
      }

      const durationMs = part.length * timePerUnit
      result.push({
        startMs,
        durationMs,
        text: part,
      })
      currentOffset += part.length
    }
  }

  return result
}

function toAmllWord(word: PlayerLyricWord): AmllLyricWord {
  const startTime = Math.max(0, Math.round(word.startMs))
  const endTime = Math.max(startTime, Math.round(word.startMs + word.durationMs))
  return {
    word: word.text,
    startTime,
    endTime,
  }
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
  if (forceSeek) scheduleLayoutSync()
}

function syncPlayState(): void {
  if (!lyricPlayer) return
  if (props.isPlaying) lyricPlayer.resume()
  else lyricPlayer.pause()
}

function syncLyricOptions(): void {
  if (!lyricPlayer) return
  lyricPlayer.setEnableBlur(settings.lyricBlur)
  lyricPlayer.setWordFadeWidth(0.5)
}

function reloadLyrics(): void {
  if (!lyricPlayer) return
  isLayoutReady.value = false
  const time = Math.max(0, Math.round(amllTimeMs.value))
  lyricPlayer.setLyricLines(buildAmllLines(), time)
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
  if (layoutSettleFrameId) cancelAnimationFrame(layoutSettleFrameId)
  layoutFrameId = 0
  layoutSettleFrameId = 0
}

function scheduleLayoutSync(): void {
  if (!lyricPlayer) return
  cancelLayoutSync()
  isLayoutReady.value = false

  layoutFrameId = requestAnimationFrame(() => {
    layoutFrameId = 0
    if (!lyricPlayer) return
    const time = Math.max(0, Math.round(amllTimeMs.value))
    lyricPlayer.setCurrentTime(time, true)
    void lyricPlayer.calcLayout(true, true)
    lyricPlayer.update(0)

    layoutSettleFrameId = requestAnimationFrame(() => {
      layoutSettleFrameId = 0
      if (!lyricPlayer) return
      lyricPlayer.setCurrentTime(time, true)
      void lyricPlayer.calcLayout(true, true)
      lyricPlayer.update(0)
      lyricPlayer.setCurrentTime(time, false)
      syncPlayState()
      lyricPlayer.update(0)
      isLayoutReady.value = true
    })
  })
}

onMounted(() => {
  nextTick(() => {
    if (!hostRef.value || lyricPlayer) return

    lyricPlayer = new DomLyricPlayer()
    lyricPlayer.addEventListener('line-click', onLineClick as EventListener)
    hostRef.value.appendChild(lyricPlayer.getElement())

    reloadLyrics()
    syncCurrentTime(true)
    startFrameLoop()
  })
})

onUnmounted(() => {
  stopFrameLoop()
  cancelLayoutSync()
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

watch(() => settings.lyricBlur, () => {
  syncLyricOptions()
})

watch(() => props.isPlaying, () => {
  syncPlayState()
})

watch(amllTimeMs, (time, oldTime) => {
  const isPreviewing = props.previewTimeMs != null
  const isLargeJump = oldTime !== undefined && Math.abs(time - oldTime) > 1000
  syncCurrentTime(isPreviewing || isLargeJump)
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
    :class="{ 'lyrics-scroll--ready': isLayoutReady }"
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
  -webkit-font-smoothing: antialiased;
  --amll-lp-line-width-aspect: 0.82;
}

:deep(.amll-lyric-player [class*="lyricMainLine"]) {
  font-weight: 850;
  letter-spacing: -0.025em;
}

:deep(.amll-lyric-player [class*="lyricSubLine"]) {
  font-weight: 650;
}

:deep(.amll-lyric-player [class*="active"] [class*="emphasize"]:not([class*="Wrapper"]) > span) {
  text-shadow:
    0 0 0.18em rgba(255, 255, 255, 0.42),
    0 0 0.38em rgba(255, 255, 255, 0.22);
}
</style>
