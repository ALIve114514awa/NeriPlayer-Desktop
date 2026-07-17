import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import ts from 'typescript'

const mockModule = Buffer.from(`
  export const invoke = (command, args) => globalThis.__playbackInvoke(command, args)
`).toString('base64')
const mockModuleUrl = `data:text/javascript;base64,${mockModule}`
const sourceUrl = new URL('../src/utils/playbackSource.ts', import.meta.url)
const source = (await readFile(sourceUrl, 'utf8')).replace(
  "from '@tauri-apps/api/core'",
  `from '${mockModuleUrl}'`,
)
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText
const moduleUrl = `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
const {
  playbackCacheReadCandidates,
  playbackCacheWriteOptions,
  resolvePlaybackResult,
  resolvePlaybackSource,
} = await import(moduleUrl)

const settings = {
  neteaseQuality: 'exhigh',
  qqMusicQuality: 'high',
  biliQuality: 'high',
  youtubeQuality: 'high',
}

function track(id) {
  return {
    id: `netease:${id}`,
    title: `song-${id}`,
    artist: 'artist',
    album: 'album',
    durationMs: 180_000,
    audioUrl: '',
    source: 'netease',
  }
}

async function run(name, test) {
  await test()
  console.log(`ok - ${name}`)
}

await run('continues below preview quality and selects the first full resource', async () => {
  const qualities = []
  const responses = [
    {
      url: 'https://music.example/preview.mp3',
      bitrate: 320_000,
      format: 'mp3',
      expected_content_length: 900_000,
      is_preview: true,
      unavailable_reason: null,
    },
    {
      url: 'https://music.example/full.mp3',
      bitrate: 192_000,
      format: 'mp3',
      expected_content_length: 4_800_000,
      is_preview: false,
      unavailable_reason: null,
    },
  ]
  globalThis.__playbackInvoke = async (command, args) => {
    assert.equal(command, 'get_netease_song_url')
    qualities.push(args.quality)
    return responses.shift()
  }

  const resolved = await resolvePlaybackSource(track(101), settings)

  assert.deepEqual(qualities, ['exhigh', 'higher'])
  assert.equal(resolved?.qualityKey, 'higher')
  assert.equal(resolved?.isPreview, false)
  assert.equal(resolved?.expectedContentLength, 4_800_000)
  assert.match(resolved?.cacheKey ?? '', /-higher$/)
})

await run('keeps only the final preview fallback and forbids formal cache writes', async () => {
  const qualities = []
  globalThis.__playbackInvoke = async (_command, args) => {
    qualities.push(args.quality)
    return {
      url: `https://music.example/${args.quality}-preview.mp3`,
      bitrate: 128_000,
      format: 'mp3',
      expected_content_length: 800_000,
      is_preview: true,
      unavailable_reason: null,
    }
  }

  const resolved = await resolvePlaybackSource(track(102), settings)

  assert.deepEqual(qualities, ['exhigh', 'higher', 'standard'])
  assert.equal(resolved?.qualityKey, 'standard')
  assert.equal(resolved?.isPreview, true)
  assert.deepEqual(playbackCacheWriteOptions(resolved, 0), {})
})

await run('candidate streams use isolated formal cache keys', async () => {
  const resolved = {
    type: 'success',
    url: 'https://audio.example/primary',
    candidateUrls: ['https://audio.example/fallback'],
    cacheKey: 'primary-cache',
    cacheKeyOverride: 'resolved-cache',
    expectedContentLength: 12_345,
    source: 'bilibili',
    qualityKey: 'lossless',
  }

  assert.deepEqual(playbackCacheWriteOptions(resolved, 0), {
    cacheKey: 'resolved-cache',
    expectedContentLength: 12_345,
  })
  assert.deepEqual(
    playbackCacheWriteOptions(resolved, 1, resolved.candidateUrls[0]),
    {
      cacheKey: 'resolved-cache|candidate:1|https://audio.example/fallback',
    },
  )
})

await run('cache-first keys match resolution keys and include NetEase fallbacks', async () => {
  const neteaseCandidates = playbackCacheReadCandidates(track(105), settings)
  assert.deepEqual(
    neteaseCandidates.map(candidate => candidate.qualityKey),
    ['exhigh', 'higher', 'standard'],
  )

  const biliTrack = {
    id: 'bilibili:BV1cache',
    title: 'cached-video',
    artist: 'artist',
    album: 'Bilibili|987654',
    durationMs: 180_000,
    audioUrl: '',
    source: 'bilibili',
  }
  const [biliCandidate] = playbackCacheReadCandidates(biliTrack, settings)
  globalThis.__playbackInvoke = async (command) => {
    assert.equal(command, 'get_bili_audio_url')
    return {
      url: 'https://audio.example/bili-primary',
      bandwidth: 320_000,
      codecs: 'mp4a.40.2',
      candidates: [],
    }
  }

  const resolved = await resolvePlaybackSource(biliTrack, settings)
  assert.equal(biliCandidate.cacheKey, resolved?.cacheKey)
})

await run('surfaces the Android-aligned login requirement', async () => {
  globalThis.__playbackInvoke = async () => ({
    url: null,
    bitrate: 0,
    format: 'mp3',
    is_preview: false,
    unavailable_reason: 'requires_login',
  })

  const resolution = await resolvePlaybackResult(track(103), settings)

  assert.equal(resolution.type, 'requires_login')
})

await run('does not retry lower qualities after an unknown response failure', async () => {
  let calls = 0
  globalThis.__playbackInvoke = async () => {
    calls++
    return {
      url: null,
      bitrate: 0,
      format: 'mp3',
      is_preview: false,
      unavailable_reason: 'unknown',
    }
  }

  const resolved = await resolvePlaybackSource(track(104), settings)

  assert.equal(resolved, null)
  assert.equal(calls, 1)
})

console.log('playback source tests passed')
