<script setup lang="ts">
import { ref, watch } from 'vue'
import { normalizeBilibiliCoverUrl, resolveBilibiliCover } from '@/utils/bilibiliCover'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  src?: string | null
  alt?: string
}>(), {
  src: '',
  alt: '',
})

const currentSrc = ref('')
const isLoaded = ref(false)
const proxyAttempted = ref(false)

watch(() => props.src, (src) => {
  currentSrc.value = normalizeBilibiliCoverUrl(src || '')
  isLoaded.value = false
  proxyAttempted.value = false
}, { immediate: true })

function handleLoad() {
  isLoaded.value = true
}

async function handleError() {
  isLoaded.value = false
  if (!props.src || proxyAttempted.value) {
    currentSrc.value = ''
    return
  }

  proxyAttempted.value = true
  const failedSrc = props.src
  try {
    const resolvedSrc = await resolveBilibiliCover(failedSrc)
    if (props.src === failedSrc) currentSrc.value = resolvedSrc
  } catch {
    if (props.src === failedSrc) currentSrc.value = ''
  }
}
</script>

<template>
  <img
    v-if="currentSrc"
    v-bind="$attrs"
    :src="currentSrc"
    :alt="alt"
    :style="[$attrs.style, { visibility: isLoaded ? undefined : 'hidden' }]"
    referrerpolicy="no-referrer"
    @load="handleLoad"
    @error="handleError"
  />
  <slot v-else />
</template>
