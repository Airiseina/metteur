<script setup lang="ts">
import { useFeedbackStore } from '@/stores/feedback'

const feedback = useFeedbackStore()
</script>

<template>
  <Teleport to="body">
    <Transition name="confirm">
      <div
        v-if="feedback.pending"
        class="fixed inset-0 z-1100 flex items-center justify-center p-4"
        style="background: var(--overlay)"
        @mousedown.self="feedback.settle(false)"
      >
        <div class="w-full max-w-sm rounded-xl p-5" style="background: var(--popover); box-shadow: var(--shadow-popover); border: 1px solid var(--divider)">
          <h2 class="text-[13.5px] font-semibold text-foreground">{{ feedback.pending.req.header }}</h2>
          <p class="mt-1.5 text-[12.5px] leading-relaxed text-muted-foreground wrap-break-word">
            {{ feedback.pending.req.message }}
          </p>
          <div class="mt-5 flex justify-end gap-2">
            <button class="btn" type="button" @click="feedback.settle(false)">
              {{ feedback.pending.req.rejectLabel || 'Cancel' }}
            </button>
            <button
              class="btn"
              :class="feedback.pending.req.danger ? 'btn-danger-outline' : 'btn-primary'"
              type="button"
              autofocus
              @click="feedback.settle(true)"
            >
              {{ feedback.pending.req.acceptLabel || 'OK' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.confirm-enter-active,
.confirm-leave-active {
  transition: opacity 140ms ease;
}
.confirm-enter-from,
.confirm-leave-to {
  opacity: 0;
}
</style>