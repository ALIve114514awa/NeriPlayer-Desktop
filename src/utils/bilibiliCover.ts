import { invoke } from '@tauri-apps/api/core'

const resolvedCoverCache = new Map<string, string>()
const pendingCoverRequests = new Map<string, Promise<string>>()
const MAX_CACHED_COVERS = 32

export function normalizeBilibiliCoverUrl(rawUrl: string): string {
  const trimmed = rawUrl.trim()
  if (trimmed.startsWith('//')) return `https:${trimmed}`
  if (trimmed.startsWith('http://')) return `https://${trimmed.slice('http://'.length)}`
  return trimmed
}

export async function resolveBilibiliCover(rawUrl: string): Promise<string> {
  const url = normalizeBilibiliCoverUrl(rawUrl)
  if (!url) throw new Error('Bilibili cover URL is empty')

  const cached = resolvedCoverCache.get(url)
  if (cached) return cached

  const pending = pendingCoverRequests.get(url)
  if (pending) return pending

  const request = invoke<string>('fetch_bilibili_cover', { url })
    .then((dataUrl) => {
      if (resolvedCoverCache.size >= MAX_CACHED_COVERS) {
        const oldestUrl = resolvedCoverCache.keys().next().value
        if (oldestUrl) resolvedCoverCache.delete(oldestUrl)
      }
      resolvedCoverCache.set(url, dataUrl)
      return dataUrl
    })
    .finally(() => pendingCoverRequests.delete(url))

  pendingCoverRequests.set(url, request)
  return request
}
