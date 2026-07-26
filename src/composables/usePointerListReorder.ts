import { onUnmounted, ref, type Ref } from 'vue'

// 指针拖拽列表排序（抽取自 LocalPlaylistView 的歌曲拖拽实现，交互与动效参数保持一致）
// 行元素需带 data-drag-key 标识，列表容器需 position: relative 以承载单实例落点指示线
export interface PointerListReorderOptions {
  /** 列表容器（行的 offsetParent，需 position: relative） */
  listRef: Ref<HTMLElement | null>
  /** 行元素选择器 */
  itemSelector: string
  /** 拖拽到边缘时自动滚动的容器；缺省回退到最近的 .content */
  getScrollContainer?: () => HTMLElement | null
  /** 是否允许开始拖拽 */
  canDrag?: () => boolean
  /** 该行是否可作为落点（用于系统歌单等固定行） */
  isValidTargetKey?: (key: string) => boolean
  /** 松手后的实际重排回调，由调用方完成数组重排与持久化 */
  onReorder: (fromKey: string, toKey: string, position: 'before' | 'after') => void
}

interface RowSnapshot {
  key: string
  top: number
  bottom: number
  midpoint: number
  layoutTop: number
  layoutHeight: number
  targetable: boolean
}

export function usePointerListReorder(options: PointerListReorderOptions) {
  const dragKey = ref<string | null>(null)
  const dragOverKey = ref<string | null>(null)
  const dragInsertPosition = ref<'before' | 'after' | null>(null)
  const dragLandingKey = ref<string | null>(null)
  const dragUnderGlassKeys = ref<Set<string>>(new Set())
  const dragGlassActive = ref(false)
  // 单实例落点指示线：位置用 transform 平移，隐藏时保留位置以便原地淡出
  const dropIndicatorVisible = ref(false)
  const dropIndicatorY = ref(0)
  const dropIndicatorSnap = ref(false)
  const dragOffsetY = ref(0)

  let dragPointerId: number | null = null
  let dragHandleElement: HTMLElement | null = null
  let dragStartPointerY = 0
  let dragStartRowTop = 0
  let dragRowHeight = 0
  let dragRowSnapshots: RowSnapshot[] = []
  let dragLandingTimer: ReturnType<typeof window.setTimeout> | null = null
  // 拖拽行的相邻行 key，用于屏蔽等价于原位的无效落点
  let dragPrevSiblingKey: string | null = null
  let dragNextSiblingKey: string | null = null
  let dropIndicatorSnapFrame: number | null = null
  let dragGlassStartFrame: number | null = null
  let dragGlassActiveFrame: number | null = null

  function dragItemStyle(key: string): { '--drag-offset': string } | undefined {
    if (dragKey.value !== key) return undefined
    return { '--drag-offset': `${dragOffsetY.value}px` }
  }

  function clearDragLandingState() {
    if (dragLandingTimer !== null) {
      window.clearTimeout(dragLandingTimer)
      dragLandingTimer = null
    }
    dragLandingKey.value = null
  }

  function startDragLandingState(key: string | null) {
    clearDragLandingState()
    if (!key) return
    dragLandingKey.value = key
    dragLandingTimer = window.setTimeout(() => {
      dragLandingKey.value = null
      dragLandingTimer = null
    }, 320)
  }

  function cancelDragGlassActivation() {
    if (dragGlassStartFrame !== null) {
      window.cancelAnimationFrame(dragGlassStartFrame)
      dragGlassStartFrame = null
    }
    if (dragGlassActiveFrame !== null) {
      window.cancelAnimationFrame(dragGlassActiveFrame)
      dragGlassActiveFrame = null
    }
  }

  function scheduleDragGlassActivation() {
    cancelDragGlassActivation()
    dragGlassActive.value = false
    dragGlassStartFrame = window.requestAnimationFrame(() => {
      dragGlassStartFrame = null
      dragGlassActiveFrame = window.requestAnimationFrame(() => {
        dragGlassActiveFrame = null
        dragGlassActive.value = dragKey.value !== null
      })
    })
  }

  function captureDragSnapshot() {
    const list = options.listRef.value
    const draggedKey = dragKey.value
    if (!list || !draggedKey) {
      dragRowSnapshots = []
      return
    }

    const listRect = list.getBoundingClientRect()
    const rows = [...list.querySelectorAll<HTMLElement>(options.itemSelector)]
    const draggedIndex = rows.findIndex(row => (row.dataset.dragKey || '') === draggedKey)
    dragPrevSiblingKey = draggedIndex > 0 ? rows[draggedIndex - 1].dataset.dragKey || null : null
    dragNextSiblingKey = draggedIndex >= 0 && draggedIndex < rows.length - 1
      ? rows[draggedIndex + 1].dataset.dragKey || null
      : null
    // 用布局位置（offsetTop）而非 getBoundingClientRect：目标行的位移过渡带 transform，
    // 若采样含 transform 会把视觉位移反馈进目标计算，造成边界处目标来回翻转闪烁
    dragRowSnapshots = rows
      .map(row => {
        const layoutTop = row.offsetTop
        const layoutHeight = row.offsetHeight
        const top = listRect.top + layoutTop
        const key = row.dataset.dragKey || ''
        return {
          key,
          top,
          bottom: top + layoutHeight,
          midpoint: top + layoutHeight / 2,
          layoutTop,
          layoutHeight,
          targetable: options.isValidTargetKey ? options.isValidTargetKey(key) : true,
        }
      })
      .filter(row => row.key && row.key !== draggedKey)
  }

  function updateDragUnderGlassKeys() {
    if (!dragKey.value || dragRowHeight <= 0) {
      dragUnderGlassKeys.value = new Set()
      return
    }

    const top = dragStartRowTop + dragOffsetY.value
    const bottom = top + dragRowHeight
    const overlapInset = 8
    dragUnderGlassKeys.value = new Set(
      dragRowSnapshots
        .filter(row => row.bottom > top + overlapInset && row.top < bottom - overlapInset)
        .map(row => row.key),
    )
  }

  function resolveInsertTarget(e: PointerEvent): { key: string; position: 'before' | 'after' } | null {
    const targetRows = dragRowSnapshots.filter(row => row.targetable)
    if (targetRows.length === 0) return null

    // 允许拖到视口外时仍按最近行判定，配合自动滚动
    for (const row of targetRows) {
      if (e.clientY < row.midpoint) {
        return { key: row.key, position: 'before' }
      }
    }

    const lastRow = targetRows[targetRows.length - 1]
    return lastRow ? { key: lastRow.key, position: 'after' } : null
  }

  function updateDragTarget(e: PointerEvent) {
    const target = resolveInsertTarget(e)
    // 与原位等价的落点不提示：拖拽行下一行的 before / 上一行的 after 都是无效移动
    const isNoopTarget =
      !!target
      && ((target.position === 'before' && target.key === dragNextSiblingKey)
        || (target.position === 'after' && target.key === dragPrevSiblingKey))
    if (!target || isNoopTarget || target.key === dragKey.value) {
      dragOverKey.value = null
      dragInsertPosition.value = null
      dropIndicatorVisible.value = false
      return
    }
    // 同一目标不重复更新，避免每帧刷新样式
    if (target.key === dragOverKey.value && target.position === dragInsertPosition.value) return
    dragOverKey.value = target.key
    dragInsertPosition.value = target.position
    moveDropIndicatorToTarget(target)
  }

  function moveDropIndicatorToTarget(target: { key: string; position: 'before' | 'after' }) {
    const snap = dragRowSnapshots.find(row => row.key === target.key)
    if (!snap) {
      dropIndicatorVisible.value = false
      return
    }
    // 指示线中心对准两行之间 2px 间隙的中点，线高 3px
    const boundaryCenter = target.position === 'before'
      ? snap.layoutTop - 1
      : snap.layoutTop + snap.layoutHeight + 1
    if (!dropIndicatorVisible.value) {
      // 从隐藏转为显示时直接落位，避免从上一次的旧位置滑入
      scheduleDropIndicatorSnap()
    }
    dropIndicatorY.value = boundaryCenter - 1.5
    dropIndicatorVisible.value = true
  }

  function scheduleDropIndicatorSnap() {
    if (dropIndicatorSnapFrame !== null) window.cancelAnimationFrame(dropIndicatorSnapFrame)
    dropIndicatorSnap.value = true
    dropIndicatorSnapFrame = window.requestAnimationFrame(() => {
      dropIndicatorSnapFrame = null
      dropIndicatorSnap.value = false
    })
  }

  function autoScrollWhileDragging(e: PointerEvent) {
    const scroller =
      options.getScrollContainer?.()
      || (options.listRef.value?.closest('.content') as HTMLElement | null)
    if (!scroller) return

    const rect = scroller.getBoundingClientRect()
    const edge = 72
    const maxStep = 28
    let delta = 0
    if (e.clientY < rect.top + edge) {
      const t = 1 - Math.max(0, e.clientY - rect.top) / edge
      delta = -Math.ceil(maxStep * t)
    } else if (e.clientY > rect.bottom - edge) {
      const t = 1 - Math.max(0, rect.bottom - e.clientY) / edge
      delta = Math.ceil(maxStep * t)
    }
    if (delta === 0) return

    const prev = scroller.scrollTop
    scroller.scrollTop = prev + delta
    const actual = scroller.scrollTop - prev
    if (actual === 0) return
    // 列表滚动后补偿拖拽起点，保持行跟随手指
    dragStartPointerY -= actual
    dragStartRowTop -= actual
  }

  function onDragPointerMove(e: PointerEvent) {
    if (dragPointerId !== e.pointerId) return
    e.preventDefault()
    autoScrollWhileDragging(e)
    // 滚动后重新采样行位置，保证目标插入点准确
    captureDragSnapshot()
    // 允许拖出当前视口，配合自动滚动完成跨屏排序
    dragOffsetY.value = e.clientY - dragStartPointerY
    updateDragUnderGlassKeys()
    updateDragTarget(e)
  }

  function cleanupDrag() {
    window.removeEventListener('pointermove', onDragPointerMove)
    window.removeEventListener('pointerup', onDragPointerUp)
    window.removeEventListener('pointercancel', cancelDrag)
    if (dragHandleElement && dragPointerId !== null) {
      if (dragHandleElement.hasPointerCapture?.(dragPointerId)) {
        dragHandleElement.releasePointerCapture(dragPointerId)
      }
    }
    dragHandleElement = null
    dragPointerId = null
    dragKey.value = null
    dragOverKey.value = null
    dragInsertPosition.value = null
    // 只翻可见标志、保留 dropIndicatorY，让指示线原地淡出而非瞬间消失
    dropIndicatorVisible.value = false
    if (dropIndicatorSnapFrame !== null) {
      window.cancelAnimationFrame(dropIndicatorSnapFrame)
      dropIndicatorSnapFrame = null
      dropIndicatorSnap.value = false
    }
    dragPrevSiblingKey = null
    dragNextSiblingKey = null
    cancelDragGlassActivation()
    dragGlassActive.value = false
    dragUnderGlassKeys.value = new Set()
    dragStartPointerY = 0
    dragStartRowTop = 0
    dragRowHeight = 0
    dragOffsetY.value = 0
    dragRowSnapshots = []
  }

  function cancelDrag() {
    startDragLandingState(dragKey.value)
    cleanupDrag()
  }

  function onDragPointerUp(e: PointerEvent) {
    if (dragPointerId !== e.pointerId) return
    e.preventDefault()
    const fromKey = dragKey.value
    const toKey = dragOverKey.value
    const insertPosition = dragInsertPosition.value
    startDragLandingState(fromKey)
    cleanupDrag()

    if (!fromKey || !toKey || !insertPosition || fromKey === toKey) return
    options.onReorder(fromKey, toKey, insertPosition)
  }

  function startDrag(e: PointerEvent, key: string) {
    if (options.canDrag && !options.canDrag()) return
    if (e.pointerType === 'mouse' && e.button !== 0) return
    e.preventDefault()
    clearDragLandingState()
    dragKey.value = key
    scheduleDragGlassActivation()
    dragPointerId = e.pointerId
    dragHandleElement = e.currentTarget as HTMLElement
    const row = dragHandleElement.closest(options.itemSelector) as HTMLElement | null
    const rowRect = row?.getBoundingClientRect()
    dragStartPointerY = e.clientY
    dragStartRowTop = rowRect?.top ?? e.clientY
    dragRowHeight = rowRect?.height ?? 0
    dragOffsetY.value = 0
    captureDragSnapshot()
    dragHandleElement.setPointerCapture?.(e.pointerId)
    updateDragTarget(e)
    window.addEventListener('pointermove', onDragPointerMove)
    window.addEventListener('pointerup', onDragPointerUp)
    window.addEventListener('pointercancel', cancelDrag)
  }

  onUnmounted(() => {
    cleanupDrag()
    clearDragLandingState()
  })

  return {
    dragKey,
    dragOverKey,
    dragInsertPosition,
    dragLandingKey,
    dragUnderGlassKeys,
    dragGlassActive,
    dropIndicatorVisible,
    dropIndicatorY,
    dropIndicatorSnap,
    dragItemStyle,
    startDrag,
    cancelDrag,
  }
}
