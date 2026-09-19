<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronRight,
  FileCode,
  FileSearch,
  FileText,
  History,
  ListTree,
  Loader2,
  PencilLine,
  Terminal,
  Wrench,
} from '@lucide/vue'
import {
  diffLines,
  diffStat,
  formatDuration,
  isDiffLike,
  outputLines,
  toolDetail,
  toolIcon,
  toolSubject,
  toolVerb,
  type ToolIcon,
} from '@/lib/chat/tool-summary'
import type { ChatMessage } from '@/core'

/**
 * One tool call, as a single row that expands into its output.
 *
 * The row carries everything a reader needs at a glance — verb, subject,
 * duration, outcome — because that is what makes a forty-step run scannable.
 * The body (a diff, command output, a file listing) is one click away and
 * height-capped so it never takes over the transcript.
 */
const props = defineProps<{
  message: ChatMessage
  /** Opens the file the call touched, when it names one. */
  onOpenFile?: (path: string) => void
}>()

const open = ref(false)
const detail = computed(() => toolDetail(props.message))
const running = computed(() => props.message.pending === true)
const failed = computed(() => props.message.pending !== true && detail.value.ok === false)
const duration = computed(() => formatDuration(detail.value.elapsedMs ?? 0))
const diff = computed(() => diffStat(props.message.content))
const lines = computed(() => outputLines(props.message.content))
const shown = computed(() => (open.value ? lines.value : lines.value.slice(0, 6)))
const file = computed(() => (typeof detail.value.file === 'string' ? detail.value.file : ''))
/** Lucide component for the call's family: a read looks like a read. */
const ICON_COMPONENTS: Record<ToolIcon, unknown> = {
  read: FileText,
  write: PencilLine,
  search: FileSearch,
  command: Terminal,
  job: ListTree,
  agent: Bot,
  check: Check,
  plan: ListTree,
  snapshot: History,
  blueprint: Wrench,
  context: History,
  tool: Wrench,
}

const icon = computed(() => ICON_COMPONENTS[toolIcon(props.message.actor)])
</script>

<template>
  <div class="chat-tool" :class="failed ? 'chat-tool-failed' : ''">
    <button
      class="chat-tool-row group/row"
      type="button"
      :aria-expanded="open"
      @click="open = !open"
    >
      <ChevronRight
        class="h-3 w-3 shrink-0 text-subtle transition-transform duration-150"
        :class="open ? 'rotate-90' : ''"
      />
      <component :is="icon" class="h-4 w-4 shrink-0 text-subtle" />
      <!-- The verb shimmers while the call runs: a moving label reads as
           "working", where a static one next to a spinner reads as stuck. -->
      <span
        class="shrink-0 font-medium text-foreground/85"
        :class="running ? 'chat-shimmer' : ''"
      >
        {{ toolVerb(message.actor) }}
      </span>
      <span class="min-w-0 flex-1 truncate font-mono text-[12.5px] text-muted-foreground">
        {{ toolSubject(message) }}
      </span>
      <span v-if="diff" class="ml-1 shrink-0 font-mono text-[11.5px] tabular-nums">
        <span class="text-status-success">+{{ diff.added }}</span>
        <span class="ml-1 text-status-error">−{{ diff.removed }}</span>
      </span>
      <Loader2 v-if="running" class="h-3 w-3 shrink-0 animate-spin text-status-running" />
      <AlertTriangle v-else-if="failed" class="h-3 w-3 shrink-0 text-status-error" />
      <Check v-else class="h-3 w-3 shrink-0 text-status-success" />
      <span
        v-if="duration"
        class="w-11 shrink-0 text-right font-mono text-[11.5px] tabular-nums text-subtle"
      >
        {{ duration }}
      </span>
    </button>

    <div v-if="open && message.content" class="chat-tool-body">
      <div v-if="isDiffLike(message.content)" class="font-mono text-[12.5px] leading-relaxed">
        <div v-for="(line, i) in diffLines(message.content)" :key="i" class="whitespace-pre" :class="line.tone">
          {{ line.text || ' ' }}
        </div>
      </div>
      <pre v-else class="chat-tool-output">{{ shown.join('\n') }}</pre>
      <div class="mt-1.5 flex items-center gap-3">
        <button
          v-if="lines.length > 6"
          class="text-[12px] text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground"
          type="button"
          @click="open = false"
        >
          Collapse
        </button>
        <button
          v-if="file && onOpenFile"
          class="flex items-center gap-1 text-[12px] text-muted-foreground underline decoration-dotted underline-offset-2 hover:text-foreground"
          type="button"
          @click="onOpenFile(file)"
        >
          <FileCode class="h-3 w-3" /> Open file
        </button>
      </div>
    </div>
  </div>
</template>
