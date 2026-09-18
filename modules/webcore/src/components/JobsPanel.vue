<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { Loader2, Square, Terminal } from '@lucide/vue'
import { useJobsStore } from '@/stores/jobs'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * Live terminal for the workspace's background commands.
 *
 * Runs as a docking panel (the right-hand panel or a sidebar), so it can sit
 * next to the editor while a build is streaming. Output arrives through the
 * daemon's job stream; the panel scrolls with the tail unless the reader
 * scrolled up.
 */
const workspace = useWorkspaceStore()
const jobs = useJobsStore()
const outputEl = ref<HTMLElement | null>(null)
const pinned = ref(true)

/** One line per job: id, state, elapsed, command. */
function summaryOf(index: number): string {
  const job = jobs.entries[index]
  const seconds = (
    ((job.finishedAt || Date.now()) - job.startedAt) /
    1000
  ).toFixed(1)
  return `${job.id} · ${job.state} · ${seconds}s`
}

const selection = computed(() => jobs.selected)

function onScroll() {
  const el = outputEl.value
  if (!el) return
  pinned.value = el.scrollHeight - el.scrollTop - el.clientHeight < 32
}

watch(
  () => selection.value?.output.length ?? 0,
  () => {
    if (pinned.value) {
      nextTick(() => {
        outputEl.value?.scrollTo({ top: outputEl.value.scrollHeight })
      })
    }
  },
)

onMounted(() => {
  if (!workspace.active) return
  void jobs.start()
})

onBeforeUnmount(() => {
  // The store lives longer than this panel; only the stream follows the
  // workspace, so nothing to stop here beyond the panel's own observers.
  pinned.value = true
})
</script>

<template>
  <div class="flex h-full min-h-0 flex-col">
    <!-- Job list -->
    <div class="max-h-48 shrink-0 overflow-auto border-b border-divider">
      <p
        v-if="!jobs.entries.length"
        class="px-3 py-3 text-[12px] text-subtle"
      >
        No background commands yet. The agent's long-running commands appear here.
      </p>
      <button
        v-for="(job, index) in jobs.entries"
        :key="job.id"
        class="flex w-full items-center gap-2 border-b border-divider px-3 py-1.5 text-left text-[12px] transition-colors duration-150 hover:bg-hover"
        :class="selection?.id === job.id ? 'bg-primary-soft' : ''"
        type="button"
        @click="jobs.select(job.id)"
      >
        <Loader2 v-if="job.live" class="h-3 w-3 shrink-0 animate-spin" style="color: var(--primary)" />
        <Terminal v-else class="h-3 w-3 shrink-0 text-subtle" />
        <span class="min-w-0 flex-1 truncate font-mono">{{ job.command || job.id }}</span>
        <span class="shrink-0 text-[10.5px] text-subtle">{{ summaryOf(index) }}</span>
        <button
          v-if="job.live"
          class="btn-icon h-5! w-5! shrink-0"
          type="button"
          title="Terminate this command"
          aria-label="Terminate command"
          @click.stop="jobs.kill(job.id)"
        >
          <Square class="h-3 w-3" />
        </button>
      </button>
    </div>

    <!-- Output of the selected job -->
    <div class="min-h-0 flex-1 overflow-hidden">
      <div
        v-if="!selection"
        class="flex h-full items-center justify-center px-4 text-center text-[12px] text-subtle"
      >
        Select a command to see its output.
      </div>
      <div
        v-else
        ref="outputEl"
        class="h-full overflow-auto px-3 py-2 font-mono text-[11.5px] leading-relaxed whitespace-pre-wrap"
        @scroll.passive="onScroll"
      >{{ selection.output || '(no output yet)' }}</div>
    </div>
  </div>
</template>
