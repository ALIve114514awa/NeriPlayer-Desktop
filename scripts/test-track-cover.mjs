import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

const sourceUrl = new URL('../src/utils/trackCover.ts', import.meta.url)
const source = await readFile(sourceUrl, 'utf8')
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
const { getTrackCoverUrl } = await import(moduleUrl)

assert.equal(getTrackCoverUrl(null), '')
assert.equal(getTrackCoverUrl({ coverUrl: '  https://img.example/direct.jpg  ' }), 'https://img.example/direct.jpg')
assert.equal(
  getTrackCoverUrl({
    coverUrl: '',
    syncPayload: { custom_cover_url: 'https://img.example/custom.jpg' },
  }),
  'https://img.example/custom.jpg',
)
assert.equal(
  getTrackCoverUrl({
    syncPayload: { originalCoverUrl: 'https://img.example/original.jpg' },
  }),
  'https://img.example/original.jpg',
)
assert.equal(
  getTrackCoverUrl({
    coverUrl: 'https://img.example/direct.jpg',
    syncPayload: { customCoverUrl: 'https://img.example/custom.jpg' },
  }),
  'https://img.example/direct.jpg',
)

console.log('track cover tests passed')
