import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

const sourceUrl = new URL('../src/modules/lyrics/lyricsRequest.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
const {
  hasLyricsRequestInFlight,
  lyricsIdentity,
  loadLyricsSingleFlight,
} = await import(moduleUrl)

const track = {
  id: 'netease:642681',
  title: 'Gips',
  artist: 'Sheena Ringo',
  durationMs: 328_000,
}

assert.equal(lyricsIdentity(track), track.id)
assert.equal(lyricsIdentity({ ...track, id: '' }), 'Gips::Sheena Ringo::328000')

let calls = 0
let resolveRequest
const loader = () => {
  calls += 1
  return new Promise(resolve => { resolveRequest = resolve })
}
const first = loadLyricsSingleFlight(track, loader)
const second = loadLyricsSingleFlight(track, loader)
assert.strictEqual(first, second)
assert.equal(calls, 1)
assert.equal(hasLyricsRequestInFlight(track), true)
resolveRequest([{ startMs: 0, durationMs: 1_000, words: [], text: 'line' }])
assert.deepEqual(await first, await second)

await Promise.resolve()
assert.equal(hasLyricsRequestInFlight(track), false)
await loadLyricsSingleFlight(track, async () => {
  calls += 1
  return []
})
assert.equal(calls, 2)

await assert.rejects(
  loadLyricsSingleFlight({ ...track, id: 'netease:failed' }, async () => {
    throw new Error('temporary failure')
  }),
  /temporary failure/,
)
await loadLyricsSingleFlight({ ...track, id: 'netease:failed' }, async () => [])

const nowPlayingSource = await readFile(
  new URL('../src/components/NowPlaying.vue', import.meta.url),
  'utf8',
)
assert.match(
  nowPlayingSource,
  /await loadLyricsSingleFlight\(track/,
  'NowPlaying must share an in-flight lyric request across component remounts',
)
assert.match(
  nowPlayingSource,
  /readCachedLyrics\(track\) \|\| cachedLyrics \|\| \[\]/,
  'a failed refresh must restore the latest cached lyrics',
)

console.log('lyrics request tests passed')
