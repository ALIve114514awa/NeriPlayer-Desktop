/**
 * 统一日志工具
 */

type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error'

/** 转发到后端插件的函数签名 */
type ForwardFn = (message: string, options?: { keyValues?: Record<string, string> }) => Promise<void>

interface PluginLogModule {
  trace: ForwardFn
  debug: ForwardFn
  info: ForwardFn
  warn: ForwardFn
  error: ForwardFn
}

// 惰性加载的插件模块缓存；null 表示尚未尝试，false 表示加载失败已放弃
let pluginPromise: Promise<PluginLogModule | null> | null = null

function loadPlugin(): Promise<PluginLogModule | null> {
  if (!pluginPromise) {
    pluginPromise = import('@tauri-apps/plugin-log')
      .then((mod) => mod as unknown as PluginLogModule)
      .catch(() => null)
  }
  return pluginPromise
}

/** 本地时间戳，毫秒精度，与后端格式对齐 */
function timestamp(): string {
  const d = new Date()
  const pad = (n: number, w = 2) => String(n).padStart(w, '0')
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`
  )
}

/** 把结构化参数拼成可读字符串，供转发到后端时保留上下文 */
function stringifyArgs(args: unknown[]): string {
  if (args.length === 0) return ''
  return args
    .map((arg) => {
      if (typeof arg === 'string') return arg
      if (arg instanceof Error) return arg.stack || arg.message
      try {
        return JSON.stringify(arg)
      } catch {
        return String(arg)
      }
    })
    .join(' ')
}

const consoleMethod: Record<LogLevel, (...data: unknown[]) => void> = {
  trace: console.debug.bind(console),
  debug: console.debug.bind(console),
  info: console.info.bind(console),
  warn: console.warn.bind(console),
  error: console.error.bind(console),
}

// 控制台分级配色（%c 样式），让 DevTools 日志按级别/作用域快速区分
const LEVEL_STYLE: Record<LogLevel, string> = {
  trace: 'color:#9e9e9e',
  debug: 'color:#26c6da',
  info: 'color:#66bb6a;font-weight:600',
  warn: 'color:#ffa726;font-weight:600',
  error: 'color:#ef5350;font-weight:700',
}
const TS_STYLE = 'color:#888'
const SCOPE_STYLE = 'color:#4dd0e1;font-weight:600'
const RESET_STYLE = 'color:inherit;font-weight:normal'

export interface Logger {
  trace: (...args: unknown[]) => void
  debug: (...args: unknown[]) => void
  info: (...args: unknown[]) => void
  warn: (...args: unknown[]) => void
  error: (...args: unknown[]) => void
}

/**
 * 创建带固定作用域的 logger。
 *
 * @param scope 作用域标签（如 `player`、`sync`、`now-playing`），
 *              作为 TAG 出现在每条日志中。
 */
export function createLogger(scope: string): Logger {
  const emit = (level: LogLevel, args: unknown[]) => {
    // 控制台用 %c 分级配色；原始参数原样追加，保留对象可展开检视
    consoleMethod[level](
      `%c${timestamp()} %c[${scope}] %c${level.toUpperCase()}%c`,
      TS_STYLE, SCOPE_STYLE, LEVEL_STYLE[level], RESET_STYLE,
      ...args,
    )

    // 转发到后端统一日志；作用域拼进消息，失败静默降级
    const message = `[${scope}] ${stringifyArgs(args)}`
    void loadPlugin()
      .then((plugin) => plugin?.[level](message))
      .catch(() => {
        // 转发失败不影响业务，控制台已有记录
      })
  }

  return {
    trace: (...args) => emit('trace', args),
    debug: (...args) => emit('debug', args),
    info: (...args) => emit('info', args),
    warn: (...args) => emit('warn', args),
    error: (...args) => emit('error', args),
  }
}
