<script setup lang="ts">
/**
 * Switch toggle with hairline track + sliding knob.
 *
 * Track/knob geometry uses explicit inline styles (not Tailwind arbitrary
 * transforms) so the knob offset and colours are stable across Tailwind
 * versions and dark mode.
 */
const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [v: boolean] }>()

function toggle() {
  emit('update:modelValue', !props.modelValue)
}
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="props.modelValue"
    class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors duration-150"
    :style="{
      background: props.modelValue ? 'var(--primary)' : 'var(--surface-muted)',
      boxShadow: 'inset 0 0 0 1px var(--border)',
    }"
    @click="toggle"
  >
    <span
      class="pointer-events-none absolute top-0.5 left-0.5 h-4 w-4 rounded-full transition-transform duration-150"
      :style="{
        background: 'var(--surface)',
        boxShadow: 'var(--shadow-card)',
        transform: props.modelValue ? 'translateX(16px)' : 'translateX(0)',
      }"
    />
  </button>
</template>