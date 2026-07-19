import type { PaletteResult } from '@/utils/paletteExtractor'

export function shouldShowDynamicBackground(
  hasPlaybackSession: boolean,
  dynamicBackgroundEnabled: boolean,
  paletteResult: PaletteResult | null,
): boolean {
  return hasPlaybackSession && dynamicBackgroundEnabled && paletteResult !== null
}
