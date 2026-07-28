import { ref, watch, onUnmounted, type Ref } from 'vue'

/**
 * 延迟标志：仅当 source 持续为 true 超过 delayMs 才置 true；source 转 false 立即清零。
 *
 * 用于避免快速加载时 spinner 一闪而过（对齐 player 的 isLoadingAudioSlow 约定，UI-016）。
 * 页面级加载用它包裹 isLoading，短加载不显示 spinner，只有超过阈值的慢加载才显示。
 */
export function useDelayedFlag(source: Ref<boolean>, delayMs = 1000): Ref<boolean> {
  const delayed = ref(false)
  let timer: ReturnType<typeof setTimeout> | null = null
  const clear = () => {
    if (timer) {
      clearTimeout(timer)
      timer = null
    }
  }
  watch(
    source,
    (v) => {
      clear()
      if (v) {
        timer = setTimeout(() => { delayed.value = true }, delayMs)
      } else {
        delayed.value = false
      }
    },
    { immediate: true },
  )
  onUnmounted(clear)
  return delayed
}
