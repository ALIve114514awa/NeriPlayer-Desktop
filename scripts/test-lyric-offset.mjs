import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

async function loadTsModule(relativePath) {
  const sourceUrl = new URL(relativePath, import.meta.url)
  const source = await readFile(sourceUrl, 'utf8')
  const compiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ES2022,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText
  const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
  return import(moduleUrl)
}

const {
  DEFAULT_CLOUD_LYRIC_OFFSET_MS,
  DEFAULT_QQ_LYRIC_OFFSET_MS,
  offsetBucketForSource,
  resolveLyricDefaultOffsetMs,
  rebaseLyricUserOffsetMs,
  shouldRebaseLyricOffset,
  clampLyricOffsetMs,
  lyricUserOffsetStorageKey,
  readSyncedUserOffsetMs,
} = await loadTsModule('../src/modules/lyrics/lyricOffset.ts')

// 分桶: netease→cloud, qq→qq, youtube/bili/local/空→none(默认 0)
assert.equal(offsetBucketForSource('qq'), 'qq')
assert.equal(offsetBucketForSource('netease'), 'cloud')
assert.equal(offsetBucketForSource('youtube'), 'none')
assert.equal(offsetBucketForSource('bilibili'), 'none')
assert.equal(offsetBucketForSource('local'), 'none')
assert.equal(offsetBucketForSource(undefined), 'none')

// 系统默认解析: cloud/qq 取各自基线, none 固定 0
assert.equal(resolveLyricDefaultOffsetMs('cloud', 1000, 500), 1000)
assert.equal(resolveLyricDefaultOffsetMs('qq', 1000, 500), 500)
assert.equal(resolveLyricDefaultOffsetMs('none', 1000, 500), 0)

// 核心回归: 系统全局默认不得算进逐曲用户偏移
// 未调整的歌曲 delta 恒为 0, 有效偏移 = 默认 + 0 = 默认(而不是把默认塞进 delta)
const freshTrack = { id: 'netease:1', syncPayload: undefined }
const freshDelta = readSyncedUserOffsetMs(freshTrack)
assert.equal(freshDelta, 0)
const effectiveFresh = resolveLyricDefaultOffsetMs('cloud', 1000, 500) + freshDelta
assert.equal(effectiveFresh, 1000)
// 逐曲编辑器展示的应是 delta(0), 而非 1000
assert.notEqual(freshDelta, 1000)

// 手动调过的歌曲: 有效偏移 = 默认 + delta
const tunedDelta = 250
const effectiveTuned = resolveLyricDefaultOffsetMs('cloud', 1000, 500) + tunedDelta
assert.equal(effectiveTuned, 1250)

// rebase: 改默认时, 已调过的歌曲保持绝对时序不变
const rebased = rebaseLyricUserOffsetMs(250, 1000, 700)
assert.equal(rebased, 550)
assert.equal(1000 + 250, 700 + rebased) // 绝对时序 1250 恒定

// shouldRebase: 仅"来源匹配且 delta != 0"才 rebase
assert.equal(shouldRebaseLyricOffset('cloud', 'cloud', 100), true)
assert.equal(shouldRebaseLyricOffset('cloud', 'cloud', 0), false)
assert.equal(shouldRebaseLyricOffset('qq', 'cloud', 100), false)
assert.equal(shouldRebaseLyricOffset('cloud', 'qq', 100), false)
assert.equal(shouldRebaseLyricOffset('none', 'cloud', 100), false)
assert.equal(shouldRebaseLyricOffset('cloud', 'none', 100), false)

// clamp: 归一为整数并夹到安全边界, 非有限值归零
assert.equal(clampLyricOffsetMs(30500), 30000)
assert.equal(clampLyricOffsetMs(-40000), -30000)
assert.equal(clampLyricOffsetMs(1.6), 2)
assert.equal(clampLyricOffsetMs(Number.NaN), 0)
assert.equal(clampLyricOffsetMs(Number.POSITIVE_INFINITY), 0)

// 存储键: 优先 id, 兜底 playlistKey, 均无则空串
assert.equal(lyricUserOffsetStorageKey({ id: 'netease:1' }), 'netease:1')
assert.equal(lyricUserOffsetStorageKey({ id: '', playlistKey: 'k' }), 'k')
assert.equal(lyricUserOffsetStorageKey(null), '')

// sync-in: 读取 Android 写下的逐曲偏移, 缺失/非法均回退 0
assert.equal(readSyncedUserOffsetMs({ syncPayload: { userLyricOffsetMs: 300 } }), 300)
assert.equal(readSyncedUserOffsetMs({ syncPayload: { user_lyric_offset_ms: 180 } }), 180)
assert.equal(readSyncedUserOffsetMs({ syncPayload: {} }), 0)
assert.equal(readSyncedUserOffsetMs({}), 0)
assert.equal(readSyncedUserOffsetMs({ syncPayload: { userLyricOffsetMs: 'x' } }), 0)

const {
  withUpdatedUserOffsetPayload,
} = await loadTsModule('../src/modules/lyrics/lyricOffset.ts')

const offsetPayload = withUpdatedUserOffsetPayload({ matchedLyric: 'x' }, 250)
assert.equal(offsetPayload.userLyricOffsetMs, 250)
assert.equal(offsetPayload.syncMetadataVersion, 1)
assert.equal(offsetPayload.matchedLyric, 'x')

// 默认常量与 Android 对齐(DEFAULT_CLOUD=1000 / DEFAULT_QQ=500)
assert.equal(DEFAULT_CLOUD_LYRIC_OFFSET_MS, 1000)
assert.equal(DEFAULT_QQ_LYRIC_OFFSET_MS, 500)

console.log('lyric offset tests passed')
