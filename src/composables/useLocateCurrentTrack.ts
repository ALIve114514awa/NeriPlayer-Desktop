import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type ComputedRef, type Ref } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'

/** 行高亮脉冲的全局类名，样式定义在 styles/global.scss */
const PULSE_CLASS = 'np-locate-pulse'
/** 行可见比例阈值：超过一半露出才算"在可视区域内" */
const VISIBLE_RATIO = 0.5
/** 顶部 sticky 搜索头的遮挡高度，行藏在其下时不算可见 */
const STICKY_HEADER_OFFSET = 56

export interface UseLocateCurrentTrackOptions {
  /** 详情页根元素（.detail-view，同时是滚动容器） */
  containerRef: Ref<HTMLElement | null>
  /** 当前播放曲目在本列表中的行 key；null 表示当前曲目不属于本列表 */
  currentKey: Ref<string | null> | ComputedRef<string | null>
  /** 自定义行定位；默认按 [data-track-key="<key>"] 查找 */
  resolveRow?: (container: HTMLElement, key: string) => HTMLElement | null
  /** 行尚未渲染时（窗口渐扩列表）请求扩容；返回 false 表示该行无法被渲染 */
  ensureRendered?: (key: string) => boolean
  /** 为 true 时抑制 FAB（如多选模式让位） */
  suppressed?: Ref<boolean> | ComputedRef<boolean>
}

function escapeAttrValue(key: string): string {
  return typeof CSS !== 'undefined' && typeof CSS.escape === 'function'
    ? CSS.escape(key)
    : key.replace(/["\\]/g, '\\$&')
}

function defaultResolveRow(container: HTMLElement, key: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-track-key="${escapeAttrValue(key)}"]`)
}

function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches
}

/** 同步估算一次行可见比例，避免等 IO 回调期间 FAB 闪现 */
function rowVisibleRatio(container: HTMLElement, row: HTMLElement): number {
  const containerRect = container.getBoundingClientRect()
  const rowRect = row.getBoundingClientRect()
  if (rowRect.height <= 0) return 0
  const top = Math.max(rowRect.top, containerRect.top + STICKY_HEADER_OFFSET)
  const bottom = Math.min(rowRect.bottom, containerRect.bottom)
  return Math.max(0, bottom - top) / rowRect.height
}

/** 行到位后闪一次主题色脉冲；连续触发时先移除类并强制重排以重播动画 */
function pulseRow(row: HTMLElement) {
  row.classList.remove(PULSE_CLASS)
  void row.offsetWidth
  row.classList.add(PULSE_CLASS)
  const clear = () => row.classList.remove(PULSE_CLASS)
  row.addEventListener('animationend', clear, { once: true })
  window.setTimeout(clear, 800)
}

/** 平滑滚动结束后回调：优先 scrollend，缺失（旧 WKWebView）时定时兜底 */
function afterScrollSettles(container: HTMLElement, done: () => void) {
  const supportsScrollEnd = 'onscrollend' in window
  let finished = false
  let timer = 0
  const finish = () => {
    if (finished) return
    finished = true
    window.clearTimeout(timer)
    container.removeEventListener('scrollend', finish)
    done()
  }
  timer = window.setTimeout(finish, supportsScrollEnd ? 1000 : 550)
  if (supportsScrollEnd) {
    container.addEventListener('scrollend', finish, { once: true, passive: true })
  }
}

/**
 * 「定位到当前播放曲目」悬浮按钮的共用逻辑。
 *
 * - fabVisible：当前曲目属于本列表且其行不可见（含未渲染）时为 true
 * - locate()：滚动使当前行居中，到位后行高亮脉冲一次
 *
 * 行可见性用 IntersectionObserver 观察，行元素被列表重渲染替换时
 * 由 MutationObserver 触发重绑，全程无每帧计算。
 */
export function useLocateCurrentTrack(options: UseLocateCurrentTrackOptions) {
  const { containerRef, currentKey } = options
  const resolveRow = options.resolveRow ?? defaultResolveRow

  const rowVisible = ref(false)
  const leaving = ref(false)

  let io: IntersectionObserver | null = null
  let mo: MutationObserver | null = null
  let observedRow: HTMLElement | null = null
  let boundContainer: HTMLElement | null = null
  let resyncScheduled = false

  function findRow(container: HTMLElement, key: string): HTMLElement | null {
    return resolveRow(container, key)
  }

  function resync() {
    const container = boundContainer
    const key = currentKey.value
    const row = container && key ? findRow(container, key) : null
    if (row === observedRow) {
      if (!row) rowVisible.value = false
      return
    }
    if (observedRow && io) io.unobserve(observedRow)
    observedRow = row
    if (!row || !container || !io) {
      rowVisible.value = false
      return
    }
    // 先同步估算一次，IO 回调稍后接管，避免切歌瞬间 FAB 闪现
    rowVisible.value = rowVisibleRatio(container, row) >= VISIBLE_RATIO
    io.observe(row)
  }

  function scheduleResync() {
    if (resyncScheduled) return
    resyncScheduled = true
    window.requestAnimationFrame(() => {
      resyncScheduled = false
      resync()
    })
  }

  function teardown() {
    io?.disconnect()
    io = null
    mo?.disconnect()
    mo = null
    observedRow = null
    boundContainer = null
    rowVisible.value = false
  }

  function bindContainer(container: HTMLElement | null) {
    if (container === boundContainer) return
    teardown()
    if (!container) return
    boundContainer = container
    io = new IntersectionObserver(entries => {
      for (const entry of entries) {
        if (entry.target !== observedRow) continue
        rowVisible.value = entry.isIntersecting && entry.intersectionRatio >= VISIBLE_RATIO
      }
    }, {
      root: container,
      rootMargin: `-${STICKY_HEADER_OFFSET}px 0px 0px 0px`,
      threshold: [0, VISIBLE_RATIO],
    })
    // 列表重渲染（搜索过滤、窗口扩容）会换掉行元素，靠 DOM 变更事件重绑观察
    mo = new MutationObserver(scheduleResync)
    mo.observe(container, { childList: true, subtree: true })
    resync()
  }

  const fabVisible = computed(() =>
    !leaving.value
    && options.suppressed?.value !== true
    && currentKey.value !== null
    && !rowVisible.value,
  )

  async function locate() {
    const container = boundContainer ?? containerRef.value
    const key = currentKey.value
    if (!container || !key) return
    let row = findRow(container, key)
    if (!row && options.ensureRendered) {
      // 窗口渐扩列表：先扩容到包含目标行再定位
      if (!options.ensureRendered(key)) return
      await nextTick()
      row = findRow(container, key)
    }
    if (!row) return

    const reduced = prefersReducedMotion()
    const alreadyCentered = rowVisibleRatio(container, row) >= 1
    row.scrollIntoView({ block: 'center', behavior: reduced ? 'auto' : 'smooth' })
    // reduced-motion 下省略脉冲
    if (reduced) return
    if (alreadyCentered) {
      pulseRow(row)
      return
    }
    afterScrollSettles(container, () => {
      // 滚动期间列表可能重渲染，脉冲前重新解析行元素
      const settled = findRow(container, key)
      if (settled) pulseRow(settled)
    })
  }

  watch(() => currentKey.value, scheduleResync, { flush: 'post' })
  watch(containerRef, container => bindContainer(container), { flush: 'post' })

  onMounted(() => bindContainer(containerRef.value))
  onBeforeUnmount(teardown)
  // 路由离场立即淡出，让 200ms 过渡赶在视图卸载前完成
  onBeforeRouteLeave(() => {
    leaving.value = true
  })

  return { fabVisible, locate }
}
