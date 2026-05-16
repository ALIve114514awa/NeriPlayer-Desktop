<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { usePlayerStore, displayAlbum } from '@/stores/player'
import { useRecommendStore } from '@/stores/recommend'
import { useSettingsStore } from '@/stores/settings'
import { useToastStore } from '@/stores/toast'
import { useDownloadStore } from '@/stores/download'
import { useI18n } from 'vue-i18n'
import { invoke } from '@tauri-apps/api/core'
import { extractPalette, type PaletteResult } from '@/utils/paletteExtractor'
import HyperBackground from './HyperBackground.vue'
import CoverBlurBackground from './CoverBlurBackground.vue'
import WaveformSlider from './WaveformSlider.vue'
import LyricsView from './LyricsView.vue'
import QueuePanel from './QueuePanel.vue'
import AddToPlaylistDialog from './AddToPlaylistDialog.vue'

const emit = defineEmits<{ collapse: [] }>()
const props = defineProps<{
  hideHeader?: boolean
}>()
const player = usePlayerStore()
const recommend = useRecommendStore()
const settings = useSettingsStore()
const toast = useToastStore()
const downloadStore = useDownloadStore()
const { t } = useI18n()
const showLyrics = ref(false)
const coverLoadError = ref(false)
const showVolumeSlider = ref(false)
const showQueue = ref(false)
const showAddToPlaylist = ref(false)
const showAudioFxPanel = ref(false)
const showSleepMenu = ref(false)
const showMoreSheet = ref(false)

// ─── 均衡器辅助 ───
import { EQ_PRESETS } from '@/stores/player'
const eqPresetIds = Object.keys(EQ_PRESETS)
const eqFreqLabels = ['60', '230', '910', '3.6k', '14k']

function onEqBandChange(index: number, value: number) {
  const bands = [...player.equalizerBands]
  bands[index] = Math.round(value)
  player.setEqualizer(player.equalizerEnabled, bands)
  player.equalizerPresetId = 'custom'
}

// --- 来源徽章（对齐 Android PlaybackSourceBadge） ---
const playbackSourceLabel = computed(() => {
  const id = player.currentTrack?.id || ''
  if (id.startsWith('netease:')) return t('player.source_netease')
  if (id.startsWith('bilibili:')) return t('player.source_bilibili')
  if (id.startsWith('youtube:')) return t('player.source_youtube')
  if (id.startsWith('local:') || player.currentTrack?.audioUrl?.startsWith('file:')) return t('player.source_local')
  return ''
})
const playbackSourceIcon = computed(() => {
  const id = player.currentTrack?.id || ''
  if (id.startsWith('netease:')) return 'cloud'
  if (id.startsWith('bilibili:')) return 'smart_display'
  if (id.startsWith('youtube:')) return 'play_circle'
  return 'folder'
})
const showSourceBadge = computed(() => settings.showCoverBadge && playbackSourceLabel.value !== '')

// --- 歌词编辑器 ---
const lyricsEditorText = ref('')

function openLyricsEditor() {
  // 将当前歌词还原为 LRC 文本
  const lines = displayLyrics.value
  if (lines.length > 0) {
    lyricsEditorText.value = lines.map(l => {
      const min = Math.floor(l.startMs / 60000)
      const sec = Math.floor((l.startMs % 60000) / 1000)
      const ms = Math.floor((l.startMs % 1000) / 10)
      return `[${String(min).padStart(2, '0')}:${String(sec).padStart(2, '0')}.${String(ms).padStart(2, '0')}]${l.text}`
    }).join('\n')
  } else {
    lyricsEditorText.value = ''
  }
  goToSubView('lyrics-editor')
}

async function applyLyricsFromEditor() {
  const text = lyricsEditorText.value.trim()
  if (!text) {
    // 清除歌词
    fetchedLyrics.value = []
    toast.success(t('player.lyrics_cleared'))
    goBackToMain()
    return
  }
  try {
    const parsed = await invoke<any[]>('parse_lrc_content', { content: text })
    fetchedLyrics.value = parsed.map(l => ({
      startMs: l.start_ms,
      durationMs: l.duration_ms,
      words: (l.words || []).map((w: any) => ({
        startMs: w.start_ms, durationMs: w.duration_ms, text: w.text,
      })),
      text: l.text,
      translation: l.translation || undefined,
    }))
    toast.success(t('player.lyrics_applied'))
  } catch (e) {
    console.error('Parse LRC failed:', e)
    toast.error(String(e))
  }
  goBackToMain()
}

// --- 歌词填充（搜索 + 应用歌词） ---
const lyricFillQuery = ref('')
const lyricFillResults = ref<any[]>([])
const isLyricFilling = ref(false)

async function doLyricFillSearch() {
  const q = lyricFillQuery.value.trim()
  if (!q) return
  isLyricFilling.value = true
  lyricFillResults.value = []
  try {
    const results = await invoke<any[]>('search', { query: q, platform: 'netease' })
    lyricFillResults.value = results
  } catch (e) {
    console.error('Lyric fill search failed:', e)
  } finally {
    isLyricFilling.value = false
  }
}

async function applyLyricFill(result: any) {
  try {
    const neteaseId = result.id ? parseInt(String(result.id)) : null
    const lyrics = await invoke<any[]>('fetch_lyrics', {
      title: result.title || '',
      artist: result.artist || '',
      durationSecs: Math.floor((result.duration_ms || 0) / 1000),
      audioPath: null,
      neteaseId: neteaseId,
    })
    fetchedLyrics.value = lyrics.map(l => ({
      startMs: l.start_ms,
      durationMs: l.duration_ms,
      words: (l.words || []).map((w: any) => ({
        startMs: w.start_ms, durationMs: w.duration_ms, text: w.text,
      })),
      text: l.text,
      translation: l.translation || undefined,
    }))
    toast.success(t('player.lyrics_fill_applied'))
    goBackToMain()
  } catch (e) {
    console.error('Lyric fill failed:', e)
    toast.error(String(e))
  }
}

// 供顶栏调用
defineExpose({
  toggleMore() { showMoreSheet.value = !showMoreSheet.value },
})
const fetchedLyrics = ref<any[]>([])
const isFetchingLyrics = ref(false)

// 歌词拖动预览状态
const previewPositionMs = ref<number | null>(null)
let previewConvergeTimer: ReturnType<typeof setTimeout> | null = null

function onSliderPreview(progress: number) {
  previewPositionMs.value = progress * player.durationMs
}

function onSliderPreviewEnd() {
  // 松手后保持预览 280ms，等待播放位置追上
  if (previewConvergeTimer) clearTimeout(previewConvergeTimer)
  previewConvergeTimer = setTimeout(() => {
    previewPositionMs.value = null
  }, 280)
}

// 从封面提取的动态颜色（归一化 RGBA）
const extractedColors = ref<[number[], number[], number[], number[]]>([
  [0.4, 0.31, 0.64, 1],
  [0.49, 0.36, 0.75, 1],
  [0.56, 0.49, 0.69, 1],
  [0.29, 0.24, 0.43, 1],
])
const paletteResult = ref<PaletteResult | null>(null)

// 使用 Median-Cut 调色板算法从封面提取主色
function extractColorsFromCover(url: string) {
  const img = new Image()
  img.crossOrigin = 'anonymous'
  img.referrerPolicy = 'no-referrer'
  img.onload = () => {
    try {
      const canvas = document.createElement('canvas')
      const size = 64
      canvas.width = size
      canvas.height = size
      const ctx = canvas.getContext('2d', { willReadFrequently: true })!
      ctx.drawImage(img, 0, 0, size, size)
      const imageData = ctx.getImageData(0, 0, size, size)

      const palette = extractPalette(imageData, 16)
      paletteResult.value = palette

      // 归一化到 0..1 RGBA
      const toNorm = (c: [number, number, number]): number[] => [
        c[0] / 255,
        c[1] / 255,
        c[2] / 255,
        1,
      ]
      extractedColors.value = [
        toNorm(palette.dominant),
        toNorm(palette.lightVibrant),
        toNorm(palette.muted),
        toNorm(palette.darkMuted),
      ] as [number[], number[], number[], number[]]
    } catch (e) {
      console.error('[NowPlaying] color extraction failed:', e)
    }
  }
  img.onerror = () => {
    console.error('[NowPlaying] cover image load failed:', url)
  }
  img.src = url
}

// 封面变化时提取颜色
watch(() => player.currentTrack?.coverUrl, (url) => {
  if (url) {
    extractColorsFromCover(url)
  }
}, { immediate: true })

function onSeek(progress: number) {
  player.seekTo(Math.round(progress * player.durationMs))
}

// 当前歌曲的网易云 ID（用于网易云收藏 API）
const currentNeteaseId = computed(() => {
  const id = player.currentTrack?.id
  if (id?.startsWith('netease:')) return parseInt(id.replace('netease:', ''))
  return null
})

// 本地"我喜欢的音乐"歌单 ID
const LIKED_PLAYLIST_NAMES = ['我喜欢的音乐', '我喜歡的音樂', 'お気に入りの曲', 'Liked Songs', 'My Favorite Music']
const localLikedPlaylistId = ref<number | null>(null)
const localLikedTrackIds = ref<Set<string>>(new Set())

// 加载本地"我喜欢的音乐"歌单数据
async function loadLocalLikedPlaylist() {
  try {
    const playlists = await invoke<any[]>('list_playlists')
    const liked = playlists.find(p => LIKED_PLAYLIST_NAMES.includes(p.name))
    if (liked) {
      localLikedPlaylistId.value = liked.id
      const tracks = await invoke<any[]>('get_playlist_tracks', { id: liked.id })
      localLikedTrackIds.value = new Set(tracks.map((t: any) => t.id))
    }
  } catch (e) {
    console.error('loadLocalLikedPlaylist:', e)
  }
}

// 收藏状态：检查本地"我喜欢的音乐"歌单
const isFavorite = computed(() => {
  const trackId = player.currentTrack?.id
  if (!trackId) return false
  return localLikedTrackIds.value.has(trackId)
})

