<script setup lang="ts">
import { useToastStore } from '@/stores/toast'
import type { ToastMessage } from '@/stores/toast'
import { ref } from 'vue'

const toast = useToastStore()

// 追踪正在退出动画的 ID
const leaving = ref<Set<number>>(new Set())

function onDismiss(id: number) {
  leaving.value.add(id)
  setTimeout(() => {
    leaving.value.delete(id)
    toast.dismiss(id)
  }, 200)
}

function onAction(msg: ToastMessage) {
  if (!msg.action) return
  void Promise.resolve(msg.action.handler()).catch((e) => {
    console.error('Toast action failed:', e)
  })
  onDismiss(msg.id)
}
</script>

<template>
  <Teleport to="body">
    <div class="toast-container" v-if="toast.messages.length > 0">
      <div
        v-for="msg in toast.messages"
        :key="msg.id"
        class="toast-item"
        :class="[msg.type, { 'toast-leaving': leaving.has(msg.id) }]"
        @click="onDismiss(msg.id)"
      >
        <span class="material-symbols-rounded toast-icon">
          {{ msg.type === 'success' ? 'check_circle' : msg.type === 'error' ? 'error' : 'info' }}
        </span>
        <span class="toast-text">{{ msg.text }}</span>
        <button
          v-if="msg.action"
          class="toast-action"
          type="button"
          @click.stop="onAction(msg)"
        >
          {{ msg.action.label }}
        </button>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.toast-container {
  position: fixed;
  bottom: 100px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 10000;
  display: flex;
  flex-direction: column-reverse;
  gap: 8px;
  pointer-events: none;
}

.toast-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 20px;
  border-radius: 12px;
  background: var(--md-inverse-surface, #313033);
  color: var(--md-inverse-on-surface, #F4EFF4);
  font-size: 14px;
  font-weight: 450;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
  pointer-events: auto;
  cursor: pointer;
  animation: toast-in 250ms cubic-bezier(0.2, 0, 0, 1) forwards;
  min-width: 200px;
  max-width: min(460px, calc(100vw - 32px));
  user-select: none;
  backdrop-filter: blur(18px);
}

.toast-item.toast-leaving {
  animation: toast-out 200ms cubic-bezier(0.2, 0, 0, 1) forwards;
}

.toast-icon {
  font-size: 20px;
  flex-shrink: 0;
}

.toast-item.success .toast-icon {
  color: #7CDB8A;
}

.toast-item.error .toast-icon {
  color: #FFB4AB;
}

.toast-item.info .toast-icon {
  color: var(--md-primary, #D0BCFF);
}

.toast-text {
  line-height: 1.4;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

.toast-action {
  flex-shrink: 0;
  margin-left: 8px;
  border: 0;
  border-radius: 999px;
  padding: 6px 12px;
  background: color-mix(in srgb, var(--md-primary, #D0BCFF) 22%, transparent);
  color: var(--md-primary, #D0BCFF);
  font: inherit;
  font-size: 13px;
  font-weight: 650;
  letter-spacing: 0.01em;
  cursor: pointer;
  transition: background 160ms ease, transform 160ms ease, color 160ms ease;
}

.toast-action:hover {
  background: var(--md-primary, #D0BCFF);
  color: var(--md-on-primary, #381E72);
  transform: translateY(-1px);
}

.toast-action:active {
  transform: translateY(0) scale(0.98);
}

@keyframes toast-in {
  from {
    opacity: 0;
    transform: translateY(16px) scale(0.95);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
}

@keyframes toast-out {
  from {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
  to {
    opacity: 0;
    transform: translateY(16px) scale(0.95);
  }
}
</style>
