<script setup lang="ts">
import { computed, ref } from 'vue'
import { Brain, ChevronRight } from '@lucide/vue'
import { renderStreaming } from '@/lib/chat/stream-render'

/**
 * The agent's reasoning.
 *
 * Collapsed to a single line that names the state and the time it took: the
 * scratchpad is not the answer and should not compete with it. It opens by
 * itself while thinking and closes a second after the reasoning ends, unless the
 * reader opened it on purpose.
 */
const props = defineProps<{
  text: string
  /** Whether reasoning is still arriving. */
  streaming: boolean
  /** Seconds spent thinking, for the settled label. */
  seconds: number
}>()

const open = ref(false)
/** Set once the reader toggles the block by hand. */
const pinned = ref(false)

const label = computed(() =>
  props.streaming ? 'Thinking' : `Thought for ${props.seconds}s`,
)

/** Auto-open while streaming, auto-close shortly after it ends. */
let closeTimer: ReturnType<typeof setTimeout> | undefined
function sync(next: boolean): void {
  if (pinned.value) return
  clearTimeout(closeTimer)
  if (next) open.value = true
  else closeTimer = setTimeout(() => (open.value = false), 1000)
}
defineExpose({ sync })

function toggle(): void {
  pinned.value = true
  open.value = !open.value
}

const html = computed(() => renderStreaming(props.text, props.streaming))
</script>

<template>
  <div class="mb-1">
    <button
      class="chat-reasoning-trigger group/think flex items-center gap-2 text-[12.5px] transition-colors duration-100 hover:text-foreground"
      type="button"
      :aria-expanded="open"
      @click="toggle"
    >
      <ChevronRight
        class="h-3 w-3 shrink-0 transition-transform duration-150"
        :class="open ? 'rotate-90' : ''"
      />
      <Brain class="h-3 w-3 shrink-0" />
      <span :class="streaming && !pinned ? 'chat-shimmer' : ''">{{ label }}</span>
    </button>
    <div
      v-if="open"
      class="chat-reasoning-body mt-1.5 overflow-auto text-[13px] leading-[21px] text-muted-foreground"
    >
      <div class="md-body md-body-quiet" v-html="html" />
    </div>
  </div>
</template>
