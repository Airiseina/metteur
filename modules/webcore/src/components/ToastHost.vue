<script setup lang="ts">
import { useFeedbackStore } from '@/stores/feedback'

const feedback = useFeedbackStore()
</script>

<template>
  <Teleport to="body">
    <div class="pointer-events-none fixed bottom-4 right-4 z-1200 flex w-80 flex-col gap-2">
      <TransitionGroup name="toast">
        <div
          v-for="t in feedback.toasts"
          :key="t.id"
          class="toast-card pointer-events-auto flex items-start gap-2 px-3 py-2.5"
          role="alert"
        >
          <span class="toast-rail mt-0.5 h-3 w-0.5 shrink-0 rounded-full" :class="`toast-rail--${t.kind}`" />
          <div class="min-w-0 flex-1">
            <p class="text-[12.5px] font-medium text-foreground">{{ t.summary }}</p>
            <p v-if="t.detail" class="mt-0.5 text-[11.5px] leading-relaxed text-muted-foreground wrap-break-word">{{ t.detail }}</p>
          </div>
          <button
            class="flex h-5 w-5 shrink-0 items-center justify-center rounded text-subtle hover:bg-hover hover:text-foreground"
            type="button"
            :aria-label="'Dismiss'"
            @click="feedback.dismiss(t.id)"
          >
            <span class="text-[13px] leading-none">✕</span>
          </button>
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.toast-card {
  border: 1px solid var(--divider);
  background: var(--surface);
  box-shadow: var(--shadow-popover);
  border-radius: 10px;
}
.toast-rail--info {
  background: var(--primary);
}
.toast-rail--success {
  background: #2fb08f;
}
.toast-rail--error {
  background: var(--danger);
}

.toast-enter-active,
.toast-leave-active {
  transition:
    opacity 160ms ease,
    transform 160ms ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(6px);
}
</style>