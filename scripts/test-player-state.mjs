import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

const sourceUrl = new URL('../src/utils/playerState.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
  fileName: pathToFileURL(sourceUrl.pathname).href,
})
const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled.outputText).toString('base64')}`
const {
  buildPersistedPlaybackQueue,
  restorePersistedPlaybackQueue,
} = await import(moduleUrl)

const first = { id: 'netease:1', playlistKey: 'playlist:first', audioUrl: '' }
const current = { id: 'netease:1', playlistKey: 'playlist:current', audioUrl: '' }
const last = { id: 'netease:2', playlistKey: 'playlist:last', audioUrl: '' }

const full = buildPersistedPlaybackQueue([first, current, last], 1, current, false)
assert.deepEqual(full, {
  queue: [first, current, last],
  queueIndex: 1,
  hasPlaybackSession: true,
  currentTrackId: 'netease:1',
  currentTrackPlaylistKey: 'playlist:current',
})

const compact = buildPersistedPlaybackQueue([first, current, last], 1, current, true)
assert.deepEqual(compact, {
  queue: [current],
  queueIndex: 0,
  hasPlaybackSession: true,
  currentTrackId: 'netease:1',
  currentTrackPlaylistKey: 'playlist:current',
})

const normalize = raw => {
  if (!raw || typeof raw !== 'object') throw new Error('invalid track')
  return raw
}
const canRestore = track => !!track.id && (!!track.audioUrl || !track.id.startsWith('local:'))

const restored = restorePersistedPlaybackQueue(
  [null, { id: 'local:missing', audioUrl: '' }, first, current, last],
  3,
  true,
  'netease:1',
  'playlist:current',
  normalize,
  canRestore,
)
assert.deepEqual(restored.queue, [first, current, last])
assert.equal(restored.queueIndex, 1)
assert.equal(restored.currentTrack, current)
assert.equal(restored.hasPlaybackSession, true)

const legacyRestored = restorePersistedPlaybackQueue(
  [first, last],
  99.8,
  undefined,
  undefined,
  undefined,
  normalize,
  canRestore,
)
assert.equal(legacyRestored.queueIndex, 1)
assert.equal(legacyRestored.currentTrack, last)
assert.equal(legacyRestored.hasPlaybackSession, true)

const empty = restorePersistedPlaybackQueue(
  [{ id: 'local:missing', audioUrl: '' }],
  0,
  true,
  'local:missing',
  undefined,
  normalize,
  canRestore,
)
assert.deepEqual(empty.queue, [])
assert.equal(empty.queueIndex, -1)
assert.equal(empty.currentTrack, null)
assert.equal(empty.hasPlaybackSession, false)

const queuedWithoutSession = buildPersistedPlaybackQueue([first, last], -1, null, false)
assert.equal(queuedWithoutSession.queueIndex, -1)
assert.equal(queuedWithoutSession.hasPlaybackSession, false)
const restoredWithoutSession = restorePersistedPlaybackQueue(
  queuedWithoutSession.queue,
  queuedWithoutSession.queueIndex,
  queuedWithoutSession.hasPlaybackSession,
  queuedWithoutSession.currentTrackId,
  queuedWithoutSession.currentTrackPlaylistKey,
  normalize,
  canRestore,
)
assert.deepEqual(restoredWithoutSession.queue, [first, last])
assert.equal(restoredWithoutSession.queueIndex, -1)
assert.equal(restoredWithoutSession.currentTrack, null)
assert.equal(restoredWithoutSession.hasPlaybackSession, false)

console.log('player state tests passed')
