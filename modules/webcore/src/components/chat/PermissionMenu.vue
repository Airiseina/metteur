<script setup lang="ts">
import { computed } from 'vue'
import { Check, ChevronUp, ShieldAlert, ShieldCheck, ShieldHalf } from '@lucide/vue'

/**
 * Who answers authorization questions.
 *
 * The three modes are shown as a dot plus a label, never as a colour alone, and
 * `full` additionally keeps a standing notice in the composer: a state that
 * skips confirmations must stay visible after the menu closes.
 */
export type PermissionMode = 'ask' | 'sandbox' | 'full'

const props = defineProps<{
  mode: PermissionMode
  open: boolean
  onToggle: () => void
  onSelect: (mode: PermissionMode) => void
}>()

interface ModeInfo {
  key: PermissionMode
  label: string
  icon: unknown
  detail: string
}

const MODES: ModeInfo[] = [
  {
    key: 'ask',
    label: 'Confirm changes',
    icon: ShieldAlert,
    detail: 'Every file edit and command waits for your approval.',
  },
  {
    key: 'sandbox',
    label: 'Sandbox',
    icon: ShieldHalf,
    detail: 'Edits inside the workspace and policy-approved commands run; everything else asks.',
  },
  {
    key: 'full',
    label: 'Full access',
    icon: ShieldCheck,
    detail: 'Risky operations are reviewed by the model instead of you, then run.',
  },
]

const current = computed(() => MODES.find((m) => m.key === props.mode) ?? MODES[1])
</script>

<template>
  <div class="relative">
    <button
      class="chat-control"
      :class="mode === 'full' ? 'chat-control-danger' : ''"
      type="button"
      :aria-expanded="open"
      :aria-label="`Permission mode: ${current.label}`"
      @click="onToggle"
    >
      <span class="chat-control-dot" :class="`chat-control-dot-${mode}`" aria-hidden="true" />
      <component :is="current.icon" class="h-4 w-4 shrink-0" />
      <span class="chat-control-label">{{ current.label }}</span>
      <ChevronUp class="h-3.5 w-3.5 shrink-0 opacity-60" />
    </button>

    <div v-if="open" class="chat-menu chat-menu-up w-80">
      <p class="chat-menu-head">Permissions</p>
      <button
        v-for="mode in MODES"
        :key="mode.key"
        class="chat-menu-item items-start!"
        type="button"
        @click="onSelect(mode.key)"
      >
        <Check v-if="mode.key === props.mode" class="mt-0.5 h-4 w-4 shrink-0 text-primary" />
        <span v-else class="mt-0.5 w-4 shrink-0" />
        <component :is="mode.icon" class="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">
          <span class="block font-medium">{{ mode.label }}</span>
          <span class="mt-0.5 block text-[12px] leading-snug text-muted-foreground">
            {{ mode.detail }}
          </span>
        </span>
      </button>
    </div>
  </div>
</template>