async function toggleFavorite() {
  const track = player.currentTrack
  if (!track) return

  // 网易云歌曲：同时调用 Netease API
  const nid = currentNeteaseId.value
  if (nid) {
    recommend.toggleLikeSong(nid, !isFavorite.value).catch(() => {})
  }

  // 本地歌单操作
  if (!localLikedPlaylistId.value) {
    // 自动创建"我喜欢的音乐"歌单
    try {
      const created = await invoke<any>('create_playlist', { name: '我喜欢的音乐' })
      localLikedPlaylistId.value = created.id
    } catch (e) {
      console.error('create liked playlist:', e)
      return
    }
  }

  try {
    if (isFavorite.value) {
      await invoke('remove_from_playlist', {
        playlistId: localLikedPlaylistId.value,
        trackId: track.id,
      })
      localLikedTrackIds.value.delete(track.id)
    } else {
      await invoke('add_to_playlist', {
        playlistId: localLikedPlaylistId.value,
        track: {
          id: track.id,
          title: track.title,
          artist: track.artist,
          album: track.album,
          duration_ms: track.durationMs,
          cover_url: track.coverUrl || null,
          url: track.audioUrl || '',
          source: 'local',
        },
      })
      localLikedTrackIds.value.add(track.id)
    }
  } catch (e) {
    console.error('toggleFavorite local:', e)
  }
}

// 曲目切换或打开时加载
watch(() => player.currentTrack?.id, () => {
  loadLocalLikedPlaylist()
}, { immediate: true })

// 睡眠定时器选项
const sleepOptions = computed(() => [
  { label: t('player.sleep_15'), value: 15 },
  { label: t('player.sleep_30'), value: 30 },
  { label: t('player.sleep_45'), value: 45 },
  { label: t('player.sleep_60'), value: 60 },
  { label: t('player.sleep_90'), value: 90 },
  { label: t('player.sleep_end_of_track'), value: -1 },
  { label: t('player.sleep_end_of_queue'), value: -2 },
])

function handleSleepOption(value: number) {
  if (value === -1) {
    player.startSleepTimerEndOfTrack()
  } else if (value === -2) {
    player.startSleepTimerEndOfQueue()
  } else {
    player.startSleepTimer(value)
  }
}

function formatSleepRemaining(seconds: number): string {
  if (seconds <= 0) return ''
  const m = Math.floor(seconds / 60)
  const s = seconds % 60
  return m > 0 ? `${m}:${s.toString().padStart(2, '0')}` : `${s}s`
}

// 封面 URL
const coverUrl = computed(() => player.currentTrack?.coverUrl || '')

// 封面加载错误时重置
watch(() => player.currentTrack?.id, () => {
  coverLoadError.value = false
})

// 当曲目切换时自动获取歌词
watch(() => player.currentTrack?.id, async (id) => {
  if (!id || !player.currentTrack) {
    fetchedLyrics.value = []
    return
  }

  isFetchingLyrics.value = true
  try {
    const track = player.currentTrack
    const neteaseId = id.startsWith('netease:') ? parseInt(id.replace('netease:', '')) : undefined

    const lyrics = await invoke<any[]>('fetch_lyrics', {
      title: track.title,
      artist: track.artist,
      durationSecs: Math.floor(track.durationMs / 1000),
      audioPath: track.audioUrl || null,
      neteaseId: neteaseId || null,
    })

    fetchedLyrics.value = lyrics.map(l => ({
      startMs: l.start_ms,
      durationMs: l.duration_ms,
      words: (l.words || []).map((w: any) => ({
        startMs: w.start_ms, durationMs: w.duration_ms, text: w.text,
      })),
      text: l.text,
      translation: l.translation || undefined,
    }))
  } catch (e) {
    console.error('Fetch lyrics failed:', e)
    fetchedLyrics.value = []
  } finally {
    isFetchingLyrics.value = false
  }
}, { immediate: true })

// --- 唱片旋转（JS 驱动，停止时保持角度 + 缓动） ---
const discRef = ref<HTMLDivElement>()
let discAngle = 0            // 当前累计角度（度）
let discAnimFrame = 0
let discLastTime = 0
const DISC_RPM = 2.4         // 每秒转过的度数 = 360 / 25s ≈ 14.4 deg/s
const DEG_PER_MS = 360 / 25000

function animateDisc(timestamp: number) {
  if (!discLastTime) discLastTime = timestamp
  const dt = timestamp - discLastTime
  discLastTime = timestamp

  if (player.isPlaying) {
    discAngle = (discAngle + DEG_PER_MS * dt) % 360
  }
  if (discRef.value) {
    discRef.value.style.transform = `rotate(${discAngle}deg)`
  }
  discAnimFrame = requestAnimationFrame(animateDisc)
}

onMounted(() => {
  discAnimFrame = requestAnimationFrame(animateDisc)
})
onUnmounted(() => {
  cancelAnimationFrame(discAnimFrame)
})

// --- 右键菜单（歌曲名/歌手复制 + 封面保存） ---
const contextMenu = ref({ show: false, x: 0, y: 0, type: '' as 'title' | 'artist' | 'cover' })

function openContextMenu(e: MouseEvent, type: 'title' | 'artist' | 'cover') {
  e.preventDefault()
  const x = Math.min(e.clientX, window.innerWidth - 220)
  const y = Math.min(e.clientY, window.innerHeight - 100)
  contextMenu.value = { show: true, x, y, type }
}

function closeContextMenu() {
  contextMenu.value.show = false
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast.success(t('player.copied'))
  } catch {
    toast.error(t('player.copy_failed'))
  }
  closeContextMenu()
}

async function saveCoverArt() {
  closeContextMenu()
  const url = player.currentTrack?.coverUrl
  if (!url) return

  try {
    const { save } = await import('@tauri-apps/plugin-dialog')
    const filePath = await save({
      defaultPath: `${player.currentTrack?.title || 'cover'}.jpg`,
      filters: [{ name: 'Image', extensions: ['jpg', 'png', 'webp'] }],
    })
    if (!filePath) return

    const response = await fetch(url, { referrerPolicy: 'no-referrer' })
    const blob = await response.blob()
    const arrayBuffer = await blob.arrayBuffer()

    await invoke('save_file_bytes', {
      path: filePath,
      data: Array.from(new Uint8Array(arrayBuffer)),
    })
    toast.success(t('player.cover_saved'))
  } catch (e) {
    console.error('Save cover failed:', e)
    toast.error(t('player.cover_save_failed'))
  }
}

const displayLyrics = computed(() => {
  if (player.lyrics.length) return player.lyrics
  if (fetchedLyrics.value.length) return fetchedLyrics.value
  return []
})

// 更多选项面板子视图
const moreSheetView = ref<'main' | 'offset' | 'fontsize' | 'speed' | 'search' | 'editinfo' | 'quality' | 'lyrics-editor' | 'lyrics-fill'>('main')
const moreSheetTransition = ref('slide-left')

function goToSubView(view: typeof moreSheetView.value) {
  moreSheetTransition.value = 'slide-left'
  moreSheetView.value = view
}
function goBackToMain() {
  moreSheetTransition.value = 'slide-right'
  moreSheetView.value = 'main'
}

// 关闭更多选项面板时重置子视图
watch(showMoreSheet, (v) => { if (!v) setTimeout(() => { moreSheetView.value = 'main' }, 220) })

// --- 获取歌曲信息（搜索） ---
const searchQuery = ref('')
const searchResults = ref<any[]>([])
const isSearching = ref(false)

async function doSearch() {
  const q = searchQuery.value.trim()
  if (!q) return
  isSearching.value = true
  searchResults.value = []
  try {
    const results = await invoke<any[]>('search', { query: q, platform: 'netease' })
    searchResults.value = results
  } catch (e) {
    console.error('Search failed:', e)
  } finally {
    isSearching.value = false
  }
}

function applySearchResult(result: any) {
  player.updateCurrentTrackInfo({
    title: result.title || player.currentTrack?.title,
    artist: result.artist || player.currentTrack?.artist,
    coverUrl: result.cover_url || player.currentTrack?.coverUrl,
  })
  toast.success(t('player.info_applied'))
  goBackToMain()
}

// --- 编辑歌曲信息 ---
const editTitle = ref('')
const editArtist = ref('')
const editCoverUrl = ref('')

function openEditInfo() {
  editTitle.value = player.currentTrack?.title || ''
  editArtist.value = player.currentTrack?.artist || ''
  editCoverUrl.value = player.currentTrack?.coverUrl || ''
  goToSubView('editinfo')
}

function saveEditInfo() {
  player.updateCurrentTrackInfo({
    title: editTitle.value,
    artist: editArtist.value,
    coverUrl: editCoverUrl.value,
  })
  toast.success(t('player.info_applied'))
  goBackToMain()
}

function restoreInfo() {
  player.restoreOriginalTrackInfo()
  toast.success(t('player.info_restored'))
  goBackToMain()
}

// --- 音质切换 ---
const currentSource = computed(() => {
  const id = player.currentTrack?.id || ''
  if (id.startsWith('netease:')) return 'netease'
  if (id.startsWith('bilibili:')) return 'bilibili'
  if (id.startsWith('youtube:')) return 'youtube'
  return 'local'
})

const currentTrackId = computed(() => player.currentTrack?.id || '')
const currentDownloadTask = computed(() => currentTrackId.value ? downloadStore.downloading.get(currentTrackId.value) : undefined)
const isCurrentDownloaded = computed(() => currentTrackId.value ? downloadStore.isDownloaded(currentTrackId.value) : false)
const isCurrentDownloading = computed(() => !!currentDownloadTask.value)

function downloadTaskStatusText(status?: string) {
  switch (status) {
    case 'resolving': return t('download.resolving')
    case 'downloading': return t('download.downloading')
    case 'cancelling': return t('download.cancelling')
    case 'cancelled': return t('download.cancelled')
    case 'error': return t('download.download_failed')
    case 'already_exists': return t('download.already_exists')
    default: return ''
  }
}

const downloadActionIcon = computed(() => {
  if (player.isPlayingFromDownload) return 'download_done'
  if (isCurrentDownloading.value) {
    const status = currentDownloadTask.value?.status
    if (status === 'error') return 'error'
    if (status === 'cancelled') return 'cancel'
    return 'downloading'
  }
  if (isCurrentDownloaded.value) return 'refresh'
  return 'download'
})

const downloadActionLabel = computed(() => {
  if (player.isPlayingFromDownload) return t('player.playing_from_download')
  if (isCurrentDownloading.value) return downloadTaskStatusText(currentDownloadTask.value?.status) || t('download.downloading')
  if (isCurrentDownloaded.value) return t('download.redownload')
  return t('library.tab_downloads')
})

