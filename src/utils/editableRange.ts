const RANGE_EPSILON = 1e-9

export function formatEditableNumber(value: number, scale = 1): string {
  const displayedValue = value * scale
  if (!Number.isFinite(displayedValue)) return ''
  if (Object.is(displayedValue, -0)) return '0'

  return trimTrailingZeros(displayedValue.toFixed(8))
}

export function parseEditableNumber(
  input: string | number,
  min: number,
  max: number,
  scale = 1,
): number | null {
  const text = typeof input === 'number' ? String(input) : input.trim()
  if (!text || !Number.isFinite(scale) || scale <= 0) return null

  const displayedValue = Number(text)
  if (!Number.isFinite(displayedValue)) return null

  const value = displayedValue / scale
  if (!Number.isFinite(value)) return null
  if (value < min - RANGE_EPSILON || value > max + RANGE_EPSILON) return null

  return normalizeFloatingPoint(value)
}

export function normalizeFloatingPoint(value: number): number {
  return Number(value.toFixed(8))
}

function trimTrailingZeros(value: string): string {
  return value.replace(/(\.\d*?[1-9])0+$|\.0+$/, '$1').replace(/\.$/, '')
}
