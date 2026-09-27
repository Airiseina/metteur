<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { Brain, ChevronRight } from '@lucide/vue'
import { renderStreaming } from '@/lib/chat/stream-render'

/**
 * The agent's reasoning.
 *
 * Collapsed to a single line with the complete request's duration: the
 * scratchpad is not the answer and should not compete with it. It opens by
 * itself while thinking and closes a second after the reasoning ends, unless the
 * reader opened it on purpose.
 */
const props = defineProps<{
  text: string
  /** Whether reasoning is still arriving. */
  streaming: boolean
  /** Missing on older transcripts that never recorded timing. */
  elapsedMs?: number
  /** Message-owned start time, so remounting does not reset the live counter. */
  startedAt?: number
}>()

const open = ref(false)
/** Set once the reader toggles the block by hand. */
const pinned = ref(false)

const now = ref(Date.now())
watch(() => props.startedAt !== undefined, (active, _previous, onCleanup) => {
  if (!active) return
  const refresh = () => (now.value = Date.now())
  refresh()
  // Derive elapsed time from the message timestamp, never from a tick count:
  // background-tab throttling must not lose seconds.
  const timer = setInterval(refresh, 1000)
  document.addEventListener('visibilitychange', refresh)
  onCleanup(() => {
    clearInterval(timer)
    document.removeEventListener('visibilitychange', refresh)
  })
}, { immediate: true })

const duration = computed(() => {
  if (props.startedAt !== undefined) {
    const elapsed = (props.elapsedMs ?? 0)
      + (props.startedAt === undefined ? 0 : Math.max(0, now.value - props.startedAt))
    return `${Math.floor(elapsed / 1000)}s`
  }
  if (props.elapsedMs === undefined) return null
  if (props.elapsedMs < 1000) return '<1s'
  return `${Math.floor(props.elapsedMs / 1000)}s`
})

/** Auto-open while streaming, auto-close shortly after it ends. */
let closeTimer: ReturnType<typeof setTimeout> | undefined
function sync(next: boolean): void {
  if (pinned.value) return
  clearTimeout(closeTimer)
  if (next) open.value = true
  else closeTimer = setTimeout(() => (open.value = false), 1000)
}
defineExpose({ sync })
onBeforeUnmount(() => clearTimeout(closeTimer))

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
      :disabled="!text"
      :aria-expanded="open"
      @click="toggle"
    >
      <ChevronRight
        v-if="text"
        class="h-3 w-3 shrink-0 transition-transform duration-150"
        :class="open ? 'rotate-90' : ''"
      />
      <Brain class="h-3 w-3 shrink-0" />
      <span class="whitespace-nowrap">
        <span :class="startedAt !== undefined && !pinned ? 'chat-shimmer' : ''">{{ startedAt !== undefined ? 'Working' : duration !== null ? 'Worked' : 'Thought' }}</span><template v-if="duration !== null"> for <span class="chat-reasoning-time inline-block min-w-[3ch] tabular-nums">{{ duration }}</span></template>
      </span>
    </button>
    <div
      v-if="open && text"
      class="chat-reasoning-body mt-1.5 overflow-auto text-[13px] leading-[21px] text-muted-foreground"
    >
      <div class="md-body md-body-quiet" v-html="html" />
    </div>
  </div>
</template>