const downloadActionDesc = computed(() => {
  if (player.isPlayingFromDownload) return t('download.using_local_file')
  if (isCurrentDownloading.value) {
    const task = currentDownloadTask.value
    if (!task) return ''
    if (task.message) return task.message
    if (typeof task.progress === 'number') return `${task.progress}%`
    return ''
  }
  if (isCurrentDownloaded.value) return t('download.redownload_desc')
  return t('download.download_desc')
})

const downloadActionDisabled = computed(() =>
  !player.currentTrack
  || player.isPlayingFromDownload
  || (isCurrentDownloading.value && currentDownloadTask.value?.status !== 'error' && currentDownloadTask.value?.status !== 'cancelled')
)

const neteaseQualities = [
  { key: 'standard', label: 'settings.q_standard' },
  { key: 'higher', label: 'settings.q_high' },
  { key: 'exhigh', label: 'settings.q_exhigh' },
  { key: 'lossless', label: 'settings.q_lossless' },
  { key: 'hires', label: 'settings.q_hires' },
  { key: 'jyeffect', label: 'settings.q_surround' },
  { key: 'sky', label: 'settings.q_sky' },
  { key: 'jymaster', label: 'settings.q_master' },
]

const youtubeQualities = [
  { key: 'low', label: 'settings.q_low' },
  { key: 'medium', label: 'settings.q_medium' },
  { key: 'high', label: 'settings.q_high_yt' },
  { key: 'very_high', label: 'settings.q_very_high' },
]

async function switchQuality(key: string) {
  if (currentSource.value === 'netease') {
    settings.neteaseQuality = key
  } else if (currentSource.value === 'youtube') {
    settings.youtubeQuality = key
  }
  showMoreSheet.value = false
  await player.replayWithQuality()
}

function currentQualityKey(): string {
  if (currentSource.value === 'netease') return settings.neteaseQuality
  if (currentSource.value === 'youtube') return settings.youtubeQuality
  return ''
}

// 下载/重新下载歌曲
async function handleDownloadAction() {
  const track = player.currentTrack
  if (!track) return
  showMoreSheet.value = false
  if (isCurrentDownloaded.value && !player.isPlayingFromDownload) {
    player.handleDownloadedFileRemoved(track.id, downloadStore.getDownloadedTrack(track.id)?.filePath)
    await downloadStore.redownloadTrack(track)
  } else {
    await downloadStore.downloadTrack(track)
  }
}

// 分享歌曲
async function shareSong() {
  const track = player.currentTrack
  if (!track) return
  // 构建分享文本
  let url = ''
  if (track.id.startsWith('netease:')) {
    const nid = track.id.replace('netease:', '')
    url = `https://music.163.com/song?id=${nid}`
  } else if (track.id.startsWith('bilibili:')) {
    const bid = track.id.replace('bilibili:', '')
    url = `https://www.bilibili.com/video/${bid}`
  } else if (track.id.startsWith('youtube:')) {
    const vid = track.id.replace('youtube:', '')
    url = `https://music.youtube.com/watch?v=${vid}`
  }
  const text = url
    ? `${track.title} - ${track.artist}\n${url}`
    : `${track.title} - ${track.artist}`
  try {
    await navigator.clipboard.writeText(text)
    toast.success(t('player.share_copied'))
  } catch {
    toast.error(t('player.copy_failed'))
  }
  showMoreSheet.value = false
}

// 专辑名
const albumName = computed(() => {
  const album = player.currentTrack?.album || ''
  return displayAlbum(album)
})

// 进度条下方音质信息
const audioInfoDisplay = computed(() => {
  const info = player.audioInfo
  if (!info) return ''
  const parts: string[] = []
  if (settings.showAudioCodec && info.codec) parts.push(info.codec)
  if (settings.showQualitySwitch && info.bitrate) parts.push(`${info.bitrate}kbps`)
  return parts.join(' \u00B7 ')
})

// AccentBackdrop 底色（对齐 Android：主色降饱和调暗后铺底）
const accentBgStyle = computed(() => {
  const bg = paletteResult.value?.accentBg
  if (!bg) return { background: 'rgb(18, 18, 18)' }
  return { background: `rgb(${bg[0]}, ${bg[1]}, ${bg[2]})` }
})

// 动态主题 CSS 变量（对齐 Android M3 动态配色）
const dynamicColorVars = computed(() => {
  const p = paletteResult.value
  if (!p) return {}
  const lv = p.lightVibrant

  // RGB → HSL 转换
  const r = lv[0] / 255, g = lv[1] / 255, b = lv[2] / 255
  const max = Math.max(r, g, b), min = Math.min(r, g, b)
  let h = 0, s = 0
  const l = (max + min) / 2
  if (max !== min) {
    const d = max - min
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min)
    if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6
    else if (max === g) h = ((b - r) / d + 2) / 6
    else h = ((r - g) / d + 4) / 6
  }

  // HSL → RGB
  const hsl2rgb = (h: number, s: number, l: number): [number, number, number] => {
    if (s === 0) return [Math.round(l * 255), Math.round(l * 255), Math.round(l * 255)]
    const hue2rgb = (p: number, q: number, t: number) => {
      if (t < 0) t += 1; if (t > 1) t -= 1
      if (t < 1/6) return p + (q - p) * 6 * t
      if (t < 1/2) return q
      if (t < 2/3) return p + (q - p) * (2/3 - t) * 6
      return p
    }
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s
    const pp = 2 * l - q
    return [
      Math.round(hue2rgb(pp, q, h + 1/3) * 255),
      Math.round(hue2rgb(pp, q, h) * 255),
      Math.round(hue2rgb(pp, q, h - 1/3) * 255),
    ]
  }

  // 主色：提升饱和度和亮度确保在暗背景上的可见性（对齐 Android M3 primary）
  const primaryS = Math.min(1, s * 1.2 + 0.15) // 保底饱和度
  const primaryL = Math.max(0.55, Math.min(0.75, l * 0.8 + 0.35)) // 亮度 55~75% 确保对比
  const [pr, pg, pb] = hsl2rgb(h, primaryS, primaryL)
  const primary = `rgb(${pr}, ${pg}, ${pb})`

  // 主色容器：更亮、低饱和度（对齐 Android primaryContainer）
  const pcS = Math.min(1, s * 0.8 + 0.1)
  const pcL = Math.max(0.70, Math.min(0.85, primaryL + 0.15))
  const [pcr, pcg, pcb] = hsl2rgb(h, pcS, pcL)
  const primaryContainer = `rgb(${pcr}, ${pcg}, ${pcb})`

  // 主色上文字：基于 primaryContainer 亮度选深/浅色
  const pcLuma = pcr * 0.299 + pcg * 0.587 + pcb * 0.114
  const onPrimary = pcLuma > 140 ? 'rgb(20, 18, 24)' : 'rgb(255, 255, 255)'

  return {
    '--np-primary': primary,
    '--np-on-primary': onPrimary,
    '--np-primary-container': primaryContainer,
    '--np-on-surface': 'rgba(255,255,255,1)',
    '--np-on-surface-variant': 'rgba(255,255,255,0.78)',
    '--waveform-thumb-color': primary,
  }
})

// 进度条活跃色（与 --np-primary 同步）
const sliderActiveColor = computed(() => {
  const vars = dynamicColorVars.value
  return (vars as any)['--np-primary'] || '#fff'
})
</script>

