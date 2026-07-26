/** 网易云封面字段兼容：picUrl / blurPicUrl / coverImgUrl / pic 数字 ID */
export function resolveNeteaseCover(...candidates: unknown[]): string {
  for (const raw of candidates) {
    if (raw == null) continue
    if (typeof raw === 'string') {
      const value = raw.trim()
      if (!value) continue
      if (value.startsWith('http://') || value.startsWith('https://') || value.startsWith('//')) {
        return value.startsWith('//') ? `https:${value}` : value
      }
      // 少数接口只返回 pic 哈希/数字串
      if (/^[A-Za-z0-9_-]+$/.test(value) && value.length >= 8) {
        return `https://p1.music.126.net/${value}.jpg`
      }
      continue
    }
    if (typeof raw === 'number' && Number.isFinite(raw) && raw > 0) {
      // 纯数字 pic 字段无法稳定还原 URL，跳过
      continue
    }
  }
  return ''
}
