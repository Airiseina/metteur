<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Activity, Pause, Play, Square, Terminal, X } from '@lucide/vue'
import { useExecutionStore } from '@/stores/execution'
import { useWorkspaceStore } from '@/stores/workspace'
import type { EventLine } from '@/stores/execution'

const workspace = useWorkspaceStore()
const execution = useExecutionStore()

/** Whether the log pane is collapsed, mirroring IDE debugging tool panels. */
const logOpen = ref(true)
/** Node whose detail is shown; derived from the node activity rail. */
const selected = ref<string | null>(null)

onMounted(() => workspace.refresh())

interface NodeActivity {
  id: string
  status: 'run' | 'done' | 'error'
  lastMessage: string
  /** Concatenated per-event detail payloads for the selected node. */
  detail?: Record<string, unknown>
}

const activities = computed<NodeActivity[]>(() => {
  const map = new Map<
    string,
    { done: boolean; error: boolean; message: string; detail: Record<string, unknown> }
  >()
  for (const ev of execution.events) {
    if (ev.kind === 'context') continue // context rolls into the usage panel
    const cur = map.get(ev.nodeId) ?? { done: false, error: false, message: '', detail: {} }
    if (ev.kind === 'finished') cur.done = true
    if (ev.kind === 'error') cur.error = true
    cur.message = ev.message
    if (ev.detail) cur.detail = { ...cur.detail, ...ev.detail }
    map.set(ev.nodeId, cur)
  }
  return Array.from(map.entries()).map(([id, s]) => ({
    id,
    status: s.error ? 'error' : s.done ? 'done' : 'run',
    lastMessage: s.message,
    detail: s.detail,
  }))
})

const selectedActivity = computed<NodeActivity | null>(
  () => activities.value.find((a) => a.id === selected.value) ?? null,
)

const KIND_TONE: Record<EventLine['kind'], string> = {
  started: 'text-primary',
  finished: 'text-emerald-600 dark:text-emerald-400',
  message: 'text-muted-foreground',
  approval_request: 'text-amber-600 dark:text-amber-400',
  context: 'text-violet-500',
  node_data: 'text-sky-600 dark:text-sky-400',
  error: 'text-danger',
}

const statusChip = computed(() => {
  switch (execution.status) {
    case 'running':
      return 'flex items-center gap-1.5 bg-primary/15 text-primary'
    case 'paused':
      return 'flex items-center gap-1.5 bg-amber-500/15 text-amber-600 dark:text-amber-400'
    case 'cancelled':
      return 'flex items-center gap-1.5 bg-danger-soft text-danger'
    default:
      return 'flex items-center gap-1.5 text-muted-foreground'
  }
})

/* Context usage panel                                                 */

const REGION_COLOR: Record<string, string> = {
  system: '#8b5cf6',
  user: '#3f8cff',
  assistant: '#2fbf8f',
  tool: '#f2994a',
}

const regionName = (r: string): string =>
  ({
    system: 'System / Prompt',
    user: 'User prompt',
    assistant: 'Assistant',
    tool: 'Tool results',
  })[r] ?? r

const usageList = computed(() => execution.contextUsage ?? [])

/** Bar segments as (region, pct, color, count) for the stacked strip. */
const barSegments = computed(() => {
  const total = Math.max(execution.contextTotal, 1)
  return usageList.value.map((r) => ({
    region: r.region,
    color: REGION_COLOR[r.region] ?? '#8f8fa8',
    pct: Math.round((r.tokens / total) * 1000) / 10,
    label: regionName(r.region),
  }))
})
</script>