<template>
  <div class="now-playing" :style="dynamicColorVars">
    <!-- AccentBackdrop 底色层（对齐 Android：主色降饱和+调暗） -->
    <div class="np-bg-solid" :style="accentBgStyle" />
    <!-- 动态背景：封面模糊 OR WebGL 着色器，互斥 -->
    <CoverBlurBackground
      v-if="settings.coverBlurBg"
      :cover-url="coverUrl"
      :blur-amount="settings.coverBlurAmount * 30"
      :darken-alpha="settings.coverBlurDarken"
    />
    <HyperBackground
      v-else-if="settings.dynamicBackground"
      :music-level="settings.audioReactive ? player.audioLevel : 0"
      :beat-impulse="settings.audioReactive ? player.beatImpulse : 0"
      :colors="extractedColors"
      :is-dark="true"
      :light-offset="paletteResult?.lightOffset ?? 0"
      :saturate-offset="paletteResult?.saturateOffset ?? 0"
    />
    <div class="np-scrim" />

    <!-- 顶栏（融合模式下由 TitleBar 接管） -->
    <header v-if="!props.hideHeader" class="np-header">
      <button class="np-icon-btn" @click.stop="emit('collapse')">
        <span class="material-symbols-rounded">keyboard_arrow_down</span>
      </button>
      <div class="np-header-center">
        <span class="np-from-label">{{ t('player.now_playing') }}</span>
        <span class="np-from-name">{{ displayAlbum(player.currentTrack?.album || '') }}</span>
      </div>
      <!-- 收藏 + 更多（对齐 Android 顶栏右侧） -->
      <button
        class="np-icon-btn fav-header-btn"
        :class="{ active: isFavorite }"
        :disabled="!player.currentTrack"
        @click="toggleFavorite"
      >
        <span class="material-symbols-rounded" :class="{ filled: isFavorite }">favorite</span>
      </button>
      <button class="np-icon-btn" @click="showMoreSheet = !showMoreSheet">
        <span class="material-symbols-rounded">more_vert</span>
      </button>
    </header>

    <!-- 双栏 -->
    <div class="np-body" :class="{ 'np-body--no-header': props.hideHeader }">
      <!-- 左侧 -->
      <section class="np-left">
        <div class="cover-wrap" @contextmenu="openContextMenu($event, 'cover')">
          <!-- Card 模式（圆角矩形，对齐 Android） -->
          <div v-if="settings.coverStyle === 'card'" class="cover-card">
            <img
              v-if="coverUrl && !coverLoadError"
              :src="coverUrl"
              referrerpolicy="no-referrer"
              class="cover-card-img"
              @error="coverLoadError = true"
            />
            <span v-else class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.35">music_note</span>
          </div>
          <!-- Disc 模式（黑胶唱片） -->
          <div v-else ref="discRef" class="cover-disc">
            <div class="cover-inner">
              <img
                v-if="coverUrl && !coverLoadError"
                :src="coverUrl"
                referrerpolicy="no-referrer"
                class="cover-img"
                @error="coverLoadError = true"
              />
              <span v-else class="material-symbols-rounded filled" style="font-size: 48px; opacity: 0.35">music_note</span>
            </div>
            <div class="cover-groove" />
            <div class="cover-hole" />
          </div>
          <!-- 来源徽章（对齐 Android PlaybackSourceBadge） -->
          <div v-if="showSourceBadge && settings.coverStyle === 'card'" class="source-badge">
            <span class="material-symbols-rounded source-badge-icon">{{ playbackSourceIcon }}</span>
            <span class="source-badge-label">{{ playbackSourceLabel }}</span>
          </div>
        </div>

        <div class="np-info">
          <h2 class="np-title" @contextmenu="openContextMenu($event, 'title')">{{ player.currentTrack?.title || t('player.not_playing') }}</h2>
          <p class="np-artist" @contextmenu="openContextMenu($event, 'artist')">{{ player.currentTrack?.artist || '' }}</p>
        </div>

        <div class="np-slider-area">
          <WaveformSlider
            :progress="player.interpolatedProgress"
            :is-playing="player.isPlaying"
            :active-color="sliderActiveColor"
            @seek="onSeek"
            @preview="onSliderPreview"
            @preview-end="onSliderPreviewEnd"
          />
          <div class="np-time">
            <span>{{ player.currentTimeFormatted }}</span>
            <span>{{ player.durationFormatted }}</span>
          </div>
          <div v-if="player.audioInfo || player.isPlayingFromDownload" class="np-audio-info">
            <span v-if="player.isPlayingFromDownload" class="np-download-chip">
              <span class="material-symbols-rounded">download_done</span>
              {{ t('player.playing_from_download') }}
            </span>
            <span v-if="audioInfoDisplay" class="np-audio-detail" :class="{ separated: player.isPlayingFromDownload }">
              {{ audioInfoDisplay }}
            </span>
          </div>
        </div>

        <div class="np-controls">
          <button
            class="ctrl-btn"
            :class="{ active: player.shuffleEnabled }"
            @click="player.toggleShuffle()"
          >
            <span class="material-symbols-rounded">shuffle</span>
          </button>

          <button class="ctrl-btn" @click="player.previous()">
            <span class="material-symbols-rounded filled" style="font-size: 30px">skip_previous</span>
          </button>

          <!-- 播放/暂停 带动画 -->
          <button class="play-btn" @click="player.togglePlayPause()" :disabled="player.isLoadingAudio">
            <transition name="play-icon">
              <span
                v-if="player.isLoadingAudio"
                class="material-symbols-rounded spinning play-icon-inner"
                key="loading"
              >progress_activity</span>
              <span
                v-else
                class="material-symbols-rounded filled play-icon-inner"
                :key="player.isPlaying ? 'pause' : 'play'"
              >{{ player.isPlaying ? 'pause' : 'play_arrow' }}</span>
            </transition>
          </button>

          <button class="ctrl-btn" @click="player.next()">
            <span class="material-symbols-rounded filled" style="font-size: 30px">skip_next</span>
          </button>

          <button
            class="ctrl-btn"
            :class="{ active: player.repeatMode !== 'off' }"
            @click="player.toggleRepeatMode()"
          >
            <span class="material-symbols-rounded">{{ player.repeatMode === 'one' ? 'repeat_one' : 'repeat' }}</span>
          </button>
        </div>

        <!-- 工具栏（对齐 Android 底部：Queue → Sleep → Volume → Speed → Add） -->
        <div class="np-toolbar">
          <button class="tool-btn" @click="showQueue = true">
            <span class="material-symbols-rounded">queue_music</span>
          </button>
          <!-- 睡眠定时器 -->
          <div class="sleep-wrap">
            <button
              class="tool-btn"
              :class="{ active: player.sleepTimerMode || showSleepMenu }"
              @click="showSleepMenu = !showSleepMenu"
            >
              <span class="material-symbols-rounded">timer</span>
              <span v-if="player.sleepRemainingSeconds > 0" class="sleep-badge">
                {{ formatSleepRemaining(player.sleepRemainingSeconds) }}
              </span>
            </button>
            <div v-if="showSleepMenu" class="sleep-popover" @mouseleave="showSleepMenu = false">
              <button
                v-for="opt in sleepOptions"
                :key="opt.value"
                class="sleep-option"
                @click="handleSleepOption(opt.value); showSleepMenu = false"
              >
                {{ opt.label }}
              </button>
              <button
                v-if="player.sleepTimerMode"
                class="sleep-option cancel"
                @click="player.cancelSleepTimer(); showSleepMenu = false"
              >
                {{ t('player.sleep_off') }}
              </button>
            </div>
          </div>
          <div class="volume-wrap">
            <button class="tool-btn" @click="showVolumeSlider = !showVolumeSlider">
              <span class="material-symbols-rounded">{{ player.volume === 0 ? 'volume_off' : player.volume < 0.5 ? 'volume_down' : 'volume_up' }}</span>
            </button>
            <div v-if="showVolumeSlider" class="volume-popover" @mouseleave="showVolumeSlider = false">
              <input
                type="range"
                min="0"
                max="1"
                step="0.01"
                :value="1 - player.volume"
                class="volume-slider"
                @input="player.setVolume(1 - parseFloat(($event.target as HTMLInputElement).value))"
              />
              <span class="volume-label">{{ Math.round(player.volume * 100) }}%</span>
            </div>
          </div>
          <!-- 音效 (AudioFX) -->
          <div class="speed-wrap">
            <button class="tool-btn" :class="{ active: showAudioFxPanel || player.hasActiveEffects }" @click="showAudioFxPanel = !showAudioFxPanel">
              <span class="material-symbols-rounded">tune</span>
            </button>
            <div v-if="showAudioFxPanel" class="audiofx-popover" @mouseleave="showAudioFxPanel = false">
              <!-- 播放速度 -->
              <div class="audiofx-section">
                <div class="audiofx-section-header">{{ t('player.speed') }}</div>
                <div class="audiofx-speed-grid">
                  <button
                    v-for="spd in [0.5, 0.75, 0.85, 1.0, 1.25, 1.5, 2.0, 3.0]"
                    :key="spd"
                    class="speed-option"
                    :class="{ active: player.playbackSpeed === spd }"
                    @click="player.setSpeed(spd)"
                  >
                    {{ spd }}x
                  </button>
                </div>
                <div class="audiofx-slider-row">
                  <span class="audiofx-slider-label">0.25x</span>
                  <input
                    type="range" min="0.25" max="3" step="0.05"
                    :value="player.playbackSpeed"
                    class="audiofx-slider"
                    @input="player.setSpeed(parseFloat(($event.target as HTMLInputElement).value))"
                  />
                  <span class="audiofx-slider-label">3x</span>
                  <span class="audiofx-slider-value">{{ player.playbackSpeed.toFixed(2) }}x</span>
                </div>
              </div>

              <!-- 响度增益 -->
              <div class="audiofx-section">
                <div class="audiofx-section-header">{{ t('player.loudness_gain') }}</div>
                <div class="audiofx-preset-row">
                  <button v-for="db in [0, 300, 600, 900, 1200, 1500]" :key="db"
                    class="speed-option" :class="{ active: player.loudnessGainMb === db }"
                    @click="player.setLoudnessGain(db)"
                  >
                    {{ db === 0 ? '0' : '+' + (db / 100).toFixed(0) }}dB
                  </button>
                </div>
                <div class="audiofx-slider-row">
                  <span class="audiofx-slider-label">0</span>
                  <input
                    type="range" min="0" max="1500" step="50"
                    :value="player.loudnessGainMb"
                    class="audiofx-slider"
                    @input="player.setLoudnessGain(parseFloat(($event.target as HTMLInputElement).value))"
                  />
                  <span class="audiofx-slider-label">+15dB</span>
                  <span class="audiofx-slider-value">+{{ (player.loudnessGainMb / 100).toFixed(1) }}dB</span>
                </div>
              </div>

              <!-- 均衡器 -->
              <div class="audiofx-section">
                <div class="audiofx-section-header">
                  {{ t('player.equalizer') }}
                  <label class="audiofx-toggle">
                    <input type="checkbox" :checked="player.equalizerEnabled"
                      @change="player.setEqualizer(($event.target as HTMLInputElement).checked, player.equalizerBands)" />
                    <span class="audiofx-toggle-slider"></span>
                  </label>
                </div>
                <select class="audiofx-select" :value="player.equalizerPresetId"
                  @change="player.setEqualizerPreset(($event.target as HTMLSelectElement).value)">
                  <option v-for="pid in eqPresetIds" :key="pid" :value="pid">
                    {{ t('player.eq_' + pid) }}
                  </option>
                </select>
                <div v-if="player.equalizerEnabled" class="audiofx-eq-bands">
                  <div v-for="(freq, i) in eqFreqLabels" :key="i" class="audiofx-eq-band">
                    <span class="audiofx-eq-val">{{ (player.equalizerBands[i] / 100).toFixed(1) }}</span>
                    <input type="range" min="-1500" max="1500" step="50"
                      class="audiofx-eq-slider" orient="vertical"
                      :value="player.equalizerBands[i]"
                      @input="onEqBandChange(i, parseFloat(($event.target as HTMLInputElement).value))" />
                    <span class="audiofx-eq-freq">{{ freq }}</span>
                  </div>
                </div>
              </div>

              <!-- 音调说明 -->
              <div class="audiofx-note">{{ t('player.pitch_note') }}</div>

              <!-- 重置 -->
              <button class="speed-option audiofx-reset" @click="player.resetAudioEffects()">
                {{ t('player.reset_effects') }}
              </button>
            </div>
          </div>
          <button class="tool-btn" @click="showAddToPlaylist = true">
            <span class="material-symbols-rounded">playlist_add</span>
          </button>
        </div>
      </section>

      <!-- 右侧歌词 -->
      <section class="np-right">
        <LyricsView
          v-if="displayLyrics.length > 0"
          :lyrics="displayLyrics"
          :current-time-ms="player.interpolatedPositionMs"
          :preview-time-ms="previewPositionMs"
          :is-playing="player.isPlaying"
          @seek="(ms) => player.seekTo(ms)"
        />
        <div v-else class="lyrics-empty">
          <span class="material-symbols-rounded" style="font-size: 36px">lyrics</span>
          <p>{{ t('player.no_lyrics') }}</p>
        </div>
      </section>
    </div>

    <!-- 播放队列面板 -->
    <QueuePanel v-if="showQueue" @close="showQueue = false" />
    <AddToPlaylistDialog v-model:open="showAddToPlaylist" :track="player.currentTrack" />

    <!-- 更多选项面板（对齐 Android MoreOptionsSheet） -->
    <Teleport to="body">
      <Transition name="more-sheet">
      <div v-if="showMoreSheet" class="np-more-overlay" @click="showMoreSheet = false">
        <div class="np-more-sheet" @click.stop>

          <Transition :name="moreSheetTransition" mode="out-in">
          <div :key="moreSheetView" class="np-more-sheet-content">

          <!-- 主菜单（对齐 Android MoreOptionsSheet 顺序） -->
          <template v-if="moreSheetView === 'main'">
            <h4 class="np-more-title">{{ t('player.more_options') }}</h4>

            <!-- 获取歌曲信息 -->
            <button class="np-more-list-item" @click="searchQuery = player.currentTrack?.title || ''; searchResults = []; goToSubView('search')">
              <span class="material-symbols-rounded">info</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.get_info') }}</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 编辑歌曲信息 -->
            <button class="np-more-list-item" @click="openEditInfo">
              <span class="material-symbols-rounded">edit</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.edit_info') }}</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 音质切换（仅在线来源显示） -->
            <button v-if="currentSource !== 'local'" class="np-more-list-item" @click="goToSubView('quality')">
              <span class="material-symbols-rounded">music_note</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.quality_switch') }}</span>
                <span class="np-more-list-desc">{{ player.audioInfo?.codec }} · {{ player.audioInfo?.bitrate }}kbps</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 音频效果 -->
            <button class="np-more-list-item" @click="goToSubView('speed')">
              <span class="material-symbols-rounded">tune</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.audio_effects') }}</span>
                <span class="np-more-list-desc">{{ t('player.audio_effects_desc') }}</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 歌词偏移 -->
            <button class="np-more-list-item" @click="goToSubView('offset')">
              <span class="material-symbols-rounded">timer</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.lyric_offset') }}</span>
                <span class="np-more-list-desc">{{ settings.cloudMusicOffset > 0 ? '+' : '' }}{{ settings.cloudMusicOffset }}ms</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 歌词字号 -->
            <button class="np-more-list-item" @click="goToSubView('fontsize')">
              <span class="material-symbols-rounded">format_size</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.font_scale') }}</span>
                <span class="np-more-list-desc">{{ Math.round(settings.lyricFontScale * 100) }}%</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 歌词编辑器（对齐 Android LyricsEditorSheet） -->
            <button class="np-more-list-item" @click="openLyricsEditor">
              <span class="material-symbols-rounded">edit_note</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.lyrics_editor') }}</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 歌词填充（对齐 Android FillOptionsDialog） -->
            <button class="np-more-list-item" @click="lyricFillQuery = player.currentTrack?.title || ''; lyricFillResults = []; goToSubView('lyrics-fill')">
              <span class="material-symbols-rounded">lyrics</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.lyrics_fill') }}</span>
                <span class="np-more-list-desc">{{ t('player.lyrics_fill_desc') }}</span>
              </div>
              <span class="material-symbols-rounded np-more-chevron">chevron_right</span>
            </button>

            <!-- 分享 -->
            <button class="np-more-list-item" @click="shareSong">
              <span class="material-symbols-rounded">share</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ t('player.share') }}</span>
              </div>
            </button>

            <!-- 下载（在线来源：下载 / 重新下载 / 本地播放状态） -->
            <button
              v-if="currentSource !== 'local'"
              class="np-more-list-item"
              :disabled="downloadActionDisabled"
              @click="handleDownloadAction"
            >
              <span class="material-symbols-rounded">{{ downloadActionIcon }}</span>
              <div class="np-more-list-info">
                <span class="np-more-list-headline">{{ downloadActionLabel }}</span>
                <span v-if="downloadActionDesc" class="np-more-list-desc">{{ downloadActionDesc }}</span>
              </div>
            </button>
          </template>

          <!-- 子视图：歌词偏移 -->
          <template v-else-if="moreSheetView === 'offset'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.lyric_offset') }}</h4>
            </div>
            <div class="np-more-item">
              <div class="np-more-row">
                <input type="range" min="-2000" max="2000" step="50"
                  :value="settings.cloudMusicOffset"
                  class="np-more-slider"
                  @input="settings.cloudMusicOffset = parseInt(($event.target as HTMLInputElement).value)"
                />
                <span class="np-offset-value" :class="{ positive: settings.cloudMusicOffset > 0, negative: settings.cloudMusicOffset < 0 }">
                  {{ settings.cloudMusicOffset > 0 ? '+' : '' }}{{ settings.cloudMusicOffset }}ms
                </span>
              </div>
            </div>
          </template>

          <!-- 子视图：字号 -->
          <template v-else-if="moreSheetView === 'fontsize'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.font_scale') }}</h4>
            </div>
            <div class="np-more-item">
              <div class="np-more-row">
                <input type="range" min="0.6" max="1.6" step="0.05"
                  :value="settings.lyricFontScale"
                  class="np-more-slider"
                  @input="settings.lyricFontScale = parseFloat(($event.target as HTMLInputElement).value)"
                />
                <span class="np-offset-value">{{ Math.round(settings.lyricFontScale * 100) }}%</span>
              </div>
              <p class="np-more-preview" :style="{ fontSize: `${24 * settings.lyricFontScale}px` }">
                {{ t('player.font_preview') }}
              </p>
            </div>
          </template>

          <!-- 子视图：音频效果（对齐 Android PlaybackSoundSheet） -->
          <template v-else-if="moreSheetView === 'speed'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.audio_effects') }}</h4>
            </div>
            <!-- 播放速度 -->
            <div class="np-more-item">
              <div class="np-more-label">
                <span class="material-symbols-rounded" style="font-size: 18px">speed</span>
                {{ t('player.playback_speed') }}
              </div>
              <div class="np-more-speed-grid">
                <button
                  v-for="spd in [0.5, 0.75, 0.85, 0.9, 1, 1.1, 1.25, 1.5, 2]"
                  :key="spd"
                  class="np-more-speed-btn"
                  :class="{ active: player.playbackSpeed === spd }"
                  @click="player.setSpeed(spd)"
                >{{ spd }}x</button>
              </div>
            </div>
            <!-- 速度微调滑块 -->
            <div class="np-more-item">
              <div class="np-more-row">
                <input type="range" min="0.25" max="3" step="0.05"
                  :value="player.playbackSpeed"
                  class="np-more-slider"
                  @input="player.setSpeed(parseFloat(($event.target as HTMLInputElement).value))"
                />
                <span class="np-offset-value">{{ player.playbackSpeed.toFixed(2) }}x</span>
              </div>
            </div>
            <!-- 音调提示 -->
            <div class="np-audio-effect-note">
              <span class="material-symbols-rounded" style="font-size: 16px">info</span>
              <span>{{ t('player.pitch') }}: {{ player.playbackSpeed !== 1 ? (player.playbackSpeed > 1 ? '↑' : '↓') : '—' }}</span>
            </div>
          </template>

          <!-- 子视图：获取歌曲信息 -->
          <template v-else-if="moreSheetView === 'search'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.get_info') }}</h4>
            </div>
            <div class="np-more-search-bar">
              <input
                v-model="searchQuery"
                class="np-more-input"
                :placeholder="t('player.search_song')"
                @keydown.enter="doSearch"
              />
              <button class="np-more-search-btn" @click="doSearch" :disabled="isSearching">
                <span class="material-symbols-rounded">search</span>
              </button>
            </div>
            <div v-if="isSearching" class="np-more-status">{{ t('player.searching') }}</div>
            <div v-else-if="searchResults.length === 0 && searchQuery" class="np-more-status">{{ t('player.no_results') }}</div>
            <div class="np-more-search-results">
              <button
                v-for="(r, ri) in searchResults"
                :key="ri"
                class="np-more-search-item"
                @click="applySearchResult(r)"
              >
                <img v-if="r.cover_url" :src="r.cover_url" class="np-more-search-cover" referrerpolicy="no-referrer" />
                <div class="np-more-search-info">
                  <span class="np-more-search-title">{{ r.title }}</span>
                  <span class="np-more-search-artist">{{ r.artist }}</span>
                </div>
                <span class="np-more-search-source">{{ r.source }}</span>
              </button>
            </div>
          </template>

          <!-- 子视图：编辑歌曲信息 -->
          <template v-else-if="moreSheetView === 'editinfo'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.edit_info') }}</h4>
            </div>
            <div class="np-more-form">
              <label class="np-more-form-label">{{ t('player.song_title') }}</label>
              <input v-model="editTitle" class="np-more-input" />

              <label class="np-more-form-label">{{ t('player.artist_name') }}</label>
              <input v-model="editArtist" class="np-more-input" />

              <label class="np-more-form-label">{{ t('player.cover_url_label') }}</label>
              <input v-model="editCoverUrl" class="np-more-input" />

              <div class="np-more-form-actions">
                <button class="np-more-form-btn primary" @click="saveEditInfo">
                  <span class="material-symbols-rounded">check</span>
                  {{ t('common.save') }}
                </button>
                <button v-if="player.hasOriginalTrackInfo()" class="np-more-form-btn" @click="restoreInfo">
                  <span class="material-symbols-rounded">restore</span>
                  {{ t('player.restore_original') }}
                </button>
              </div>
            </div>
          </template>

          <!-- 子视图：音质切换 -->
          <template v-else-if="moreSheetView === 'quality'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.quality_switch') }}</h4>
            </div>
            <div v-if="currentSource === 'netease'" class="np-more-quality-list">
              <button
                v-for="q in neteaseQualities"
                :key="q.key"
                class="np-more-quality-item"
                :class="{ active: currentQualityKey() === q.key }"
                @click="switchQuality(q.key)"
              >
                <span>{{ t(q.label) }}</span>
                <span v-if="currentQualityKey() === q.key" class="material-symbols-rounded" style="font-size: 18px">check</span>
              </button>
            </div>
            <div v-else-if="currentSource === 'youtube'" class="np-more-quality-list">
              <button
                v-for="q in youtubeQualities"
                :key="q.key"
                class="np-more-quality-item"
                :class="{ active: currentQualityKey() === q.key }"
                @click="switchQuality(q.key)"
              >
                <span>{{ t(q.label) }}</span>
                <span v-if="currentQualityKey() === q.key" class="material-symbols-rounded" style="font-size: 18px">check</span>
              </button>
            </div>
            <div v-else class="np-more-status">{{ t('player.not_available') }}</div>
          </template>

          <!-- 子视图：歌词编辑器（对齐 Android LyricsEditorSheet） -->
          <template v-else-if="moreSheetView === 'lyrics-editor'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.lyrics_editor') }}</h4>
            </div>
            <div class="np-lyrics-editor">
              <textarea
                v-model="lyricsEditorText"
                class="np-lyrics-textarea"
                :placeholder="t('player.lyrics_editor_placeholder')"
                spellcheck="false"
              />
              <div class="np-more-form-actions">
                <button class="np-more-form-btn primary" @click="applyLyricsFromEditor">
                  <span class="material-symbols-rounded">check</span>
                  {{ t('player.lyrics_apply') }}
                </button>
                <button class="np-more-form-btn" @click="lyricsEditorText = ''; applyLyricsFromEditor()">
                  <span class="material-symbols-rounded">clear_all</span>
                  {{ t('player.lyrics_clear') }}
                </button>
              </div>
            </div>
          </template>

          <!-- 子视图：歌词填充（对齐 Android FillOptionsDialog） -->
          <template v-else-if="moreSheetView === 'lyrics-fill'">
            <div class="np-more-sub-header">
              <button class="np-more-back" @click="goBackToMain()">
                <span class="material-symbols-rounded">arrow_back</span>
              </button>
              <h4 class="np-more-title">{{ t('player.lyrics_fill') }}</h4>
            </div>
            <div class="np-more-search-bar">
              <input
                v-model="lyricFillQuery"
                class="np-more-input"
                :placeholder="t('player.search_song')"
                @keydown.enter="doLyricFillSearch"
              />
              <button class="np-more-search-btn" @click="doLyricFillSearch" :disabled="isLyricFilling">
                <span class="material-symbols-rounded">search</span>
              </button>
            </div>
            <div v-if="isLyricFilling" class="np-more-status">{{ t('player.searching') }}</div>
            <div v-else-if="lyricFillResults.length === 0 && lyricFillQuery" class="np-more-status">{{ t('player.no_results') }}</div>
            <div class="np-more-search-results">
              <button
                v-for="(r, ri) in lyricFillResults"
                :key="ri"
                class="np-more-search-item"
                @click="applyLyricFill(r)"
              >
                <img v-if="r.cover_url" :src="r.cover_url" class="np-more-search-cover" referrerpolicy="no-referrer" />
                <div class="np-more-search-info">
                  <span class="np-more-search-title">{{ r.title }}</span>
                  <span class="np-more-search-artist">{{ r.artist }}</span>
                </div>
                <span class="material-symbols-rounded" style="font-size: 18px; color: rgba(255,255,255,0.3)">lyrics</span>
              </button>
            </div>
          </template>

          </div>
          </Transition>

        </div>
      </div>
      </Transition>
    </Teleport>

    <!-- 右键菜单 -->
    <Teleport to="body">
      <div v-if="contextMenu.show" class="np-ctx-overlay" @click="closeContextMenu" @contextmenu.prevent="closeContextMenu">
        <div class="np-ctx-menu" :style="{ left: contextMenu.x + 'px', top: contextMenu.y + 'px' }">
          <template v-if="contextMenu.type === 'title'">
            <button class="np-ctx-item" @click="copyText(player.currentTrack?.title || '')">
              <span class="material-symbols-rounded" style="font-size: 20px">content_copy</span>
              <span>{{ t('player.copy_title') }}</span>
            </button>
          </template>
          <template v-else-if="contextMenu.type === 'artist'">
            <button class="np-ctx-item" @click="copyText(player.currentTrack?.artist || '')">
              <span class="material-symbols-rounded" style="font-size: 20px">content_copy</span>
              <span>{{ t('player.copy_artist') }}</span>
            </button>
          </template>
          <template v-else-if="contextMenu.type === 'cover'">
            <button class="np-ctx-item" @click="saveCoverArt">
              <span class="material-symbols-rounded" style="font-size: 20px">save</span>
              <span>{{ t('player.save_cover') }}</span>
            </button>
          </template>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped lang="scss">
