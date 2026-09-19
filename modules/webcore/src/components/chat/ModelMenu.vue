<script setup lang="ts">
import { computed } from 'vue'
import { Brain, Check, ChevronUp, Cpu, Settings2, SlidersHorizontal } from '@lucide/vue'
import type { ChatOptions, LlmModelConfig } from '@/core'

/**
 * Model and reasoning effort in one control.
 *
 * They are chosen together — a model without an effort level, or an effort
 * level without knowing which model runs it, is not a decision anyone makes
 * separately — so the button reads `model · effort` and the menu holds both.
 * Changing either never touches the draft.
 */
const props = defineProps<{
  models: Record<string, LlmModelConfig>
  modelKeys: string[]
  model: string
  effort: NonNullable<ChatOptions['reasoning_effort']>
  open: boolean
  onToggle: () => void
  onSelectModel: (key: string) => void
  onSelectEffort: (effort: NonNullable<ChatOptions['reasoning_effort']>) => void
  onOpenParameters: () => void
  onOpenSettings: () => void
}>()

const EFFORTS: Array<{ key: NonNullable<ChatOptions['reasoning_effort']>; label: string }> = [
  { key: 'none', label: 'Off' },
  { key: 'low', label: 'Low' },
  { key: 'medium', label: 'Medium' },
  { key: 'high', label: 'High' },
]

const label = computed(() => {
  const key = props.model || props.modelKeys[0] || ''
  const name = props.models[key]?.display_name || key || 'No model'
  const effort = EFFORTS.find((e) => e.key === props.effort)?.label ?? 'Medium'
  return `${name} · ${effort}`
})

const short = computed(() => {
  const key = props.model || props.modelKeys[0] || ''
  return props.models[key]?.display_name || key || 'No model'
})
</script>

<template>
  <div class="relative min-w-0">
    <button
      class="chat-control chat-control-wide"
      type="button"
      :aria-expanded="open"
      :aria-label="`Model and reasoning effort: ${label}`"
      @click="onToggle"
    >
      <Cpu class="h-4 w-4 shrink-0" />
      <span class="chat-control-label">{{ short }}</span>
      <span class="chat-control-sep" aria-hidden="true">·</span>
      <span class="chat-control-effort">{{ EFFORTS.find((e) => e.key === effort)?.label }}</span>
      <ChevronUp class="h-3.5 w-3.5 shrink-0 opacity-60" />
    </button>

    <div v-if="open" class="chat-menu chat-menu-up w-80">
      <p class="chat-menu-head">Reasoning effort</p>
      <div class="chat-segment" role="group" aria-label="Reasoning effort">
        <button
          v-for="option in EFFORTS"
          :key="option.key"
          class="chat-segment-item"
          :class="option.key === effort ? 'chat-segment-item-active' : ''"
          type="button"
          @click="onSelectEffort(option.key)"
        >
          <Brain class="h-3.5 w-3.5" />
          {{ option.label }}
        </button>
      </div>

      <div class="my-2 border-t border-divider" />
      <p class="chat-menu-head">Model</p>
      <div v-if="!modelKeys.length" class="px-2 py-1.5 text-[12.5px] text-subtle">
        No model configured.
      </div>
      <div class="max-h-56 overflow-y-auto">
        <button
          v-for="key in modelKeys"
          :key="key"
          class="chat-menu-item"
          type="button"
          @click="onSelectModel(key)"
        >
          <Check v-if="key === model" class="h-4 w-4 shrink-0 text-primary" />
          <span v-else class="w-4 shrink-0" />
          <span class="min-w-0 flex-1 truncate">{{ models[key]?.display_name || key }}</span>
          <span class="shrink-0 font-mono text-[11px] text-subtle">{{ models[key]?.model_id || key }}</span>
        </button>
      </div>

      <div class="my-1.5 border-t border-divider" />
      <button class="chat-menu-item" type="button" @click="onOpenParameters">
        <SlidersHorizontal class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">Generation parameters…</span>
      </button>
      <button class="chat-menu-item" type="button" @click="onOpenSettings">
        <Settings2 class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">Models and endpoints…</span>
      </button>
    </div>
  </div>
</template>
