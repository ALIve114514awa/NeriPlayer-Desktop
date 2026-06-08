// 深色/浅色模式管理 + 圆形扩散过渡动画
// 参考 Android 端 pending state 模式：视觉切换与持久化解耦，消除卡顿
import { applyThemeColor, applyThemeColorVisual, getSavedThemeColor } from './themeColor'

export type ThemeMode = 'system' | 'dark' | 'light'

let currentMode: ThemeMode = 'system'

export function getThemeMode(): ThemeMode {
  return (localStorage.getItem('theme-mode') as ThemeMode) || 'system'
}

function resolvedIsDark(mode: ThemeMode): boolean {
  if (mode === 'dark') return true
  if (mode === 'light') return false
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

/**
 * 仅做视觉切换（不写 localStorage），供 startViewTransition 回调使用
 * 将 isDark 预计算后传入 applyThemeColorVisual，避免回调内
 * classList 写后读触发强制 reflow
 */
function applyThemeVisual(mode: ThemeMode) {
  currentMode = mode
  const dark = resolvedIsDark(mode)
  document.documentElement.classList.toggle('light-theme', !dark)
  document.documentElement.classList.toggle('dark-theme', dark)
  // 直接传入 dark 状态，跳过 classList 二次读取
  applyThemeColorVisual(getSavedThemeColor(), dark)
}

/** 无动画直接应用（含持久化，用于初始化和 fallback） */
export function applyTheme(mode: ThemeMode) {
  applyThemeVisual(mode)
  localStorage.setItem('theme-mode', mode)
}

/**
 * 带圆形扩散动画的主题切换
 * 核心优化：transition 回调内只做纯视觉 DOM 操作，
 * localStorage 写入移到 microtask 异步执行
 */
export async function switchThemeWithRipple(mode: ThemeMode, x: number, y: number) {
  // 如果浏览器不支持 View Transition，直接切换
  if (!(document as any).startViewTransition) {
    applyTheme(mode)
    return
  }

  // 计算圆形遮罩最大半径（覆盖整个屏幕）
  const maxRadius = Math.hypot(
    Math.max(x, window.innerWidth - x),
    Math.max(y, window.innerHeight - y),
  )

  const transition = (document as any).startViewTransition(() => {
    // 仅视觉切换，不含 localStorage 写入
    applyThemeVisual(mode)
  })

  // 异步持久化，不阻塞动画关键路径
  queueMicrotask(() => localStorage.setItem('theme-mode', mode))

  try {
    await transition.ready

    // 新视图从 clip-path: circle(0) 扩散到 circle(maxRadius)
    document.documentElement.animate(
      {
        clipPath: [
          `circle(0px at ${x}px ${y}px)`,
          `circle(${maxRadius}px at ${x}px ${y}px)`,
        ],
      },
      {
        duration: 500,
        easing: 'cubic-bezier(0.2, 0, 0, 1)',
        pseudoElement: '::view-transition-new(root)',
      },
    )
  } catch {
    // transition 被中断也没关系
  }
}

// 初始化
export function initTheme() {
  const mode = getThemeMode()
  applyTheme(mode)
  // 初始化主题色
  applyThemeColor(getSavedThemeColor())

  // 监听系统主题变化
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (getThemeMode() === 'system') {
      applyTheme('system')
    }
  })
}
