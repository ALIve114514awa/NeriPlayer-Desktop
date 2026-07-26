// 全局键盘快捷键
//
// 设计约束：
// - 输入框获得焦点时一律让位，避免打字被当成命令
// - 有原生对话框/弹层时只保留 Escape，其余交给弹层自己处理
// - macOS 用 ⌘，Windows / Linux 用 Ctrl（见 platform.ts）
// - 所有处理过的按键都 preventDefault，避免页面被空格滚动

import {
  hasNoModifier,
  hasOnlyPrimaryModifier,
  isEditableTarget,
  primaryModifierLabel,
} from './platform'

export interface ShortcutActions {
  togglePlay: () => void
  next: () => void
  previous: () => void
  seekBy: (deltaMs: number) => void
  adjustVolume: (delta: number) => void
  toggleMute: () => void
  toggleNowPlaying: () => void
  closeOverlay: () => boolean
  focusSearch: () => void
  toggleShuffle: () => void
  cycleRepeat: () => void
  isOverlayOpen: () => boolean
}

export interface ShortcutDescriptor {
  id: string
  keys: string[]
}

const SEEK_STEP_MS = 5_000
const SEEK_STEP_LARGE_MS = 30_000
const VOLUME_STEP = 0.05

/// 快捷键清单，供设置页展示；标签在 i18n 的 shortcuts.* 下
export function shortcutDescriptors(): ShortcutDescriptor[] {
  const mod = primaryModifierLabel
  return [
    { id: 'toggle_play', keys: ['Space'] },
    { id: 'next', keys: [`${mod}`, '→'] },
    { id: 'previous', keys: [`${mod}`, '←'] },
    { id: 'seek_forward', keys: ['→'] },
    { id: 'seek_back', keys: ['←'] },
    { id: 'seek_forward_large', keys: ['Shift', '→'] },
    { id: 'seek_back_large', keys: ['Shift', '←'] },
    { id: 'volume_up', keys: ['↑'] },
    { id: 'volume_down', keys: ['↓'] },
    { id: 'mute', keys: ['M'] },
    { id: 'now_playing', keys: [`${mod}`, 'P'] },
    { id: 'search', keys: [`${mod}`, 'F'] },
    { id: 'shuffle', keys: ['S'] },
    { id: 'repeat', keys: ['R'] },
    { id: 'close', keys: ['Esc'] },
  ]
}

export function installGlobalShortcuts(actions: ShortcutActions): () => void {
  function handle(event: KeyboardEvent) {
    if (event.defaultPrevented || event.repeat) return

    // Escape 是唯一在输入态也要响应的键：先让输入框失焦，再关弹层
    if (event.key === 'Escape') {
      if (isEditableTarget(event.target)) {
        ;(event.target as HTMLElement).blur()
        event.preventDefault()
        return
      }
      if (actions.closeOverlay()) event.preventDefault()
      return
    }

    if (isEditableTarget(event.target)) return

    // 弹层打开时不劫持全局播放键，交给弹层内部交互
    if (actions.isOverlayOpen() && event.key !== ' ') return

    if (hasOnlyPrimaryModifier(event)) {
      switch (event.key.toLowerCase()) {
        case 'arrowright':
          actions.next()
          break
        case 'arrowleft':
          actions.previous()
          break
        case 'f':
          actions.focusSearch()
          break
        case 'p':
          actions.toggleNowPlaying()
          break
        default:
          return
      }
      event.preventDefault()
      return
    }

    if (event.altKey || event.metaKey || event.ctrlKey) return

    switch (event.key) {
      case ' ':
      case 'Spacebar':
        actions.togglePlay()
        break
      case 'ArrowRight':
        actions.seekBy(event.shiftKey ? SEEK_STEP_LARGE_MS : SEEK_STEP_MS)
        break
      case 'ArrowLeft':
        actions.seekBy(event.shiftKey ? -SEEK_STEP_LARGE_MS : -SEEK_STEP_MS)
        break
      case 'ArrowUp':
        actions.adjustVolume(VOLUME_STEP)
        break
      case 'ArrowDown':
        actions.adjustVolume(-VOLUME_STEP)
        break
      default: {
        if (!hasNoModifier(event) && !event.shiftKey) return
        switch (event.key.toLowerCase()) {
          case 'm':
            actions.toggleMute()
            break
          case 's':
            actions.toggleShuffle()
            break
          case 'r':
            actions.cycleRepeat()
            break
          default:
            return
        }
      }
    }
    event.preventDefault()
  }

  window.addEventListener('keydown', handle)
  return () => window.removeEventListener('keydown', handle)
}
