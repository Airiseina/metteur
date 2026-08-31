<script setup lang="ts">
import { computed, ref } from 'vue'
import { ChevronDown } from '@lucide/vue'

/**
 * A searchable single-select dropdown: type to filter the option list, pick to
 * emit `update:modelValue`. Used for currency / timezone in the Billing group.
 */

const props = defineProps<{
  modelValue: string
  options: string[]
  placeholder?: string
}>()

const emit = defineEmits<{ 'update:modelValue': [v: string] }>()

const open = ref(false)
const query = ref(props.modelValue)

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  if (!q) return props.options
  return props.options.filter((o) => o.toLowerCase().includes(q))
})

function onFocus() {
  query.value = props.modelValue
  open.value = true
}

function onBlur() {
  open.value = false
  query.value = props.modelValue
}

function choose(o: string) {
  emit('update:modelValue', o)
  query.value = o
  open.value = false
}
</script>

<template>
  <div class="relative w-44">
    <div class="relative">
      <input
        class="input h-7! w-full! pr-6! text-[12px]!"
        :value="query"
        :placeholder="props.placeholder"
        @focus="onFocus"
        @blur="onBlur"
        @input="query = ($event.target as HTMLInputElement).value"
      />
      <ChevronDown class="pointer-events-none absolute right-1.5 top-1/2 h-3 w-3 -translate-y-1/2 text-subtle" />
    </div>
    <div
      v-if="open"
      class="glass absolute left-0 right-0 top-full z-20 mt-1 max-h-48 overflow-auto rounded-lg border border-divider py-1 shadow-card"
    >
      <button
        v-for="o in filtered"
        :key="o"
        class="flex w-full items-center gap-1.5 rounded-md px-2.5 py-1 text-left text-[12px] hover:bg-accent"
        :class="o === props.modelValue ? 'bg-accent text-accent-foreground' : 'text-foreground'"
        type="button"
        @mousedown.prevent="choose(o)"
      >
        <span class="truncate">{{ o }}</span>
      </button>
      <p v-if="!filtered.length" class="px-2.5 py-1.5 text-[11.5px] text-subtle">No match</p>
    </div>
  </div>
</template>