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
