<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from 'vue'

export interface SelectOption {
  value: string
  label: string
}

const props = withDefaults(defineProps<{
  modelValue: string
  options: SelectOption[]
  surface?: 'default' | 'dark'
}>(), {
  surface: 'default',
})

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const isOpen = ref(false)
const triggerRef = ref<HTMLElement | null>(null)
const menuRef = ref<HTMLElement | null>(null)
const menuStyle = ref<Record<string, string>>({})

const selectedLabel = computed(() => {
  const opt = props.options.find(o => o.value === props.modelValue)
  return opt?.label ?? props.modelValue
})
const isDarkSurface = computed(() => props.surface === 'dark')

function toggle() {
  if (isOpen.value) {
    isOpen.value = false
    return
  }
  updatePosition()
  isOpen.value = true
}

function select(value: string) {
  emit('update:modelValue', value)
  isOpen.value = false
}

function updatePosition() {
  if (!triggerRef.value) return
  const rect = triggerRef.value.getBoundingClientRect()
  const spaceBelow = window.innerHeight - rect.bottom
  const spaceAbove = rect.top
  const menuHeight = Math.min(props.options.length * 40 + 16, 280)

  // 底部空间不足时向上弹出
  if (spaceBelow < menuHeight && spaceAbove > spaceBelow) {
    menuStyle.value = {
      left: `${rect.left}px`,
      bottom: `${window.innerHeight - rect.top + 4}px`,
      minWidth: `${rect.width}px`,
      maxHeight: `${Math.min(spaceAbove - 16, 280)}px`,
    }
  } else {
    menuStyle.value = {
      left: `${rect.left}px`,
      top: `${rect.bottom + 4}px`,
      minWidth: `${rect.width}px`,
      maxHeight: `${Math.min(spaceBelow - 16, 280)}px`,
    }
  }
}

function handleClickOutside(e: MouseEvent) {
  if (!isOpen.value) return
  const target = e.target as HTMLElement
  if (triggerRef.value?.contains(target)) return
  if (menuRef.value?.contains(target)) return
  isOpen.value = false
}

onMounted(() => {
  document.addEventListener('pointerdown', handleClickOutside, true)
})

onUnmounted(() => {
  document.removeEventListener('pointerdown', handleClickOutside, true)
})

watch(isOpen, async (open) => {
  if (open) {
    await nextTick()
    updatePosition()
  }
})
</script>

<template>
  <div class="custom-select" :class="{ 'custom-select--dark': isDarkSurface }" ref="triggerRef">
    <button class="custom-select-trigger" @click="toggle" type="button">
      <span class="custom-select-label">{{ selectedLabel }}</span>
      <span class="material-symbols-rounded custom-select-arrow" :class="{ open: isOpen }">expand_more</span>
    </button>

    <Teleport to="body">
      <Transition name="cs-menu">
        <div
          v-if="isOpen"
          ref="menuRef"
          class="custom-select-menu"
          :class="{ 'custom-select-menu--dark': isDarkSurface }"
          :style="menuStyle"
        >
          <button
            v-for="opt in options"
            :key="opt.value"
            class="custom-select-option"
            :class="{ active: opt.value === modelValue }"
            @click="select(opt.value)"
            type="button"
          >
            {{ opt.label }}
          </button>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<style scoped lang="scss">
.custom-select {
  position: relative;
  display: inline-flex;
}

.custom-select-trigger {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 8px 12px;
  border-radius: var(--radius-md);
  background: var(--md-surface-container-high);
  color: var(--md-on-surface);
  font-size: 13px;
  font-weight: 500;
  font-family: inherit;
  cursor: pointer;
  border: 1px solid var(--md-outline-variant);
  transition: background 150ms, border-color 150ms;
  min-width: 0;
  width: 100%;

  &:hover {
    background: var(--md-surface-container-highest);
    border-color: var(--md-outline);
  }
}

.custom-select-label {
  flex: 1;
  text-align: left;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.custom-select-arrow {
  font-size: 18px !important;
  opacity: 0.6;
  transition: transform 200ms ease;

  &.open {
    transform: rotate(180deg);
  }
}

.custom-select--dark {
  .custom-select-trigger {
    background: rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.88);
    border-color: rgba(255, 255, 255, 0.1);

    &:hover {
      background: rgba(255, 255, 255, 0.11);
      border-color: rgba(255, 255, 255, 0.18);
    }
  }

  .custom-select-arrow {
    opacity: 0.68;
  }
}
</style>

<style lang="scss">
/* 全局样式（Teleport 到 body 的菜单） */
.custom-select-menu {
  position: fixed;
  z-index: 500;
  background: var(--md-surface-container-high);
  border: 1px solid var(--md-outline-variant);
  border-radius: var(--radius-md);
  padding: 4px;
  overflow-y: auto;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.28), 0 2px 8px rgba(0, 0, 0, 0.12);
  transform-origin: top center;
}

.custom-select-option {
  display: block;
  width: 100%;
  padding: 8px 14px;
  border: none;
  background: none;
  color: var(--md-on-surface);
  font-size: 13px;
  font-weight: 500;
  font-family: inherit;
  text-align: left;
  cursor: pointer;
  border-radius: var(--radius-sm);
  transition: background 120ms;

  &:hover {
    background: var(--md-surface-container-highest);
  }

  &.active {
    color: var(--md-primary);
    background: color-mix(in srgb, var(--md-primary) 10%, transparent);
  }
}

.custom-select-menu--dark {
  background: rgba(22, 21, 27, 0.98);
  border-color: rgba(255, 255, 255, 0.1);
  box-shadow: 0 16px 42px rgba(0, 0, 0, 0.46), 0 0 0 1px rgba(255, 255, 255, 0.03) inset;
  backdrop-filter: blur(20px);

  .custom-select-option {
    color: rgba(255, 255, 255, 0.78);

    &:hover {
      background: rgba(255, 255, 255, 0.1);
      color: rgba(255, 255, 255, 0.94);
    }

    &.active {
      color: rgba(185, 225, 255, 0.96);
      background: rgba(120, 190, 255, 0.16);
    }
  }
}

/* 入场/退场动画 */
.cs-menu-enter-active {
  transition: opacity 120ms ease, transform 120ms cubic-bezier(0.05, 0.7, 0.1, 1);
}
.cs-menu-leave-active {
  transition: opacity 80ms ease, transform 80ms ease;
}
.cs-menu-enter-from {
  opacity: 0;
  transform: scaleY(0.92);
}
.cs-menu-leave-to {
  opacity: 0;
  transform: scaleY(0.96);
}
</style>
