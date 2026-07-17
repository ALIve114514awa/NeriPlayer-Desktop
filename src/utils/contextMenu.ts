export interface ContextMenuItemOptions {
  id?: string
  icon?: string
  disabled?: boolean
  danger?: boolean
  shortcut?: string
}

export interface ContextMenuActionItem extends ContextMenuItemOptions {
  label: string
  separator?: false
}

export interface ContextMenuSeparatorItem {
  id?: string
  separator: true
}

export type ContextMenuItem = ContextMenuActionItem | ContextMenuSeparatorItem

export interface ContextMenuPosition {
  x: number
  y: number
}

export interface ContextMenuPositionOptions {
  viewportWidth?: number
  viewportHeight?: number
  margin?: number
}

const DEFAULT_CONTEXT_MENU_MARGIN = 8

function getViewportWidth() {
  return typeof window === 'undefined' ? 0 : window.innerWidth
}

function getViewportHeight() {
  return typeof window === 'undefined' ? 0 : window.innerHeight
}

function finiteOr(value: number | undefined, fallback: number) {
  return value !== undefined && Number.isFinite(value) ? value : fallback
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max)
}

export function clampContextMenuPosition(
  x: number,
  y: number,
  menuWidth = 0,
  menuHeight = 0,
  options: ContextMenuPositionOptions = {},
): ContextMenuPosition {
  const viewportWidth = Math.max(0, finiteOr(options.viewportWidth, getViewportWidth()))
  const viewportHeight = Math.max(0, finiteOr(options.viewportHeight, getViewportHeight()))
  const width = Math.max(0, finiteOr(menuWidth, 0))
  const height = Math.max(0, finiteOr(menuHeight, 0))
  const margin = Math.max(0, finiteOr(options.margin, DEFAULT_CONTEXT_MENU_MARGIN))
  const maxX = Math.max(margin, viewportWidth - width - margin)
  const maxY = Math.max(margin, viewportHeight - height - margin)

  return {
    x: clamp(finiteOr(x, margin), margin, maxX),
    y: clamp(finiteOr(y, margin), margin, maxY),
  }
}

export function createContextMenuItem(
  label: string,
  options: ContextMenuItemOptions = {},
): ContextMenuActionItem {
  return { label, ...options }
}

export function createContextMenuSeparator(id?: string): ContextMenuSeparatorItem {
  return id === undefined ? { separator: true } : { id, separator: true }
}

export function isContextMenuSeparator(
  item: ContextMenuItem,
): item is ContextMenuSeparatorItem {
  return item.separator === true
}
