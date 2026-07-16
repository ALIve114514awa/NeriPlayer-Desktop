interface CacheEntry<T> {
  value: T
  updatedAt: number
}

interface CacheOptions {
  maxAgeMs: number
  maxEntries: number
}

type CacheBucket<T> = Record<string, CacheEntry<T>>

function readBucket<T>(bucketKey: string): CacheBucket<T> {
  try {
    const raw = localStorage.getItem(bucketKey)
    if (!raw) return {}
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? parsed : {}
  } catch {
    return {}
  }
}

function pruneBucket<T>(
  bucket: CacheBucket<T>,
  maxAgeMs: number,
  maxEntries: number,
): CacheBucket<T> {
  const now = Date.now()
  const entries = Object.entries(bucket)
    .filter(([, entry]) => entry && now - entry.updatedAt <= maxAgeMs)
    .sort((a, b) => b[1].updatedAt - a[1].updatedAt)
    .slice(0, maxEntries)

  return Object.fromEntries(entries) as CacheBucket<T>
}

function writeBucket<T>(bucketKey: string, bucket: CacheBucket<T>) {
  localStorage.setItem(bucketKey, JSON.stringify(bucket))
}

export function getCachedValue<T>(
  bucketKey: string,
  key: string,
  maxAgeMs: number,
): T | null {
  const bucket = readBucket<T>(bucketKey)
  const entry = bucket[key]
  if (!entry) return null
  if (Date.now() - entry.updatedAt > maxAgeMs) {
    delete bucket[key]
    try {
      writeBucket(bucketKey, bucket)
    } catch {}
    return null
  }
  return entry.value
}

export function setCachedValue<T>(
  bucketKey: string,
  key: string,
  value: T,
  options: CacheOptions,
) {
  const bucket = readBucket<T>(bucketKey)
  bucket[key] = { value, updatedAt: Date.now() }
  const pruned = pruneBucket(bucket, options.maxAgeMs, options.maxEntries)

  try {
    writeBucket(bucketKey, pruned)
  } catch {
    const compact = Object.fromEntries(
      Object.entries(pruned).slice(0, Math.max(1, Math.floor(options.maxEntries / 2))),
    ) as CacheBucket<T>
    try {
      writeBucket(bucketKey, compact)
    } catch {}
  }
}

export function removeCachedValue(bucketKey: string, key: string) {
  const bucket = readBucket<unknown>(bucketKey)
  if (!(key in bucket)) return
  delete bucket[key]
  try {
    writeBucket(bucketKey, bucket)
  } catch {}
}