<template>
  <div class="flex h-full flex-col">
    <!-- Execution toolbar -->
    <div class="flex h-11 shrink-0 items-center gap-2 border-b border-divider px-3">
      <Activity class="h-4 w-4 text-muted-foreground" />
      <span class="text-[13px] font-medium">Execution</span>
      <span class="chip" :class="statusChip">
        <span
          v-if="execution.status === 'running'"
          class="h-1.5 w-1.5 animate-pulse rounded-full bg-primary"
        />
        {{ execution.status }}
      </span>

      <div class="ml-auto flex items-center gap-1.5">
        <button
          v-if="execution.status === 'running'"
          class="btn btn-outline"
          type="button"
          @click="execution.pause()"
        >
          <Pause class="h-4 w-4" /> Pause
        </button>
        <button
          v-if="execution.status === 'paused'"
          class="btn btn-outline"
          type="button"
          @click="execution.resume()"
        >
          <Play class="h-4 w-4" /> Resume
        </button>
        <button
          v-if="execution.running"
          class="btn btn-danger-outline"
          type="button"
          @click="execution.cancel()"
        >
          <Square class="h-4 w-4" /> Cancel
        </button>
        <button class="btn btn-primary" type="button" @click="execution.run('bp-build-feature')">
          <Play class="h-4 w-4" /> Run
        </button>
      </div>
    </div>

    <div class="flex min-h-0 flex-1">
      <!-- Node activity rail -->
      <aside
        v-if="activities.length"
        class="flex w-44 shrink-0 flex-col border-r border-divider bg-sidebar p-2"
      >
        <p class="panel-heading mb-1 px-2">Execution flow</p>
        <ul class="space-y-0.5">
          <li v-for="a in activities" :key="a.id">
            <button
              class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-[12px] transition-colors duration-150 hover:bg-hover"
              :class="
                selected === a.id
                  ? 'bg-accent'
                  : a.status === 'run'
                    ? 'text-foreground'
                    : 'text-muted-foreground'
              "
              type="button"
              :title="a.lastMessage"
              @click="selected = a.id"
            >
              <span
                class="h-1.5 w-1.5 shrink-0 rounded-full"
                :class="
                  a.status === 'run'
                    ? 'animate-pulse bg-primary'
                    : a.status === 'error'
                      ? 'bg-danger'
                      : 'bg-subtle'
                "
              />
              <span class="truncate font-medium">{{ a.id }}</span>
              <X v-if="a.status === 'error'" class="ml-auto h-3 w-3 shrink-0 text-danger" />
            </button>
          </li>
        </ul>

        <!-- Selected node detail -->
        <div v-if="selectedActivity" class="mt-2 border-t border-divider pt-2">
          <p class="panel-heading mb-1 px-2">{{ selectedActivity.id }}</p>
          <p class="px-2 text-[11.5px] leading-relaxed text-muted-foreground">
            {{ selectedActivity.lastMessage || '—' }}
          </p>
          <pre
            v-if="selectedActivity.detail && Object.keys(selectedActivity.detail).length"
            class="mt-1 max-h-40 overflow-auto rounded-md bg-surface-muted p-2 text-[10.5px] leading-relaxed text-muted-foreground"
            >{{ JSON.stringify(selectedActivity.detail, null, 2) }}</pre>
        </div>
      </aside>

      <!-- Event log -->
      <div class="flex min-w-0 flex-1 flex-col">
        <div class="flex items-center gap-2 px-3 py-2">
          <Terminal class="h-3.5 w-3.5 text-muted-foreground" />
          <span class="text-[11px] font-semibold uppercase tracking-wider text-subtle"
            >Event Log</span
          >
          <button
            class="btn-icon ml-auto h-6! w-6!"
            type="button"
            :title="logOpen ? 'Collapse' : 'Expand'"
            @click="logOpen = !logOpen"
          >
            <X v-if="logOpen" class="h-3.5 w-3.5" />
            <Terminal v-else class="h-3.5 w-3.5" />
          </button>
        </div>

        <ul
          class="min-h-0 flex-1 overflow-y-auto px-1 font-mono text-[12px] leading-5"
          :class="logOpen ? '' : 'hidden'"
        >
          <li v-for="(ev, i) in execution.events" :key="i" class="flex gap-3 px-2 py-0.5">
            <span class="w-20 shrink-0" :class="KIND_TONE[ev.kind]">{{ ev.kind }}</span>
            <span class="w-20 shrink-0 text-muted-foreground">{{ ev.nodeId }}</span>
            <span class="min-w-0 flex-1 wrap-break-word">{{ ev.message }}</span>
          </li>
          <li v-if="execution.events.length === 0" class="px-2 py-1 text-muted-foreground">
            No events yet — press Run to start.
          </li>
        </ul>
      </div>

      <!-- Live context usage panel -->
      <aside
        class="flex w-72 shrink-0 flex-col border-l border-divider bg-sidebar p-3"
        :class="execution.contextUsage ? '' : 'opacity-60'"
      >
        <p class="panel-heading mb-2">Context usage</p>
        <p v-if="!execution.contextUsage" class="text-[11.5px] text-muted-foreground">
          Waiting for a Call LLM node…
        </p>

        <template v-else>
          <p class="mb-1 flex items-baseline justify-between">
            <span class="text-[11px] text-muted-foreground">Estimated tokens</span>
            <span class="text-[13px] font-semibold">{{
              execution.contextTotal.toLocaleString()
            }}</span>
          </p>

          <!-- Stacked region bar -->
          <div
            class="flex h-2.5 w-full overflow-hidden rounded-full"
            role="img"
            :aria-label="'Context regions'"
          >
            <div
              v-for="s in barSegments"
              :key="s.region"
              class="h-full transition-all duration-300"
              :style="{ width: s.pct + '%', background: s.color }"
              :title="`${s.label}: ${s.pct}%`"
            />
          </div>

          <!-- Per-region list -->
          <ul class="mt-3 space-y-1.5">
            <li v-for="r in usageList" :key="r.region" class="flex items-center gap-2">
              <span
                class="h-2.5 w-2.5 shrink-0 rounded-sm"
                :style="{ background: REGION_COLOR[r.region] ?? '#8f8fa8' }"
              />
              <span class="min-w-0 flex-1 truncate text-[11.5px] text-muted-foreground">{{
                regionName(r.region)
              }}</span>
              <span class="shrink-0 text-[11px] tabular-nums">{{ r.tokens.toLocaleString() }}</span>
            </li>
          </ul>

          <p
            v-if="execution.contextNode"
            class="mt-2 border-t border-divider pt-2 text-[10.5px] text-subtle"
          >
            snapshot from {{ execution.contextNode }}
          </p>
        </template>
      </aside>
    </div>
  </div>
</template>

