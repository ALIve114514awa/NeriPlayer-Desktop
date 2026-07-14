<script setup lang="ts">
import { ref, watch } from 'vue'
import { resolveBilibiliCover } from '@/utils/bilibiliCover'

defineOptions({ inheritAttrs: false })

const props = withDefaults(defineProps<{
  src?: string | null
  alt?: string
}>(), {
  src: '',
  alt: '',
})

const resolvedSrc = ref('')
const isLoaded = ref(false)

watch(() => props.src, async (src) => {
  resolvedSrc.value = ''
  isLoaded.value = false
  if (!src) return

  try {
    resolvedSrc.value = await resolveBilibiliCover(src)
  } catch {
    resolvedSrc.value = ''
  }
}, { immediate: true })

function handleLoad() {
  isLoaded.value = true
}

function handleError() {
  resolvedSrc.value = ''
  isLoaded.value = false
}
</script>

<template>
  <img
    v-if="resolvedSrc"
    v-bind="$attrs"
    :src="resolvedSrc"
    :alt="alt"
    referrerpolicy="no-referrer"
    @load="handleLoad"
    @error="handleError"
  />
  <slot v-else />
</template>
