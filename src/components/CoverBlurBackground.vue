<script setup lang="ts">
/**
 * 封面模糊背景组件
 * 将封面图全屏铺底并应用高斯模糊 + 暗化遮罩
 * 作为 HyperBackground 的替代背景模式
 *
 * 切歌时使用双缓冲交叉淡入淡出，消除闪烁
 */
import { ref, watch, onUnmounted } from 'vue'

const props = defineProps<{
  coverUrl: string
  blurAmount: number   // 模糊像素值
  darkenAlpha: number  // 暗化遮罩透明度 0-1
}>()

// 双缓冲：front 为当前显示，back 为预加载
const frontUrl = ref(props.coverUrl)
const backUrl = ref('')
const showBack = ref(false)

// in-flight 状态：用于取消迟到的 onload 与清理定时器，避免快速切歌时乱序覆盖
let pendingImg: HTMLImageElement | null = null
let swapTimer: ReturnType<typeof setTimeout> | null = null

watch(() => props.coverUrl, (newUrl) => {
  if (!newUrl || newUrl === frontUrl.value) return

  // 取消上一张仍在加载/等待交换的图，防止迟到回调覆盖更新的封面
  if (pendingImg) { pendingImg.onload = null; pendingImg.onerror = null; pendingImg = null }
  if (swapTimer) { clearTimeout(swapTimer); swapTimer = null }

  // 预加载新图片，加载完成后交叉淡入
  const img = new Image()
  pendingImg = img
  img.crossOrigin = 'anonymous'
  img.referrerPolicy = 'no-referrer'
  img.onload = () => {
    // 若期间又切歌，当前回调已过期，丢弃
    if (newUrl !== props.coverUrl) return
    pendingImg = null
    backUrl.value = newUrl
    // 触发淡入 back 层
    requestAnimationFrame(() => {
      showBack.value = true
      // 过渡结束后交换：back→front，重置 back
      swapTimer = setTimeout(() => {
        swapTimer = null
        frontUrl.value = newUrl
        showBack.value = false
        backUrl.value = ''
      }, 720) // 与 CSS transition 时长一致
    })
  }
  img.onerror = () => {
    if (newUrl !== props.coverUrl) return
    pendingImg = null
    // 加载失败直接切换，不做动画
    frontUrl.value = newUrl
  }
  img.src = newUrl
})

onUnmounted(() => {
  if (pendingImg) { pendingImg.onload = null; pendingImg.onerror = null; pendingImg = null }
  if (swapTimer) { clearTimeout(swapTimer); swapTimer = null }
})
</script>

<template>
  <div class="cover-blur-bg">
    <!-- Front 层：当前显示 -->
    <img
      v-if="frontUrl"
      :src="frontUrl"
      referrerpolicy="no-referrer"
      class="cover-blur-img front"
      :style="{ filter: `blur(${blurAmount}px)` }"
    />
    <!-- Back 层：预加载完成后淡入覆盖 front -->
    <img
      v-if="backUrl"
      :src="backUrl"
      referrerpolicy="no-referrer"
      class="cover-blur-img back"
      :class="{ visible: showBack }"
      :style="{ filter: `blur(${blurAmount}px)` }"
    />
    <div class="cover-blur-darken" :style="{ background: `rgba(0,0,0,${darkenAlpha})` }" />
  </div>
</template>

<style scoped>
.cover-blur-bg {
  position: absolute;
  inset: 0;
  z-index: 0;
  overflow: hidden;
}

.cover-blur-img {
  position: absolute;
  /* 超出边缘避免模糊白边 */
  width: calc(100% + 80px);
  height: calc(100% + 80px);
  top: -40px;
  left: -40px;
  object-fit: cover;
  will-change: filter, opacity;
}

.cover-blur-img.front {
  z-index: 0;
}

.cover-blur-img.back {
  z-index: 1;
  opacity: 0;
  transition: opacity 0.72s cubic-bezier(0.22, 1, 0.36, 1);
}

.cover-blur-img.back.visible {
  opacity: 1;
}

.cover-blur-darken {
  position: absolute;
  inset: 0;
  z-index: 2;
}
</style>
