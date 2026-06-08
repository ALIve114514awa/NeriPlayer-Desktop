import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface SearchResult {
  id: string
  title: string
  artist: string
  album: string
  duration_ms: number
  source: string
  cover_url: string | null
  synced_lyrics?: string | null
  plain_lyrics?: string | null
  translated_lyrics?: string | null
}

export const useSearchStore = defineStore('search', () => {
  const results = ref<SearchResult[]>([])
  const isSearching = ref(false)
  const query = ref('')
  const platform = ref('all') // all | netease | qq | bilibili | youtube

  async function search(q: string, p?: string) {
    if (!q.trim()) {
      results.value = []
      return
    }

    query.value = q
    if (p) platform.value = p
    isSearching.value = true

    try {
      const r = await invoke<SearchResult[]>('search', {
        query: q,
        platform: platform.value,
      })
      results.value = r
    } catch (e) {
      console.error('Search failed:', e)
      results.value = []
    } finally {
      isSearching.value = false
    }
  }

  function clear() {
    results.value = []
    query.value = ''
  }

  return { results, isSearching, query, platform, search, clear }
})
