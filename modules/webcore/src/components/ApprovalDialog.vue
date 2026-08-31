<script setup lang="ts">
import { ShieldAlert, X } from '@lucide/vue'
import { useExecutionStore } from '@/stores/execution'

const execution = useExecutionStore()
</script>

<template>
  <Teleport to="body">
    <Transition name="fade">
      <div
        v-if="execution.approval"
        class="fixed inset-0 z-50 flex items-center justify-center p-4"
        style="background: var(--overlay)"
        role="dialog"
        aria-modal="true"
      >
        <div class="glass w-full max-w-md rounded-xl p-5">
          <div class="flex items-center justify-between">
            <div class="flex items-center gap-2 text-danger">
              <ShieldAlert class="h-5 w-5" />
              <span class="text-[13px] font-semibold">{{ execution.approval.title }}</span>
            </div>
            <button
              class="btn-icon"
              type="button"
              aria-label="Dismiss"
              @click="execution.respond(false)"
            >
              <X class="h-4 w-4" />
            </button>
          </div>

          <p class="mt-3 text-[13px] text-muted-foreground">{{ execution.approval.detail }}</p>

          <pre
            v-if="execution.approval.command"
            class="mt-3 overflow-x-auto rounded-lg bg-surface-muted p-3 font-mono text-[12px] leading-5 text-foreground"
            >{{ execution.approval.command }}</pre>

          <div class="mt-5 flex justify-end gap-2">
            <button class="btn btn-danger-outline" type="button" @click="execution.respond(false)">
              Deny
            </button>
            <button class="btn btn-primary" type="button" @click="execution.respond(true)">
              Allow
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.15s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.fade-enter-active .glass {
  transform: translateY(0) scale(1);
}

.fade-enter-from .glass {
  transform: translateY(8px) scale(0.97);
}
</style>