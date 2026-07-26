// localStorage 缓存桶：按「时间 + 条数 + 字节」三重裁剪
//
// 只限条数是不够的。歌单详情一条就能到几百 KB（一条曲目约 232 字节，
// 一个 500 曲的歌单就是 116 KB），80 条能堆到 8 MB 以上，
// 而 localStorage 通常只有 5 MB。超了会抛 QuotaExceededError，
// 之前的处理是「减半重试一次，再失败就 catch {} 」——
// 缓存从此永久失效，而且没有任何痕迹。

interface CacheEntry<T> {
  value: T
  updatedAt: number
}

interface CacheOptions {
  maxAgeMs: number
  maxEntries: number
  /// 整桶序列化后的字节上限；不传则用默认值
  maxBytes?: number
}

type CacheBucket<T> = Record<string, CacheEntry<T>>

/// 单桶默认字节上限
///
/// localStorage 配额是整个源共享的，各桶都吃满就会互相挤兑。
/// 1.5 MB 给歌单详情足够放下几十个常看的歌单，也给其它桶留了余量。
const DEFAULT_MAX_BYTES = 1_536 * 1024

/// 配额超限后逐级缩减的比例，直到写得进去为止
const SHRINK_STEPS = [0.5, 0.25, 0.1] as const

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

/// 新鲜的排在前面，裁剪一律从尾部砍
function sortedEntries<T>(
  bucket: CacheBucket<T>,
  maxAgeMs: number,
): Array<[string, CacheEntry<T>]> {
  const now = Date.now()
  return Object.entries(bucket)
    .filter(([, entry]) => entry && now - entry.updatedAt <= maxAgeMs)
    .sort((a, b) => b[1].updatedAt - a[1].updatedAt)
}

/// 按字节预算保留最新的若干条
///
/// 逐条累加实际序列化长度，而不是估算：不同来源的曲目体积能差一个数量级
/// （携带签名 URL 的能到 1 KB 以上），估算会系统性低估。
function fitToByteBudget<T>(
  entries: Array<[string, CacheEntry<T>]>,
  maxBytes: number,
): Array<[string, CacheEntry<T>]> {
  const kept: Array<[string, CacheEntry<T>]> = []
  // 外层 {} 与条目间的逗号
  let used = 2
  for (const entry of entries) {
    const size = JSON.stringify({ [entry[0]]: entry[1] }).length
    if (kept.length > 0 && used + size > maxBytes) break
    used += size
    kept.push(entry)
  }
  return kept
}

function writeBucket<T>(bucketKey: string, bucket: CacheBucket<T>) {
  localStorage.setItem(bucketKey, JSON.stringify(bucket))
}

/// 写入并在配额超限时逐级缩减
///
/// 缩减到最后一级仍写不进去，就把这个桶清掉：留着一份写不进也删不掉的
/// 旧数据，只会让后续每次写入都白跑一遍序列化。
function writeWithQuotaFallback<T>(
  bucketKey: string,
  entries: Array<[string, CacheEntry<T>]>,
) {
  try {
    writeBucket(bucketKey, Object.fromEntries(entries) as CacheBucket<T>)
    return
  } catch {
    // 落到下面逐级缩减
  }

  for (const ratio of SHRINK_STEPS) {
    const size = Math.max(1, Math.floor(entries.length * ratio))
    try {
      writeBucket(bucketKey, Object.fromEntries(entries.slice(0, size)) as CacheBucket<T>)
      console.warn(`[cache] ${bucketKey} 配额超限，已缩减到 ${size}/${entries.length} 条`)
      return
    } catch {
      continue
    }
  }

  try {
    localStorage.removeItem(bucketKey)
    console.warn(`[cache] ${bucketKey} 配额超限且无法缩减，已清空该桶`)
  } catch (error) {
    console.warn(`[cache] ${bucketKey} 清理失败:`, error)
  }
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
    } catch {
      // 过期项清理失败不影响返回值，下次写入时还会再裁一遍
    }
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

  const byAge = sortedEntries(bucket, options.maxAgeMs).slice(0, options.maxEntries)
  const fitted = fitToByteBudget(byAge, options.maxBytes ?? DEFAULT_MAX_BYTES)
  if (fitted.length < byAge.length) {
    console.warn(
      `[cache] ${bucketKey} 字节预算触顶，丢弃 ${byAge.length - fitted.length} 条最旧记录`,
    )
  }
  writeWithQuotaFallback(bucketKey, fitted)
}

export function removeCachedValue(bucketKey: string, key: string) {
  const bucket = readBucket<unknown>(bucketKey)
  if (!(key in bucket)) return
  delete bucket[key]
  try {
    writeBucket(bucketKey, bucket)
  } catch {
    // 删除后体积只会更小，写失败说明整个 localStorage 已不可用，无需再兜底
  }
}

/// 供测试与诊断：当前桶占用的字节数
export function cachedBucketBytes(bucketKey: string): number {
  try {
    return localStorage.getItem(bucketKey)?.length ?? 0
  } catch {
    return 0
  }
}
