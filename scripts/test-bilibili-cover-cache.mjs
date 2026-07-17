import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

const sourceUrl = new URL('../src/utils/bilibiliCoverCache.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
const {
  BilibiliCoverCache,
  normalizeBilibiliCoverUrl,
  normalizeCoverUrlForDisplay,
  normalizeProxiedCoverUrl,
} = await import(moduleUrl)

const coverUrl = name => `https://i0.hdslb.com/bfs/archive/${name}.jpg`
const dataUrl = value => `data:image/png;base64,${Buffer.from(value).toString('base64')}`

async function run(name, test) {
  await test()
  console.log(`ok - ${name}`)
}

await run('canonicalizes only allowed HTTPS cover URLs', async () => {
  assert.equal(
    normalizeBilibiliCoverUrl('//I0.HDSLB.COM/bfs/archive/a.jpg#ignored'),
    coverUrl('a'),
  )
  assert.equal(
    normalizeBilibiliCoverUrl('http://archive.biliimg.com/a.webp?size=1'),
    'https://archive.biliimg.com/a.webp?size=1',
  )
  assert.equal(normalizeBilibiliCoverUrl('https://example.com/a.jpg'), '')
  assert.equal(normalizeBilibiliCoverUrl('https://user@i0.hdslb.com/a.jpg'), '')
  assert.equal(normalizeBilibiliCoverUrl('https://i0.hdslb.com:8443/a.jpg'), '')
  assert.equal(
    normalizeCoverUrlForDisplay('//y.qq.com/music/photo_new/T002R300x300M000.jpg'),
    'https://y.qq.com/music/photo_new/T002R300x300M000.jpg',
  )
  assert.equal(
    normalizeCoverUrlForDisplay('https://p4.music.126.net/cover.jpg'),
    'https://p4.music.126.net/cover.jpg',
  )
  assert.equal(
    normalizeProxiedCoverUrl('https://y.qq.com/music/photo_new/cover.jpg'),
    'https://y.qq.com/music/photo_new/cover.jpg',
  )
  assert.equal(
    normalizeProxiedCoverUrl('https://p4.music.126.net/cover.jpg'),
    'https://p4.music.126.net/cover.jpg',
  )
  assert.equal(
    normalizeProxiedCoverUrl('https://i.ytimg.com/vi/example/maxresdefault.jpg'),
    'https://i.ytimg.com/vi/example/maxresdefault.jpg',
  )
  assert.equal(normalizeProxiedCoverUrl('https://example.com/cover.jpg'), '')
})

await run('deduplicates canonical and concurrent requests', async () => {
  let fetchCount = 0
  let releaseFetch
  const cache = new BilibiliCoverCache(
    async () => {
      fetchCount++
      return new Promise(resolve => { releaseFetch = resolve })
    },
    async () => {},
  )

  const first = cache.resolve('//i0.hdslb.com/bfs/archive/shared.jpg#one')
  const second = cache.resolve(coverUrl('shared'))
  assert.equal(fetchCount, 1)
  releaseFetch(dataUrl('shared'))
  assert.equal(await first, dataUrl('shared'))
  assert.equal(await second, dataUrl('shared'))
  assert.equal(fetchCount, 1)
})

await run('does not cache fetch, validation, or decode failures', async () => {
  let fetchCount = 0
  let decodeCount = 0
  const responses = [
    new Error('network failed'),
    'data:text/html;base64,PGh0bWw+',
    dataUrl('decode-fails'),
    dataUrl('success'),
  ]
  const cache = new BilibiliCoverCache(
    async () => {
      const response = responses[fetchCount++]
      if (response instanceof Error) throw response
      return response
    },
    async value => {
      decodeCount++
      if (value === dataUrl('decode-fails')) throw new Error('decode failed')
    },
  )

  await assert.rejects(cache.resolve(coverUrl('failures')), /network failed/)
  await assert.rejects(cache.resolve(coverUrl('failures')), /valid image data URL/)
  await assert.rejects(cache.resolve(coverUrl('failures')), /decode failed/)
  assert.equal(await cache.resolve(coverUrl('failures')), dataUrl('success'))
  assert.equal(await cache.resolve(coverUrl('failures')), dataUrl('success'))
  assert.equal(fetchCount, 4)
  assert.equal(decodeCount, 2)
})

await run('uses LRU eviction and keeps query variants isolated', async () => {
  const fetchCounts = new Map()
  const cache = new BilibiliCoverCache(
    async url => {
      fetchCounts.set(url, (fetchCounts.get(url) ?? 0) + 1)
      return dataUrl(url)
    },
    async () => {},
    { maxEntries: 2 },
  )

  await cache.resolve(coverUrl('a'))
  await cache.resolve(coverUrl('b'))
  await cache.resolve(coverUrl('a'))
  await cache.resolve(coverUrl('c'))
  await cache.resolve(coverUrl('b'))
  await cache.resolve(`${coverUrl('a')}?size=2`)

  assert.equal(fetchCounts.get(coverUrl('a')), 1)
  assert.equal(fetchCounts.get(coverUrl('b')), 2)
  assert.equal(fetchCounts.get(`${coverUrl('a')}?size=2`), 1)
})

await run('expires entries and skips entries above the data URL budget', async () => {
  let now = 1_000
  let fetchCount = 0
  const value = dataUrl('budget')
  const cache = new BilibiliCoverCache(
    async () => {
      fetchCount++
      return value
    },
    async () => {},
    { ttlMs: 100, maxDataUrlChars: value.length - 1, now: () => now },
  )

  await cache.resolve(coverUrl('budget'))
  await cache.resolve(coverUrl('budget'))
  assert.equal(fetchCount, 2)

  const expiringCache = new BilibiliCoverCache(
    async () => {
      fetchCount++
      return dataUrl(`ttl-${fetchCount}`)
    },
    async () => {},
    { ttlMs: 100, now: () => now },
  )
  const first = await expiringCache.resolve(coverUrl('ttl'))
  now += 99
  assert.equal(await expiringCache.resolve(coverUrl('ttl')), first)
  now += 1
  assert.notEqual(await expiringCache.resolve(coverUrl('ttl')), first)
})

await run('prevents superseded requests from overwriting a refresh', async () => {
  const resolvers = []
  let fetchCount = 0
  const cache = new BilibiliCoverCache(
    async () => {
      fetchCount++
      return new Promise(resolve => resolvers.push(resolve))
    },
    async () => {},
  )

  const staleRequest = cache.resolve(coverUrl('race'))
  const freshRequest = cache.resolve(coverUrl('race'), { forceRefresh: true })
  resolvers[1](dataUrl('fresh'))
  assert.equal(await freshRequest, dataUrl('fresh'))
  resolvers[0](dataUrl('stale'))
  assert.equal(await staleRequest, dataUrl('stale'))
  assert.equal(await cache.resolve(coverUrl('race')), dataUrl('fresh'))
  assert.equal(fetchCount, 2)
})

await run('forwards force refresh to the persistent backend cache', async () => {
  const refreshFlags = []
  const cache = new BilibiliCoverCache(
    async (_url, forceRefresh) => {
      refreshFlags.push(forceRefresh)
      return dataUrl(`refresh-${refreshFlags.length}`)
    },
    async () => {},
  )

  await cache.resolve(coverUrl('refresh'))
  await cache.resolve(coverUrl('refresh'), { forceRefresh: true })

  assert.deepEqual(refreshFlags, [false, true])
})
