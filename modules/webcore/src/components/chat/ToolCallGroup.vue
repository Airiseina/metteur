<script setup lang="ts">
import { computed, ref } from 'vue'
import { ChevronRight } from '@lucide/vue'
import ToolCallCard from './ToolCallCard.vue'
import { formatDuration, toolDetail, toolSubject, toolVerb } from '@/lib/chat/tool-summary'
import type { ChatMessage } from '@/core'

/**
 * A run of consecutive tool calls, folded into one line.
 *
 * A single edit or search is noise on its own; ten of them in a row are a wall.
 * While the group runs it stays open and names the call in flight; once it
 * settles it collapses to `4 steps · 6.2s`, which is what makes a long run
 * readable at a glance.
 */
const props = defineProps<{
  messages: ChatMessage[]
  onOpenFile?: (path: string) => void
}>()

const expanded = ref(false)

const running = computed(() => props.messages.some((m) => m.pending === true))
const totalMs = computed(() =>
  props.messages.reduce((sum, m) => sum + (toolDetail(m).elapsedMs ?? 0), 0),
)
const failures = computed(
  () => props.messages.filter((m) => m.pending !== true && toolDetail(m).ok === false).length,
)

/** The step in flight, or the last one that finished. */
const current = computed(() => props.messages[props.messages.length - 1])

const summary = computed(() => {
  const steps = props.messages.length === 1 ? '1 step' : `${props.messages.length} steps`
  const time = formatDuration(totalMs.value)
  return time ? `${steps} · ${time}` : steps
})

const open = computed(() => expanded.value || running.value)
</script>

<template>
  <div class="chat-tool-group">
    <button
      class="chat-group-row"
      type="button"
      :aria-expanded="open"
      @click="expanded = !expanded"
    >
      <ChevronRight
        class="h-3 w-3 shrink-0 text-subtle transition-transform duration-150"
        :class="open ? 'rotate-90' : ''"
      />
      <span class="font-medium text-foreground/85">
        {{ running ? toolVerb(current.actor) : summary }}
      </span>
      <span v-if="running" class="min-w-0 flex-1 truncate font-mono text-[12.5px] text-subtle">
        {{ toolSubject(current) }}
      </span>
      <span v-if="failures" class="shrink-0 text-[11.5px] text-status-error">
        {{ failures }} failed
      </span>
    </button>
    <div v-if="open" class="chat-group-body">
      <ToolCallCard
        v-for="message in messages"
        :key="message.id"
        :message="message"
        :on-open-file="onOpenFile"
      />
    </div>
  </div>
</template>