.now-playing {
  position: fixed;
  inset: 0;
  z-index: 200;
  display: flex;
  flex-direction: column;
  // 确保完全不透明
  isolation: isolate;
}

// 纯色底层 — 由 accentBgStyle 动态控制颜色
.np-bg-solid {
  position: absolute;
  inset: 0;
  z-index: -1;
  transition: background 0.8s ease;
}

// 对齐 Android：无全局遮罩，对比度完全由文字颜色和动态配色保证
.np-scrim {
  display: none;
}

/* 顶栏 */
.np-header {
  position: relative;
  z-index: 2;
  display: flex;
  align-items: center;
  padding: 44px 24px 14px; /* 顶部 44px = 36px 顶栏 + 8px 间距 */
  gap: 12px;
  flex-shrink: 0;
}

.np-header-center {
  flex: 1;
  text-align: center;
  display: flex;
  flex-direction: column;
  gap: 1px;
}

.np-from-label {
  font-size: 11px;
  font-weight: 600;
  color: rgba(255,255,255,0.60);
  text-transform: uppercase;
  letter-spacing: 1.2px;
}

.np-from-name {
  font-size: 13px;
  font-weight: 600;
  color: rgba(255,255,255,0.87);
}

.np-icon-btn {
  width: 46px;
  height: 46px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(255,255,255,0.92);
  transition: background 150ms;

  &:hover { background: rgba(255,255,255,0.12); }
  .material-symbols-rounded { font-size: 28px; }
}

