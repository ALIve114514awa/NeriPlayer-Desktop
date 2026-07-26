// 平台相关的快捷键适配
//
// macOS 用 Command，Windows / Linux 用 Ctrl。三端统一走 primaryModifier()
// 判断，避免各处散落 metaKey/ctrlKey 分支。

export type DesktopPlatform = 'mac' | 'windows' | 'linux'

function rawPlatform(): string {
  const data = (navigator as unknown as { userAgentData?: { platform?: string } }).userAgentData
  return (data?.platform || navigator.platform || navigator.userAgent || '').toLowerCase()
}

export function detectPlatform(): DesktopPlatform {
  const value = rawPlatform()
  if (/mac|iphone|ipad|darwin/.test(value)) return 'mac'
  if (/win/.test(value)) return 'windows'
  return 'linux'
}

export const isMacPlatform = detectPlatform() === 'mac'

/// 主修饰键是否按下：macOS 为 ⌘，其余为 Ctrl
export function hasPrimaryModifier(event: KeyboardEvent): boolean {
  return isMacPlatform ? event.metaKey : event.ctrlKey
}

/// 除主修饰键外没有别的修饰键，避免和系统/浏览器快捷键抢
export function hasOnlyPrimaryModifier(event: KeyboardEvent): boolean {
  if (!hasPrimaryModifier(event)) return false
  if (event.altKey) return false
  return isMacPlatform ? !event.ctrlKey : !event.metaKey
}

export function hasNoModifier(event: KeyboardEvent): boolean {
  return !event.metaKey && !event.ctrlKey && !event.altKey
}

/// 展示用的修饰键符号
export const primaryModifierLabel = isMacPlatform ? '⌘' : 'Ctrl'

/// 焦点是否落在可输入元素上，是则让位给输入
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  const tag = target.tagName
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true
  if (tag !== 'INPUT') return false
  const type = (target as HTMLInputElement).type
  // 这些 input 自己不消费方向键/空格，可以放行给全局快捷键
  return !['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color'].includes(type)
}
