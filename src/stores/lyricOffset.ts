// 逐曲歌词偏移(delta) store：对齐 Android 的两层偏移模型
//
// 背景: 桌面端此前把"系统全局默认偏移"直接当成"逐曲用户偏移"来编辑/展示,
// 于是每首歌都显示成默认值(如 1000ms)。这里把两层拆开:
//   - 系统全局默认(基线): settings.cloudMusicOffset / qqMusicOffset, 全局共享
//   - 逐曲用户偏移(delta): 默认 0, 只有对该曲手动微调后才非 0, 本地持久化
// 有效偏移 = 基线 + delta。delta 以曲目 id 为键存本地, 并可从同步载荷里读取
// Android 写下的逐曲值(sync-in), 从而双端语义一致。
import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { useSettingsStore } from '@/stores/settings'
import {
  clampLyricOffsetMs,
  lyricUserOffsetStorageKey,
  offsetBucketForSource,
  readSyncedUserOffsetMs,
  rebaseLyricUserOffsetMs,
  resolveLyricDefaultOffsetMs,
  withUpdatedUserOffsetPayload,
  type LyricOffsetBucket,
} from '@/modules/lyrics/lyricOffset'
import { persistTrackSyncPayload } from '@/modules/lyrics/syncTrackPayload'
import { usePlayerStore } from '@/stores/player'

const STORAGE_KEY = 'neri.lyric-user-offsets'

type OffsetTrack =
  | {
      id?: string | null
      source?: string | null
      playlistKey?: string | null
      syncPayload?: Record<string, unknown> | null
    }
  | null
  | undefined

// 从曲目键(如 netease:123 / qq:456)还原来源, 用于 rebase 时分桶
function sourceFromKey(key: string): string {
  const idx = key.indexOf(':')
  return idx > 0 ? key.slice(0, idx) : 'local'
}

function readStored(): Record<string, number> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== 'object') return {}
    const result: Record<string, number> = {}
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      const num = clampLyricOffsetMs(typeof value === 'number' ? value : Number(value))
      if (key && num !== 0) result[key] = num
    }
    return result
  } catch {
    // 本地缓存损坏时降级为空表, 不阻断偏移功能
    return {}
  }
}

function writeStored(map: Record<string, number>) {
  try {
    const compact: Record<string, number> = {}
    for (const [key, value] of Object.entries(map)) {
      if (value !== 0) compact[key] = value
    }
    localStorage.setItem(STORAGE_KEY, JSON.stringify(compact))
  } catch {
    // localStorage 不可用(隐私模式/配额)时静默降级, 仅丢失持久化
  }
}

export const useLyricOffsetStore = defineStore('lyricOffset', () => {
  const settings = useSettingsStore()
  const offsets = ref<Record<string, number>>(readStored())

  function persist() {
    writeStored(offsets.value)
  }

  // 逐曲用户偏移(delta): 本地覆盖优先, 其次同步载荷里的 Android 值, 否则 0
  function getUserOffsetMs(track: OffsetTrack): number {
    const key = lyricUserOffsetStorageKey(track)
    if (key && Object.prototype.hasOwnProperty.call(offsets.value, key)) {
      return offsets.value[key]
    }
    return clampLyricOffsetMs(readSyncedUserOffsetMs(track))
  }

  function setUserOffsetMs(track: OffsetTrack, value: number) {
    const key = lyricUserOffsetStorageKey(track)
    if (!key) return
    const delta = clampLyricOffsetMs(value)
    const next = { ...offsets.value }
    // delta 归零即视为"未调整", 删除键避免本地表膨胀, 也让 rebase 跳过
    if (delta === 0) delete next[key]
    else next[key] = delta
    offsets.value = next
    persist()

    // 写回 syncPayload.userLyricOffsetMs, 对齐 Android SongItem 字段, 供同步上传
    try {
      const player = usePlayerStore()
      const current = player.currentTrack
      const sameTrack = current
        && (lyricUserOffsetStorageKey(current) === key
          || (!!track?.id && current.id === track.id))
      if (sameTrack && current) {
        const nextPayload = withUpdatedUserOffsetPayload(current.syncPayload, delta)
        player.patchCurrentTrackSyncPayload(nextPayload)
        void persistTrackSyncPayload(player.currentTrack)
      }
    } catch {
      // player store 未就绪时仅保留本地 delta, 不阻断偏移编辑
    }
  }

  // 该来源的系统全局默认偏移(基线)
  function defaultOffsetMs(bucket: LyricOffsetBucket): number {
    return resolveLyricDefaultOffsetMs(bucket, settings.cloudMusicOffset, settings.qqMusicOffset)
  }

  // 有效偏移 = 基线 + 逐曲 delta, 供歌词渲染使用
  function effectiveOffsetMs(track: OffsetTrack, bucket: LyricOffsetBucket): number {
    return defaultOffsetMs(bucket) + getUserOffsetMs(track)
  }

  // 系统默认变化时, 对"来源匹配且已手动调过"的歌曲 rebase, 保持绝对时序不变
  function rebaseBucket(bucket: LyricOffsetBucket, prevDefault: number, newDefault: number) {
    if (prevDefault === newDefault) return
    const next = { ...offsets.value }
    let changed = false
    for (const key of Object.keys(next)) {
      if (offsetBucketForSource(sourceFromKey(key)) !== bucket) continue
      const delta = next[key]
      if (!delta) continue
      const rebased = clampLyricOffsetMs(rebaseLyricUserOffsetMs(delta, prevDefault, newDefault))
      if (rebased === 0) delete next[key]
      else next[key] = rebased
      changed = true
    }
    if (changed) {
      offsets.value = next
      persist()
    }
  }

  // hydration 守卫: 首次(含从磁盘读入设置)只建立基线, 之后用户手动调整默认才触发 rebase
  let lastCloud: number | null = settings.isHydrated ? settings.cloudMusicOffset : null
  let lastQq: number | null = settings.isHydrated ? settings.qqMusicOffset : null
  watch(
    () => settings.cloudMusicOffset,
    (next) => {
      if (lastCloud === null) {
        lastCloud = next
        return
      }
      const prev = lastCloud
      lastCloud = next
      rebaseBucket('cloud', prev, next)
    },
  )
  watch(
    () => settings.qqMusicOffset,
    (next) => {
      if (lastQq === null) {
        lastQq = next
        return
      }
      const prev = lastQq
      lastQq = next
      rebaseBucket('qq', prev, next)
    },
  )

  return {
    offsets,
    getUserOffsetMs,
    setUserOffsetMs,
    defaultOffsetMs,
    effectiveOffsetMs,
    rebaseBucket,
  }
})
