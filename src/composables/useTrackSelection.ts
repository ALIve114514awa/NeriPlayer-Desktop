import { computed, ref, type ComputedRef, type Ref } from 'vue'

export function useTrackSelection<T extends { id: string }>(
  allItems: Ref<T[]>,
  visibleItems: ComputedRef<T[]> | Ref<T[]>,
) {
  const selectionMode = ref(false)
  const selectedIds = ref<Set<string>>(new Set())
  const selectedItems = computed(() => allItems.value.filter(item => selectedIds.value.has(item.id)))
  const visibleSelectedCount = computed(() => visibleItems.value.filter(item => selectedIds.value.has(item.id)).length)
  const allVisibleSelected = computed(() => visibleItems.value.length > 0 && visibleSelectedCount.value === visibleItems.value.length)

  function enterSelectionMode(item?: T) {
    selectionMode.value = true
    if (item) {
      selectedIds.value = new Set(selectedIds.value).add(item.id)
    }
  }

  function leaveSelectionMode() {
    selectionMode.value = false
    selectedIds.value = new Set()
  }

  function toggleSelected(id: string) {
    const next = new Set(selectedIds.value)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    selectedIds.value = next
    if (selectionMode.value && next.size === 0) selectionMode.value = false
  }

  function toggleSelectAllVisible() {
    const next = new Set(selectedIds.value)
    if (allVisibleSelected.value) {
      for (const item of visibleItems.value) next.delete(item.id)
    } else {
      for (const item of visibleItems.value) next.add(item.id)
    }
    selectedIds.value = next
    if (next.size > 0) selectionMode.value = true
    else selectionMode.value = false
  }

  function pruneSelection() {
    const validIds = new Set(allItems.value.map(item => item.id))
    selectedIds.value = new Set([...selectedIds.value].filter(id => validIds.has(id)))
    if (selectionMode.value && selectedIds.value.size === 0) selectionMode.value = false
  }

  return {
    selectionMode,
    selectedIds,
    selectedItems,
    visibleSelectedCount,
    allVisibleSelected,
    enterSelectionMode,
    leaveSelectionMode,
    toggleSelected,
    toggleSelectAllVisible,
    pruneSelection,
  }
}
