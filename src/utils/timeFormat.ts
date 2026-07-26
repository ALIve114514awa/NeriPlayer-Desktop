/**
 * 播放时间格式化（m:ss）
 * 从 MiniPlayer / player store 中提取的共用实现，避免各处重复
 */
export function formatTimeMs(ms: number): string {
  const totalSeconds = Math.floor(Math.max(0, ms) / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${minutes}:${seconds.toString().padStart(2, '0')}`
}

/**
 * 列表行曲目时长格式化
 * 数据源缺失时长（0 / undefined / NaN）时显示占位 "--:--" 而非误导性的 "0:00"
 */
export function formatTrackDuration(ms?: number | null): string {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) return '--:--'
  return formatTimeMs(ms)
}
