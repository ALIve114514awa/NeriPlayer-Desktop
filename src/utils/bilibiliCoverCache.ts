const BILIBILI_IMAGE_DOMAINS = ['hdslb.com', 'biliimg.com'] as const
const PROXIED_COVER_IMAGE_DOMAINS = [
  ...BILIBILI_IMAGE_DOMAINS,
  'y.qq.com',
  'qqmusic.qq.com',
  'y.gtimg.cn',
  'music.126.net',
  'ytimg.com',
  'ggpht.com',
  'googleusercontent.com',
] as const
const DATA_URL_PATTERN = /^data:(image\/(?:avif|gif|jpe?g|png|webp));base64,([a-z0-9+/]+={0,2})$/i

const DEFAULT_MAX_ENTRIES = 24
const DEFAULT_MAX_DATA_URL_CHARS = 24 * 1024 * 1024
const DEFAULT_TTL_MS = 30 * 60 * 1000

interface CoverCacheEntry {
  dataUrl: string
  expiresAt: number
}

interface PendingCoverRequest {
  generation: number
  promise: Promise<string>
}

export interface BilibiliCoverCacheOptions {
  maxEntries?: number
  maxDataUrlChars?: number
  ttlMs?: number
  now?: () => number
}

export interface ResolveBilibiliCoverOptions {
  forceRefresh?: boolean
}

export function normalizeCoverUrlForDisplay(rawUrl: string): string {
  const trimmed = rawUrl.trim()
  if (!trimmed) return ''
  if (trimmed.startsWith('//')) return `https:${trimmed}`
  return trimmed
}

export function normalizeBilibiliCoverUrl(rawUrl: string): string {
  const normalized = normalizeProxiedCoverUrl(rawUrl)
  if (!normalized) return ''
  const url = new URL(normalized)
  return isAllowedImageHost(url.hostname, BILIBILI_IMAGE_DOMAINS) ? normalized : ''
}

export function normalizeProxiedCoverUrl(rawUrl: string): string {
  const trimmed = normalizeCoverUrlForDisplay(rawUrl)
  if (!trimmed) return ''

  try {
    const url = new URL(trimmed)
    if (url.protocol === 'http:') url.protocol = 'https:'
    if (url.protocol !== 'https:' || url.username || url.password || url.port) return ''
    if (!isAllowedImageHost(url.hostname, PROXIED_COVER_IMAGE_DOMAINS)) return ''

    url.hash = ''
    return url.toString()
  } catch {
    return ''
  }
}

export function validateBilibiliCoverDataUrl(dataUrl: string): void {
  const match = DATA_URL_PATTERN.exec(dataUrl)
  if (!match || match[2].length === 0 || match[2].length % 4 === 1) {
    throw new Error('Cover response is not a valid image data URL')
  }
}

export class BilibiliCoverCache {
  private readonly cache = new Map<string, CoverCacheEntry>()
  private readonly pending = new Map<string, PendingCoverRequest>()
  private readonly generations = new Map<string, number>()
  private readonly maxEntries: number
  private readonly maxDataUrlChars: number
  private readonly ttlMs: number
  private readonly now: () => number
  private cachedDataUrlChars = 0

  constructor(
    private readonly fetchCover: (url: string, forceRefresh: boolean) => Promise<string>,
    private readonly decodeCover: (dataUrl: string) => Promise<void>,
    options: BilibiliCoverCacheOptions = {},
  ) {
    this.maxEntries = Math.max(1, options.maxEntries ?? DEFAULT_MAX_ENTRIES)
    this.maxDataUrlChars = Math.max(1, options.maxDataUrlChars ?? DEFAULT_MAX_DATA_URL_CHARS)
    this.ttlMs = Math.max(1, options.ttlMs ?? DEFAULT_TTL_MS)
    this.now = options.now ?? Date.now
  }

  async resolve(
    rawUrl: string,
    options: ResolveBilibiliCoverOptions = {},
  ): Promise<string> {
    const key = normalizeProxiedCoverUrl(rawUrl)
    if (!key) throw new Error('Cover URL is invalid')

    if (options.forceRefresh) this.invalidateKey(key)
    const generation = this.generationFor(key)

    if (!options.forceRefresh) {
      const cached = this.readCached(key)
      if (cached) return cached

      const pending = this.pending.get(key)
      if (pending?.generation === generation) return pending.promise
    }

    const promise = this.fetchCover(key, options.forceRefresh === true)
      .then(async (dataUrl) => {
        validateBilibiliCoverDataUrl(dataUrl)
        await this.decodeCover(dataUrl)
        if (this.generationFor(key) === generation) this.writeCached(key, dataUrl)
        return dataUrl
      })

    const pendingRequest = { generation, promise }
    this.pending.set(key, pendingRequest)
    void promise.then(
      () => this.clearPending(key, pendingRequest),
      () => this.clearPending(key, pendingRequest),
    )
    return promise
  }

  peek(rawUrl: string): string {
    const key = normalizeProxiedCoverUrl(rawUrl)
    if (!key) return ''
    return this.readCached(key) ?? ''
  }

  private readCached(key: string): string | null {
    const entry = this.cache.get(key)
    if (!entry) return null
    if (entry.expiresAt <= this.now()) {
      this.deleteCached(key)
      return null
    }

    this.cache.delete(key)
    this.cache.set(key, entry)
    return entry.dataUrl
  }

  private writeCached(key: string, dataUrl: string): void {
    if (dataUrl.length > this.maxDataUrlChars) return

    this.deleteCached(key)
    this.pruneExpired()
    while (
      this.cache.size >= this.maxEntries
      || this.cachedDataUrlChars + dataUrl.length > this.maxDataUrlChars
    ) {
      const oldestKey = this.cache.keys().next().value
      if (typeof oldestKey !== 'string') break
      this.deleteCached(oldestKey)
    }

    this.cache.set(key, {
      dataUrl,
      expiresAt: this.now() + this.ttlMs,
    })
    this.cachedDataUrlChars += dataUrl.length
  }

  private pruneExpired(): void {
    const now = this.now()
    for (const [key, entry] of this.cache) {
      if (entry.expiresAt <= now) this.deleteCached(key)
    }
  }

  private deleteCached(key: string): void {
    const cached = this.cache.get(key)
    if (!cached) return
    this.cachedDataUrlChars -= cached.dataUrl.length
    this.cache.delete(key)
  }

  private invalidateKey(key: string): void {
    this.deleteCached(key)
    this.pending.delete(key)
    this.generations.set(key, this.generationFor(key) + 1)
  }

  private generationFor(key: string): number {
    return this.generations.get(key) ?? 0
  }

  private clearPending(key: string, request: PendingCoverRequest): void {
    if (this.pending.get(key) === request) this.pending.delete(key)
  }
}

function isAllowedImageHost(
  host: string,
  domains: readonly string[],
): boolean {
  const normalizedHost = host.toLowerCase()
  return domains.some(
    domain => normalizedHost === domain || normalizedHost.endsWith(`.${domain}`),
  )
}
