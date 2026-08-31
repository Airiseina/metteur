<script setup lang="ts">
import { RotateCcw } from '@lucide/vue'

/**
 * One setting row: label + description, the control (slotted), a chip
 * showing the layer the current value comes from, and a reset action that
 * clears the setting from the layer being edited.
 */

const props = defineProps<{
  label: string
  description?: string
  /** Where the displayed value comes from (drives the chip label). */
  source: 'User' | 'Workspace' | 'Inherit' | 'Default'
  /** Whether the edited layer holds a value for this setting. */
  resettable: boolean
}>()

const emit = defineEmits<{ reset: [] }>()
</script>

<template>
  <div class="flex items-center gap-4 px-4 py-3">
    <div class="min-w-0 flex-1">
      <div class="flex items-center gap-2">
        <p class="text-[13px] font-medium">{{ props.label }}</p>
        <span
          class="chip h-4.5! px-1.5! text-[10px]!"
          :class="
            props.source === 'Workspace'
              ? 'bg-primary-soft! text-primary!'
              : props.source === 'User'
                ? 'bg-emerald-500/10! text-emerald-600! dark:text-emerald-400!'
                : ''
          "
        >
          {{ props.source }}
        </span>
      </div>
      <p v-if="props.description" class="mt-0.5 text-[12px] text-muted-foreground">
        {{ props.description }}
      </p>
    </div>
    <div class="flex shrink-0 items-center gap-1.5">
      <slot />
    </div>
    <button
      v-if="props.resettable"
      class="btn-icon h-6! w-6!"
      type="button"
      title="Reset to the layer below"
      :aria-label="`Reset ${props.label}`"
      @click="emit('reset')"
    >
      <RotateCcw class="h-3.5 w-3.5" />
    </button>
  </div>
</template>