import { defineStore } from 'pinia'
import { ref } from 'vue'
import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import type { TrackInfo } from './player'
import { useSettingsStore } from './settings'
import { createLogger } from '@/utils/logger'

const log = createLogger('library')

export const useLibraryStore = defineStore('library', () => {
  const tracks = ref<TrackInfo[]>([])
  const isScanning = ref(false)
  const scanError = ref<string | null>(null)
  const scanSkipped = ref<{ path: string; reason: string }[]>([])
  const lastScanDir = ref<string | null>(null)

  async function scanDirectory(dir: string) {
    isScanning.value = true
    scanError.value = null
    scanSkipped.value = []

    try {
      const settings = useSettingsStore()
      const results = await invoke<{ tracks: any[]; skipped: { path: string; reason: string }[] }>('scan_music_directory', {
        dir,
        nameTemplate: settings.downloadNameTemplate || null,
      })
      scanSkipped.value = results.skipped || []

      // 后端返回的字段名是 snake_case，映射到前端 camelCase
      tracks.value = (results.tracks || []).map(t => ({
        id: t.id,
        title: t.title,
        artist: t.artist,
        album: t.album,
        durationMs: t.duration_ms,
        coverUrl: toDisplayableCoverUrl(t.cover_url),
        audioUrl: t.url,
      }))

      lastScanDir.value = dir
      // 持久化扫描路径
      localStorage.setItem('neri:last_scan_dir', dir)
    } catch (e: any) {
      scanError.value = String(e)
      log.error('Scan failed:', e)
    } finally {
      isScanning.value = false
    }
  }

  function toDisplayableCoverUrl(value?: string | null) {
    if (!value) return ''
    if (/^(https?:|asset:|data:|blob:)/i.test(value)) return value
    return convertFileSrc(value)
  }

  // 启动时恢复上次扫描路径
  function restoreLastScan() {
    const dir = localStorage.getItem('neri:last_scan_dir')
    if (dir) scanDirectory(dir)
  }

  return {
    tracks, isScanning, scanError, scanSkipped, lastScanDir,
    scanDirectory, restoreLastScan,
  }
})