/* 双栏主体 — 五五分 */
.np-body {
  position: relative;
  z-index: 2;
  flex: 1;
  display: flex;
  padding: 0 0 20px;
  gap: 0;
  overflow: hidden;
  min-height: 0;

  &.np-body--no-header {
    padding-top: 64px; /* 56px 播放器顶栏 + 8px 间距 */
  }
}

.np-left {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 0 20px 0 40px;
}

.np-right {
  flex: 1;
  background: transparent;
  display: flex;
  align-items: stretch;
  overflow: hidden;
  padding: 0 40px 0 0;
}

/* 封面 */
.cover-wrap {
  position: relative;
  width: min(60%, 280px);
  aspect-ratio: 1;
  filter: drop-shadow(0 16px 48px rgba(0,0,0,0.5));
  flex-shrink: 0;
}

/* Card 模式（圆角矩形，对齐 Android） */
.cover-card {
  width: 100%;
  height: 100%;
  border-radius: 24px;
  background: linear-gradient(135deg, #2d2640 0%, #1a1724 50%, #1e1a2e 100%);
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  overflow: hidden;
  box-shadow: 0 16px 48px rgba(0,0,0,0.5);
}

.cover-card-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: 24px;
}

/* Disc 模式（黑胶唱片） */

.cover-disc {
  width: 100%;
  height: 100%;
  border-radius: 50%;
  background: conic-gradient(
    from 0deg,
    #2d2640,
    #1e1a2e,
    #252030,
    #2d2640
  );
  display: flex;
  align-items: center;
  justify-content: center;
  position: relative;
  will-change: transform;
}

.cover-inner {
  width: 78%;
  height: 78%;
  border-radius: 50%;
  background: linear-gradient(135deg,
    #2d2640 0%,
    #1a1724 50%,
    #1e1a2e 100%
  );
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  overflow: hidden;
}

.cover-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: 50%;
}

.cover-groove {
  position: absolute;
  width: 90%;
  height: 90%;
  border-radius: 50%;
  border: 1px solid rgba(255,255,255,0.04);
  pointer-events: none;
}

.cover-hole {
  position: absolute;
  width: 40px;
  height: 40px;
  border-radius: 50%;
  background: rgba(0,0,0,0.6);
  border: 2px solid rgba(255,255,255,0.06);
}

/* 曲目信息 */
.np-info {
  text-align: center;
  width: 100%;
  padding: 2px 12px 0;
  margin-bottom: 8px;
}

.np-title {
  font-size: 22px;
  font-weight: 700;
  color: white;
  line-height: 1.3;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  letter-spacing: -0.3px;
}

.np-artist {
  font-size: 14px;
  color: rgba(255,255,255,0.78);
  margin-top: 3px;
  font-weight: 500;
}

/* 进度条区域 */
.np-slider-area {
  width: 100%;
}

.np-time {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  font-weight: 600;
  color: rgba(255,255,255,0.78);
  padding: 2px 4px 0;
  font-variant-numeric: tabular-nums;
}

.np-audio-info {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  text-align: center;
  font-size: 10px;
  font-weight: 600;
  color: rgba(255,255,255,0.60);
  letter-spacing: 0.5px;
  margin-top: 0px;
  min-height: 18px;
}

