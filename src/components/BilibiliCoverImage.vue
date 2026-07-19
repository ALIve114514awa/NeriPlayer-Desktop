<script setup lang="ts">
import { ref, watch } from 'vue'
import {
  normalizeCoverUrlForDisplay,
  normalizeProxiedCoverUrl,
  peekCoverImage,
  resolveCoverImage,
} from '@/utils/bilibiliCover'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  src?: string | null
  alt?: string
}>(), {
  src: '',
  alt: '',
})

const resolvedSrc = ref('')
let resolveRequestToken = 0
let renderRetryCount = 0

watch(() => props.src, (src) => {
  resolveRequestToken++
  resolvedSrc.value = peekCoverImage(src || '')
  renderRetryCount = 0
  if (!src) return

  if (resolvedSrc.value) return

  const proxiedUrl = normalizeProxiedCoverUrl(src)
  if (!proxiedUrl) {
    resolvedSrc.value = normalizeCoverUrlForDisplay(src)
    return
  }
  void loadCover(proxiedUrl, false)
}, { immediate: true })

async function loadCover(src: string, forceRefresh: boolean) {
  const requestToken = ++resolveRequestToken
  try {
    const nextSrc = await resolveCoverImage(src, { forceRefresh })
    if (requestToken === resolveRequestToken) resolvedSrc.value = nextSrc
  } catch {
    if (requestToken === resolveRequestToken) resolvedSrc.value = ''
  }
}

function handleError(event: Event) {
  const failedSrc = (event.currentTarget as HTMLImageElement).src
  const proxiedUrl = normalizeProxiedCoverUrl(props.src || '')
  if (!proxiedUrl || failedSrc !== resolvedSrc.value) return

  resolvedSrc.value = ''
  if (renderRetryCount >= 1) return

  renderRetryCount++
  void loadCover(proxiedUrl, true)
}
</script>

<template>
  <img
    v-if="resolvedSrc"
    v-bind="$attrs"
    :src="resolvedSrc"
    :alt="alt"
    referrerpolicy="no-referrer"
    @error="handleError"
  />
  <slot v-else />
</template>
