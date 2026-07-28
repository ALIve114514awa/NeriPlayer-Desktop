/**
 * Listen Together mapper 对齐回归
 * node scripts/test-listen-together-mapper.mjs
 */
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

async function loadMapperModule() {
  const protocolPath = path.join(root, 'src/stores/listenTogether/protocol.ts')
  const mapperPath = path.join(root, 'src/stores/listenTogether/mapper.ts')
  const protocolSource = await readFile(protocolPath, 'utf8')
  const mapperSource = await readFile(mapperPath, 'utf8')

  // 去掉对 protocol 的 import, 把 channel 常量内联后一起 transpile
  const protocolConsts = protocolSource
    .split('export interface')[0]
    .replace(/^[\s\S]*?export const LtChannels/, 'const LtChannels')
  const mapperBody = mapperSource
    .replace(/import type \{ TrackInfo \} from ['"]@\/stores\/player['"]\s*/g, '')
    .replace(/import \{ LtChannels, type ListenTogetherTrack \} from ['"]\.\/protocol['"]\s*/g, '')

  const combined = `${protocolConsts}\n${mapperBody}\nexport { LtChannels }\n`
  const compiled = ts.transpileModule(combined, {
    compilerOptions: {
      module: ts.ModuleKind.ES2022,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText
  const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
  return import(moduleUrl)
}

async function loadProtocolModule() {
  const protocolPath = path.join(root, 'src/stores/listenTogether/protocol.ts')
  const protocolSource = await readFile(protocolPath, 'utf8')
  const compiled = ts.transpileModule(protocolSource, {
    compilerOptions: {
      module: ts.ModuleKind.ES2022,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText
  const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
  return import(moduleUrl)
}

const {
  buildStableKey,
  trackInfoToLtTrack,
  ltTrackToTrackInfo,
  toShareableQueueSnapshot,
  isTrustedInboundStreamUrl,
} = await loadMapperModule()

const {
  normalizeLtHttpBaseUrl,
  normalizeLtInviteBaseUrl,
  normalizeLtJoinSecret,
  resolveLtJoinSecret,
} = await loadProtocolModule()

assert.equal(
  normalizeLtHttpBaseUrl(' https://listen.example/room/ '),
  'https://listen.example/room',
)
assert.equal(normalizeLtInviteBaseUrl('https://listen.example/room/'), 'https://listen.example/room')
assert.equal(normalizeLtInviteBaseUrl('http://listen.example'), null)
assert.equal(normalizeLtInviteBaseUrl('https://listen.example?token=secret'), null)
assert.equal(normalizeLtInviteBaseUrl('javascript:alert(1)'), null)
assert.equal(normalizeLtJoinSecret(' invite-secret '), 'invite-secret')
assert.equal(normalizeLtJoinSecret('x'.repeat(257)), undefined)
assert.equal(resolveLtJoinSecret(undefined, 'invite-secret'), 'invite-secret')
assert.equal(resolveLtJoinSecret('response-secret', 'invite-secret'), 'response-secret')

// buildStableKey: 对齐 Android buildStableTrackKey
assert.equal(buildStableKey('netease', '123'), 'netease:123')
assert.equal(buildStableKey('bilibili', 'BV1', '999'), 'bilibili:BV1:999')
assert.equal(buildStableKey('bilibili', 'BV1'), 'bilibili:BV1')
assert.equal(buildStableKey('youtubeMusic', 'vid'), 'youtubeMusic:vid')
assert.equal(
  buildStableKey('youtubeMusic', 'vid', undefined, 'PL123'),
  'youtubeMusic:vid:PL123',
)

// 入站直链必须同时满足协议和来源 CDN 白名单
assert.equal(isTrustedInboundStreamUrl('https://m801.music.126.net/song.mp3', 'netease'), true)
assert.equal(isTrustedInboundStreamUrl('https://example.test/song.mp3', 'netease'), false)
assert.equal(isTrustedInboundStreamUrl('file:///tmp/song.mp3', 'netease'), false)
assert.equal(isTrustedInboundStreamUrl('https://rr1.googlevideo.com/videoplayback?id=1', 'youtubeMusic'), true)
assert.equal(isTrustedInboundStreamUrl('https://rr1.googlevideo.com/videoplayback?id=1', 'qqMusic'), false)

// netease 往返
{
  const track = {
    id: 'netease:42',
    title: 'Song',
    artist: 'Artist',
    album: 'Album',
    durationMs: 1000,
    coverUrl: 'https://cover',
    audioUrl: '',
    source: 'netease',
  }
  const lt = trackInfoToLtTrack(track)
  assert.equal(lt.channelId, 'netease')
  assert.equal(lt.audioId, '42')
  assert.equal(lt.stableKey, 'netease:42')
  const back = ltTrackToTrackInfo(lt)
  assert.equal(back.id, 'netease:42')
  assert.equal(back.source, 'netease')
}

// bilibili subAudioId 从 album 提取
{
  const track = {
    id: 'bilibili:BV1xx',
    title: 'Bili',
    artist: 'UP',
    album: 'Bilibili|888',
    durationMs: 2000,
    coverUrl: '',
    audioUrl: '',
    source: 'bilibili',
  }
  const lt = trackInfoToLtTrack(track)
  assert.equal(lt.channelId, 'bilibili')
  assert.equal(lt.audioId, 'BV1xx')
  assert.equal(lt.subAudioId, '888')
  assert.equal(lt.stableKey, 'bilibili:BV1xx:888')
  const back = ltTrackToTrackInfo(lt)
  assert.equal(back.id, 'bilibili:BV1xx')
  assert.equal(back.album, 'Bilibili|888')
}

// YouTube: playlistContext 进入 stableKey, mediaUri 回填
{
  const track = {
    id: 'youtube:abc',
    title: 'YT',
    artist: 'Chan',
    album: '',
    durationMs: 3000,
    coverUrl: '',
    audioUrl: '',
    source: 'youtube',
    syncPayload: {
      channelId: 'youtube_music',
      audioId: 'abc',
      mediaUri: 'ytmusic://video/abc?playlistId=RDEM',
      playlistContextId: 'RDEM',
    },
  }
  const lt = trackInfoToLtTrack(track)
  assert.equal(lt.channelId, 'youtubeMusic')
  assert.equal(lt.audioId, 'abc')
  assert.equal(lt.playlistContextId, 'RDEM')
  assert.equal(lt.stableKey, 'youtubeMusic:abc:RDEM')
  assert.equal(lt.mediaUri, 'ytmusic://video/abc?playlistId=RDEM')

  const back = ltTrackToTrackInfo(lt)
  assert.equal(back.id, 'youtube:abc')
  assert.equal(back.source, 'youtube')
  assert.equal(back.syncPayload?.playlistContextId, 'RDEM')
  assert.equal(back.syncPayload?.mediaUri, 'ytmusic://video/abc?playlistId=RDEM')
}

// 默认 shareable 快照排除 local
{
  const { queue, resolvedIndex } = toShareableQueueSnapshot(
    [
      {
        id: 'netease:1',
        title: 'A',
        artist: 'a',
        album: '',
        durationMs: 1,
        coverUrl: '',
        audioUrl: '',
      },
      {
        id: 'local:x',
        title: 'Local',
        artist: 'l',
        album: '',
        durationMs: 1,
        coverUrl: '',
        audioUrl: '/tmp/a.mp3',
      },
      {
        id: 'netease:2',
        title: 'B',
        artist: 'b',
        album: '',
        durationMs: 1,
        coverUrl: '',
        audioUrl: '',
      },
    ],
    2,
  )
  assert.deepEqual(
    queue.map((t) => t.stableKey),
    ['netease:1', 'netease:2'],
  )
  assert.equal(resolvedIndex, 1)
}

// QQ Music 没有 Android 频道，不能进入跨端共享队列；当前曲被过滤时不回退首项
{
  const { queue, resolvedIndex } = toShareableQueueSnapshot([
    {
      id: 'qq:mid', title: 'QQ', artist: 'Artist', album: '', durationMs: 1,
      coverUrl: '', audioUrl: '',
    },
    {
      id: 'netease:ok', title: 'Netease', artist: 'Artist', album: '', durationMs: 1,
      coverUrl: '', audioUrl: '',
    },
  ], 0)
  assert.deepEqual(queue.map(t => t.stableKey), ['netease:ok'])
  assert.equal(resolvedIndex, -1)
}

// 本地当前曲被排除时同样没有有效共享索引
{
  const { queue, resolvedIndex } = toShareableQueueSnapshot([
    {
      id: 'netease:ok', title: 'Netease', artist: 'Artist', album: '', durationMs: 1,
      coverUrl: '', audioUrl: '',
    },
    {
      id: 'local:private', title: 'Local', artist: 'Artist', album: '', durationMs: 1,
      coverUrl: '', audioUrl: '/tmp/private.mp3',
    },
  ], 1)
  assert.deepEqual(queue.map(t => t.stableKey), ['netease:ok'])
  assert.equal(resolvedIndex, -1)
}

console.log('test-listen-together-mapper: ok')