.np-audio-codec {
  color: var(--np-primary-container, var(--md-primary-container, #E8DEF8));
  transition: color 0.6s ease;
}

.np-audio-detail {
  color: rgba(255,255,255,0.60);

  &.separated::before {
    content: '·';
    margin-right: 6px;
    color: rgba(255,255,255,0.42);
  }
}

.np-download-chip {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  padding: 2px 7px;
  border-radius: 999px;
  color: var(--np-primary-container, var(--md-primary-container, #E8DEF8));
  background: rgba(255,255,255,0.10);
  border: 1px solid rgba(255,255,255,0.14);

  .material-symbols-rounded {
    font-size: 12px;
  }
}

/* 控制栏 */
.np-controls {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  width: 100%;
  margin-top: 50px;
}

.ctrl-btn {
  width: 46px;
  height: 46px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(255,255,255,0.87);
  transition: color 150ms, background 150ms;

  &:hover { background: rgba(255,255,255,0.10); color: rgba(255,255,255,0.95); }
  &.active { color: var(--np-primary, white); }
  &:active { transform: scale(0.9); }
}

.play-btn {
  width: 52px;
  height: 52px;
  border-radius: var(--radius-full);
  background: var(--np-primary-container, white);
  color: var(--np-on-primary, rgb(20, 18, 24));
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 4px 24px rgba(0,0,0,0.35);
  margin: 0 4px;
  transition: transform 150ms var(--ease-standard), box-shadow 150ms, background 0.6s ease, color 0.6s ease;
  overflow: hidden;
  position: relative; /* 绝对定位子元素的容器 */

  &:hover { transform: scale(1.05); box-shadow: 0 6px 28px rgba(0,0,0,0.4); }
  &:active { transform: scale(0.94); }
}

.play-icon-inner {
  font-size: 28px;
  display: block;
  position: absolute; /* 重叠过渡，避免 out-in 空窗 */

  &.spinning {
    animation: np-spin 1s linear infinite;
    color: inherit;
  }
}

@keyframes np-spin { to { transform: rotate(360deg); } }

/* 播放/暂停图标切换动画（同时进出，绝对定位重叠） */
.play-icon-enter-active {
  transition: transform 200ms var(--ease-decelerate), opacity 150ms var(--ease-decelerate);
}
.play-icon-leave-active {
  transition: transform 120ms var(--ease-accelerate), opacity 80ms var(--ease-accelerate);
}
.play-icon-enter-from { transform: scale(0.5); opacity: 0; }
.play-icon-leave-to { transform: scale(0.5); opacity: 0; }

/* 工具栏 */
.np-toolbar {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 4px;
  width: 100%;
  margin-top: 4px;
}

.tool-btn {
  width: 42px;
  height: 42px;
  border-radius: var(--radius-full);
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(255,255,255,0.72);
  transition: color 150ms, background 150ms;

  .material-symbols-rounded { font-size: 20px; }

  &:hover { background: rgba(255,255,255,0.10); color: rgba(255,255,255,0.87); }
  &.active { color: var(--np-primary, var(--md-primary-container, #E8DEF8)); }
  &.disabled { opacity: 0.38; cursor: default; }
  &:active { transform: scale(0.88); }
}

// 收藏按钮活跃态保持红色（对齐 Android Color.Red.copy(alpha=0.6f)）
.fav-btn.active {
  color: rgba(239, 83, 80, 0.6) !important;
}

// 顶栏收藏按钮（对齐 Android 顶栏右侧）
.fav-header-btn {
  &.active {
    color: rgba(239, 83, 80, 0.6) !important;
  }
}

/* 来源徽章（对齐 Android PlaybackSourceBadge） */
.source-badge {
  position: absolute;
  bottom: 10px;
  right: 10px;
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 10px 4px 7px;
  border-radius: 20px;
  background: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(12px);
  border: 1px solid rgba(255, 255, 255, 0.1);
  animation: badge-in 0.35s cubic-bezier(0.22, 1, 0.36, 1);
}

.source-badge-icon {
  font-size: 14px;
  color: rgba(255, 255, 255, 0.7);
}

.source-badge-label {
  font-size: 11px;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.8);
  letter-spacing: 0.3px;
}

@keyframes badge-in {
  from { opacity: 0; transform: scale(0.7); }
  to { opacity: 1; transform: scale(1); }
}

/* 音量控制 */
.volume-wrap {
  position: relative;
}

.volume-popover {
  position: absolute;
  bottom: 46px;
  left: 50%;
  transform: translateX(-50%);
  background: rgba(30, 28, 34, 0.95);
  backdrop-filter: blur(20px);
  border-radius: 14px;
  padding: 16px 12px;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  box-shadow: 0 8px 32px rgba(0,0,0,0.5);
  border: 1px solid rgba(255,255,255,0.08);
  z-index: 10;
}

.volume-slider {
  writing-mode: vertical-lr;
  appearance: none;
  width: 4px;
  height: 100px;
  background: rgba(255,255,255,0.15);
  border-radius: 2px;
  outline: none;
  cursor: pointer;

  &::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: white;
    box-shadow: 0 1px 4px rgba(0,0,0,0.3);
    cursor: pointer;
  }
}

.volume-label {
  font-size: 11px;
  font-weight: 600;
  color: rgba(255,255,255,0.5);
  font-variant-numeric: tabular-nums;
}

/* 播放速度 / 音效面板 */
.speed-wrap, .sleep-wrap {
  position: relative;
}

.speed-label {
  font-size: 12px;
  font-weight: 700;
  letter-spacing: -0.3px;
}

.audiofx-popover {
  position: absolute;
  bottom: 46px;
  right: -40px;
  background: rgba(30, 28, 34, 0.96);
  backdrop-filter: blur(24px);
  border-radius: 16px;
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  box-shadow: 0 8px 40px rgba(0,0,0,0.6);
  border: 1px solid rgba(255,255,255,0.08);
  z-index: 10;
  min-width: 300px;
  max-height: 480px;
  overflow-y: auto;

  &::-webkit-scrollbar { width: 4px; }
  &::-webkit-scrollbar-thumb { background: rgba(255,255,255,0.15); border-radius: 2px; }
}

.audiofx-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.audiofx-section-header {
  font-size: 12px;
  font-weight: 600;
  color: rgba(255,255,255,0.5);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.audiofx-speed-grid, .audiofx-preset-row {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.audiofx-speed-grid .speed-option,
.audiofx-preset-row .speed-option {
  padding: 5px 10px;
  font-size: 12px;
  min-width: unset;
  flex: 0 0 auto;
}

.audiofx-slider-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.audiofx-slider-label {
  font-size: 10px;
  color: rgba(255,255,255,0.35);
  min-width: 28px;
  text-align: center;
}

.audiofx-slider-value {
  font-size: 11px;
  font-weight: 600;
  color: var(--md-primary, #D0BCFF);
  min-width: 42px;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.audiofx-slider {
  flex: 1;
  height: 4px;
  accent-color: var(--md-primary, #D0BCFF);
  cursor: pointer;
}

.audiofx-select {
  background: rgba(255,255,255,0.08);
  color: rgba(255,255,255,0.85);
  border: 1px solid rgba(255,255,255,0.1);
  border-radius: 8px;
  padding: 6px 10px;
  font-size: 12px;
  cursor: pointer;
  outline: none;

  option {
    background: #1e1c22;
    color: white;
  }
}

.audiofx-toggle {
  position: relative;
  display: inline-block;
  width: 34px;
  height: 18px;

  input { opacity: 0; width: 0; height: 0; }

  .audiofx-toggle-slider {
    position: absolute;
    cursor: pointer;
    inset: 0;
    background: rgba(255,255,255,0.15);
    border-radius: 18px;
    transition: 0.2s;

    &::before {
      content: '';
      position: absolute;
      height: 14px;
      width: 14px;
      left: 2px;
      bottom: 2px;
      background: white;
      border-radius: 50%;
      transition: 0.2s;
    }
  }

  input:checked + .audiofx-toggle-slider {
    background: var(--md-primary, #D0BCFF);
  }

  input:checked + .audiofx-toggle-slider::before {
    transform: translateX(16px);
  }
}

.audiofx-eq-bands {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 4px;
  padding: 4px 0;
}

.audiofx-eq-band {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  flex: 1;
}

.audiofx-eq-val {
  font-size: 10px;
  color: var(--md-primary, #D0BCFF);
  font-variant-numeric: tabular-nums;
  min-height: 14px;
}

.audiofx-eq-freq {
  font-size: 9px;
  color: rgba(255,255,255,0.35);
}

.audiofx-eq-slider {
  writing-mode: vertical-lr;
  direction: rtl;
  width: 4px;
  height: 80px;
  accent-color: var(--md-primary, #D0BCFF);
  cursor: pointer;
  appearance: slider-vertical;
}

.audiofx-note {
  font-size: 11px;
  color: rgba(255,255,255,0.3);
  font-style: italic;
  text-align: center;
  padding: 2px 0;
}

.audiofx-reset {
  width: 100%;
  text-align: center;
  color: #EF5350 !important;
  border-top: 1px solid rgba(255,255,255,0.06);
  margin-top: 2px;
  padding-top: 10px;
}

.speed-popover, .sleep-popover {
  position: absolute;
  bottom: 46px;
  left: 50%;
  transform: translateX(-50%);
  background: rgba(30, 28, 34, 0.95);
  backdrop-filter: blur(20px);
  border-radius: 14px;
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 2px;
  box-shadow: 0 8px 32px rgba(0,0,0,0.5);
  border: 1px solid rgba(255,255,255,0.08);
  z-index: 10;
  min-width: 120px;
}

.speed-option, .sleep-option {
  padding: 8px 16px;
  border: none;
  background: transparent;
  color: rgba(255,255,255,0.7);
  font-size: 13px;
  cursor: pointer;
  border-radius: 8px;
  text-align: left;
  white-space: nowrap;
  transition: all 0.15s;

  &:hover {
    background: rgba(255,255,255,0.1);
    color: white;
  }

  &.active {
    color: var(--md-primary, #D0BCFF);
    font-weight: 600;
  }

  &.cancel {
    color: #EF5350;
    border-top: 1px solid rgba(255,255,255,0.06);
    margin-top: 4px;
    padding-top: 10px;
  }
}

.sleep-badge {
  position: absolute;
  top: -2px;
  right: -4px;
  font-size: 9px;
  font-weight: 700;
  background: var(--md-primary, #D0BCFF);
  color: #1C1B1F;
  padding: 1px 4px;
  border-radius: 6px;
  font-variant-numeric: tabular-nums;
}

/* 歌词空状态 */
.lyrics-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  width: 100%;
  gap: 10px;
  color: rgba(255,255,255,0.18);
  font-size: 14px;
  font-weight: 500;
}
</style>

<!-- 右键菜单样式（Teleport 到 body，需 unscoped） -->
<style lang="scss">
.np-ctx-overlay {
  position: fixed;
  inset: 0;
  z-index: 500;
}

.np-ctx-menu {
  position: fixed;
  min-width: 200px;
  background: rgba(30, 28, 34, 0.95);
  backdrop-filter: blur(20px);
  border-radius: 12px;
  border: 1px solid rgba(255,255,255,0.08);
  padding: 4px 0;
  z-index: 501;
  box-shadow: 0 8px 32px rgba(0,0,0,0.5);
  animation: np-ctx-in 120ms ease-out;
}

@keyframes np-ctx-in {
  from { opacity: 0; transform: scale(0.95) translateY(-4px); }
  to { opacity: 1; transform: scale(1) translateY(0); }
}

.np-ctx-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 10px 16px;
  border: none;
  background: transparent;
  color: rgba(255,255,255,0.8);
  font-size: 14px;
  cursor: pointer;
  transition: background 0.1s;

  &:hover {
    background: rgba(255,255,255,0.08);
  }
}

/* 更多选项面板 — Overlay + Sheet 过渡 */
.more-sheet-enter-active {
  transition: opacity 280ms cubic-bezier(0.2, 0, 0, 1);
  .np-more-sheet {
    transition: opacity 280ms cubic-bezier(0.2, 0, 0, 1), transform 280ms cubic-bezier(0.2, 0, 0, 1);
  }
}
.more-sheet-leave-active {
  transition: opacity 200ms cubic-bezier(0.2, 0, 0, 1);
  .np-more-sheet {
    transition: opacity 200ms cubic-bezier(0.2, 0, 0, 1), transform 200ms cubic-bezier(0.2, 0, 0, 1);
  }
}
.more-sheet-enter-from,
.more-sheet-leave-to {
  opacity: 0;
  .np-more-sheet {
    opacity: 0;
    transform: scale(0.92) translateY(16px);
  }
}

/* 子视图滑动过渡 */
.slide-left-enter-active,
.slide-left-leave-active,
.slide-right-enter-active,
.slide-right-leave-active {
  transition: transform 180ms cubic-bezier(0.2, 0, 0, 1), opacity 180ms cubic-bezier(0.2, 0, 0, 1);
}
.slide-left-enter-from { transform: translateX(24px); opacity: 0; }
.slide-left-leave-to   { transform: translateX(-24px); opacity: 0; }
.slide-right-enter-from { transform: translateX(-24px); opacity: 0; }
.slide-right-leave-to   { transform: translateX(24px); opacity: 0; }

.np-more-overlay {
  position: fixed;
  inset: 0;
  z-index: 9000;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0,0,0,0.35);
  backdrop-filter: blur(4px);
}

.np-more-sheet {
  width: 380px;
  max-height: 80vh;
  overflow-y: auto;
  background: rgba(30, 28, 34, 0.95);
  backdrop-filter: blur(20px);
  border-radius: 24px;
  padding: 24px;
  box-shadow: 0 16px 48px rgba(0,0,0,0.5);
  border: 1px solid rgba(255,255,255,0.06);

  /* 自定义滚动条 */
  &::-webkit-scrollbar { width: 4px; }
  &::-webkit-scrollbar-track { background: transparent; }
  &::-webkit-scrollbar-thumb { background: rgba(255,255,255,0.12); border-radius: 2px; }
}

.np-more-sheet-content {
  min-height: 0;
}

.np-more-title {
  font-size: 18px;
  font-weight: 600;
  color: rgba(255,255,255,0.9);
  margin: 0 0 16px;
}

// 子视图 header（返回按钮 + 标题）
.np-more-sub-header {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
  padding-bottom: 12px;
  border-bottom: 1px solid rgba(255,255,255,0.06);

  .np-more-title { margin: 0; }
}

.np-more-back {
  width: 36px;
  height: 36px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  color: rgba(255,255,255,0.7);
  transition: background 0.15s;
  &:hover { background: rgba(255,255,255,0.08); }
}

// Android 风格列表项
.np-more-list-item {
  display: flex;
  align-items: center;
  gap: 16px;
  width: 100%;
  padding: 14px 4px;
  border: none;
  border-bottom: 1px solid rgba(255,255,255,0.05);
  background: transparent;
  color: rgba(255,255,255,0.85);
  cursor: pointer;
  border-radius: 0;
  transition: background 0.15s, transform 0.15s cubic-bezier(0.2, 0, 0, 1);

  &:last-child { border-bottom: none; }

  &:hover {
    background: rgba(255,255,255,0.06);
    transform: translateX(2px);
    .np-more-chevron { color: rgba(255,255,255,0.45) !important; }
  }

  > .material-symbols-rounded:first-child {
    font-size: 20px;
    color: rgba(255,255,255,0.6);
    flex-shrink: 0;
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    background: rgba(255,255,255,0.07);
  }
}

.np-more-list-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 2px;
  text-align: left;
}

.np-more-list-headline {
  font-size: 15px;
  font-weight: 500;
}

.np-more-list-desc {
  font-size: 12px;
  color: rgba(255,255,255,0.4);
}

.np-more-chevron {
  font-size: 20px !important;
  color: rgba(255,255,255,0.25) !important;
  transition: color 0.15s;
}

// 速度选择网格
.np-more-speed-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
  padding: 8px 0;
}

.np-more-speed-btn {
  padding: 12px;
  border: none;
  border-radius: 12px;
  background: rgba(255,255,255,0.06);
  color: rgba(255,255,255,0.7);
  font-size: 14px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s;

  &:hover { background: rgba(255,255,255,0.10); }
  &.active {
    background: var(--md-primary-container, #E8DEF8);
    color: var(--md-on-primary-container, #1D192B);
  }
}

.np-more-item {
  margin-bottom: 20px;
}

.np-more-label {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
  font-weight: 500;
  color: rgba(255,255,255,0.7);
  margin-bottom: 10px;
}

.np-more-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.np-more-slider {
  flex: 1;
  appearance: none;
  height: 4px;
  background: rgba(255,255,255,0.15);
  border-radius: 2px;
  outline: none;
  cursor: pointer;

  &::-webkit-slider-thumb {
    appearance: none;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--md-primary, #D0BCFF);
    cursor: pointer;
    box-shadow: 0 1px 4px rgba(0,0,0,0.3);
  }
}

.np-offset-value {
  font-size: 12px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
  color: rgba(255,255,255,0.5);
  min-width: 60px;
  text-align: right;

  &.positive { color: #66BB6A; }
  &.negative { color: #EF5350; }
}

.np-more-preview {
  margin-top: 8px;
  font-weight: 700;
  color: rgba(255,255,255,0.5);
  line-height: 1.4;
  transition: font-size 0.15s;
}

// 搜索栏
.np-more-search-bar {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}

.np-more-input {
  flex: 1;
  padding: 10px 14px;
  border: 1px solid rgba(255,255,255,0.12);
  border-radius: 12px;
  background: rgba(255,255,255,0.06);
  color: white;
  font-size: 14px;
  outline: none;
  transition: border-color 0.15s;

  &:focus { border-color: rgba(255,255,255,0.3); }
  &::placeholder { color: rgba(255,255,255,0.3); }
}

.np-more-search-btn {
  width: 42px;
  height: 42px;
  border-radius: 12px;
  background: rgba(255,255,255,0.08);
  color: rgba(255,255,255,0.7);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  transition: background 0.15s;

  &:hover { background: rgba(255,255,255,0.14); }
  &:disabled { opacity: 0.4; }
}

.np-more-status {
  text-align: center;
  color: rgba(255,255,255,0.35);
  font-size: 13px;
  padding: 16px 0;
}

// 搜索结果列表
.np-more-search-results {
  max-height: 300px;
  overflow-y: auto;
  &::-webkit-scrollbar { display: none; }
}

.np-more-search-item {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 10px 4px;
  border: none;
  background: transparent;
  color: rgba(255,255,255,0.85);
  cursor: pointer;
  border-radius: 10px;
  transition: background 0.15s;
  text-align: left;

  &:hover { background: rgba(255,255,255,0.06); }
}

.np-more-search-cover {
  width: 40px;
  height: 40px;
  border-radius: 6px;
  object-fit: cover;
  flex-shrink: 0;
  background: rgba(255,255,255,0.06);
}

.np-more-search-info {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.np-more-search-title {
  font-size: 14px;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.np-more-search-artist {
  font-size: 12px;
  color: rgba(255,255,255,0.4);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.np-more-search-source {
  font-size: 10px;
  font-weight: 600;
  color: rgba(255,255,255,0.3);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  flex-shrink: 0;
}

// 编辑表单
.np-more-form {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.np-more-form-label {
  font-size: 12px;
  font-weight: 600;
  color: rgba(255,255,255,0.45);
  margin-top: 4px;
}

.np-more-form-actions {
  display: flex;
  gap: 8px;
  margin-top: 12px;
}

.np-more-form-btn {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 10px;
  border: none;
  border-radius: 12px;
  background: rgba(255,255,255,0.08);
  color: rgba(255,255,255,0.7);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.15s;

  .material-symbols-rounded { font-size: 18px; }

  &:hover { background: rgba(255,255,255,0.12); }

  &.primary {
    background: var(--md-primary-container, #E8DEF8);
    color: var(--md-on-primary-container, #1D192B);
    &:hover { opacity: 0.9; }
  }
}

// 音质列表
.np-more-quality-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

// 歌词编辑器
.np-lyrics-editor {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.np-lyrics-textarea {
  width: 100%;
  min-height: 200px;
  max-height: 320px;
  padding: 12px 14px;
  border: 1px solid rgba(255,255,255,0.12);
  border-radius: 12px;
  background: rgba(255,255,255,0.04);
  color: rgba(255,255,255,0.85);
  font-size: 13px;
  font-family: 'Cascadia Code', 'JetBrains Mono', 'Fira Code', monospace;
  line-height: 1.6;
  resize: vertical;
  outline: none;
  transition: border-color 0.15s;

  &:focus { border-color: rgba(255,255,255,0.3); }
  &::placeholder { color: rgba(255,255,255,0.2); }
  &::-webkit-scrollbar { width: 4px; }
  &::-webkit-scrollbar-thumb {
    background: rgba(255,255,255,0.12);
    border-radius: 2px;
  }
}

// 音频效果提示
.np-audio-effect-note {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 10px 12px;
  border-radius: 10px;
  background: rgba(255,255,255,0.04);
  color: rgba(255,255,255,0.35);
  font-size: 12px;
  font-weight: 500;
}

.np-more-quality-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  padding: 14px 12px;
  border: none;
  background: transparent;
  color: rgba(255,255,255,0.7);
  font-size: 15px;
  cursor: pointer;
  border-radius: 10px;
  transition: background 0.15s;

  &:hover { background: rgba(255,255,255,0.06); }

  &.active {
    color: var(--md-primary-container, #E8DEF8);
    font-weight: 600;
  }
}
</style>
