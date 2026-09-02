<script setup lang="ts">
import { X } from '@lucide/vue'

/** Modal showing file/folder metadata gathered by the explorer (properties). */
defineProps<{
  title: string
  /** Label/value pairs displayed in a definition list. */
  rows: { label: string; value: string }[]
}>()

const emit = defineEmits<{ (e: 'close'): void }>()
</script>

<template>
  <div
    class="glass fixed inset-0 z-50 flex items-center justify-center bg-background/40"
    role="dialog"
    :aria-label="title"
    @mousedown.self="emit('close')"
  >
    <div class="w-96 max-w-[92%] overflow-hidden rounded-xl border border-divider bg-background shadow-card">
      <div class="flex h-9 items-center gap-2 border-b border-divider px-3">
        <span class="text-[13px] font-medium">{{ title }}</span>
        <button class="btn-icon ml-auto h-6! w-6!" type="button" aria-label="Close" @click="emit('close')">
          <X class="h-3.5 w-3.5" />
        </button>
      </div>
      <dl class="divide-y divide-divider px-3 py-1">
        <div v-for="row in rows" :key="row.label" class="flex gap-3 py-1.5">
          <dt class="w-24 shrink-0 text-[11.5px] text-subtle">{{ row.label }}</dt>
          <dd class="min-w-0 flex-1 truncate text-[11.5px] text-foreground" :title="row.value">{{ row.value }}</dd>
        </div>
      </dl>
    </div>
  </div>
</template>