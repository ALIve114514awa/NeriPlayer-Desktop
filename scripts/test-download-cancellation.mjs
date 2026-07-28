import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { pathToFileURL } from 'node:url'
import ts from 'typescript'

const sourceUrl = new URL(
  '../src/modules/download/downloadCancellation.ts',
  import.meta.url,
)
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
  consumeResolvingCancellation,
  markResolvingTasksCancelled,
} = await import(moduleUrl)

const cancelled = new Set()
const resolvingIds = markResolvingTasksCancelled(
  [
    { trackId: 'resolving-1', status: 'resolving' },
    { trackId: 'downloading-1', status: 'downloading' },
    { trackId: 'resolving-2', status: 'resolving' },
  ],
  cancelled,
  trackId => ({
    'resolving-1': 'token-1',
    'resolving-2': 'token-2',
  })[trackId],
)
assert.deepEqual(resolvingIds, [
  { trackId: 'resolving-1', token: 'token-1' },
  { trackId: 'resolving-2', token: 'token-2' },
])
assert.deepEqual([...cancelled], ['token-1', 'token-2'])
assert.equal(consumeResolvingCancellation(cancelled, 'token-1'), true)
assert.equal(consumeResolvingCancellation(cancelled, 'token-1'), false)
assert.equal(consumeResolvingCancellation(cancelled, 'downloading-1'), false)
assert.deepEqual([...cancelled], ['token-2'])

console.log('download cancellation tests passed')
