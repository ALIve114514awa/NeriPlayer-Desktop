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
const { getTrackCoverUrl, upgradeYouTubeThumbnailUrl } = await import(moduleUrl)

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

// YouTube 缩略图尺寸升级（对齐 Android upgradeYouTubeThumbnailUrl）
assert.equal(
  upgradeYouTubeThumbnailUrl('https://lh3.googleusercontent.com/abc123=w226-h226-l90-rj'),
  'https://lh3.googleusercontent.com/abc123=w1200-h1200',
)
assert.equal(
  upgradeYouTubeThumbnailUrl('https://lh3.googleusercontent.com/abc123=w60-h60-l90-rj'),
  'https://lh3.googleusercontent.com/abc123=w1200-h1200',
)
assert.equal(
  upgradeYouTubeThumbnailUrl('https://lh3.googleusercontent.com/abc123'),
  'https://lh3.googleusercontent.com/abc123=w1200-h1200',
)
assert.equal(
  upgradeYouTubeThumbnailUrl('https://yt3.ggpht.com/abc123=w120-h120'),
  'https://yt3.ggpht.com/abc123=w1200-h1200',
)
assert.equal(
  upgradeYouTubeThumbnailUrl('https://yt3.ggpht.com/avatar=s88'),
  'https://yt3.ggpht.com/avatar=w1200-h1200',
)
assert.equal(
  upgradeYouTubeThumbnailUrl('https://i.ytimg.com/vi/abc123/maxresdefault.jpg'),
  'https://i.ytimg.com/vi/abc123/maxresdefault.jpg',
)
assert.equal(upgradeYouTubeThumbnailUrl(''), '')
assert.equal(upgradeYouTubeThumbnailUrl('  '), '  ')

// getTrackCoverUrl 应对 YouTube 封面自动升清
assert.equal(
  getTrackCoverUrl({
    coverUrl: 'https://lh3.googleusercontent.com/cover=w60-h60-l90-rj',
  }),
  'https://lh3.googleusercontent.com/cover=w1200-h1200',
)

console.log('track cover tests passed')
