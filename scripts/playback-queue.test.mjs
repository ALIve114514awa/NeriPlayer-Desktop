import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

const sourceUrl = new URL('../src/utils/playbackQueue.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
  fileName: pathToFileURL(sourceUrl.pathname).href,
})
const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled.outputText).toString('base64')}`
const { resolvePlaybackQueueStartIndex } = await import(moduleUrl)

const tracks = [
  { id: 'first', playlistKey: 'membership:first' },
  { id: 'second', playlistKey: 'membership:second-a' },
  { id: 'second', playlistKey: 'membership:second-b' },
  { id: 'third', playlistKey: 'membership:third' },
]

assert.equal(resolvePlaybackQueueStartIndex([], 'second'), -1)
assert.equal(resolvePlaybackQueueStartIndex(tracks), 0)
assert.equal(resolvePlaybackQueueStartIndex(tracks, 'third'), 3)
assert.equal(resolvePlaybackQueueStartIndex(tracks, 'missing'), 0)
assert.equal(resolvePlaybackQueueStartIndex(tracks, '  second  '), 1)
assert.equal(
  resolvePlaybackQueueStartIndex(tracks, 'second', 'membership:second-b'),
  2,
)
assert.equal(
  resolvePlaybackQueueStartIndex(tracks, 'second', 'missing-membership'),
  1,
)

console.log('playback queue tests passed')
