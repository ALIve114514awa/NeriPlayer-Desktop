import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { TrackInfo } from './player'

export interface PlayedEntry {
  track: TrackInfo
  playedAt: number
}

const STORAGE_KEY = 'neri:play-history'
const MAX_ENTRIES = 1000
export const HISTORY_CHANGED_EVENT = 'neri:history-changed'

export const useHistoryStore = defineStore('history', () => {
  const entries = ref<PlayedEntry[]>([])

  // 启动时从 localStorage 恢复
  function load() {
    try {
      const raw = localStorage.getItem(STORAGE_KEY)
      if (raw) entries.value = JSON.parse(raw)
    } catch {
      entries.value = []
    }
  }

  function save() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(entries.value))
    } catch {
      // 存储失败忽略
    }
  }

  function emitHistoryChanged(type: 'record' | 'remove' | 'clear') {
    if (typeof window === 'undefined') return
    window.dispatchEvent(new CustomEvent(HISTORY_CHANGED_EVENT, {
      detail: { type, at: Date.now() },
    }))
  }

  // 记录播放（去重：同 ID 更新时间戳，移到最前）
  function record(track: TrackInfo) {
    const idx = entries.value.findIndex(e => e.track.id === track.id)
    if (idx >= 0) entries.value.splice(idx, 1)
    entries.value.unshift({ track, playedAt: Date.now() })
    if (entries.value.length > MAX_ENTRIES) {
      entries.value = entries.value.slice(0, MAX_ENTRIES)
    }
    save()
    emitHistoryChanged('record')
  }

  // 删除单条
  function remove(trackId: string) {
    const before = entries.value.length
    entries.value = entries.value.filter(e => e.track.id !== trackId)
    save()
    if (entries.value.length !== before) emitHistoryChanged('remove')
  }

  // 清空全部
  function clear() {
    if (entries.value.length === 0) return
    entries.value = []
    save()
    emitHistoryChanged('clear')
  }

  // 初始化加载
  load()

  return { entries, record, remove, clear }
})
