<script setup lang="ts">
import { computed, ref } from 'vue'
import { Check } from '@lucide/vue'
import type { SnapshotInfo } from '@/core'
import { useVersionStore } from '@/stores/version'
import { useFeedbackStore } from '@/stores/feedback'
import ContextMenu from '@/components/ContextMenu.vue'

/** The version timeline, rendered in the generic ide-sidebar panel. */

const version = useVersionStore()
const feedback = useFeedbackStore()

const sortedSnapshots = computed(() => [...version.snapshots].sort((a, b) => b.createdAt - a.createdAt))
const menu = ref<{ x: number; y: number; snapshot: SnapshotInfo } | null>(null)
const copied = ref(false)

function format(ts: number): string {
  return new Intl.DateTimeFormat(undefined, { timeStyle: 'short', dateStyle: 'medium' }).format(ts)
}

function onContext(e: MouseEvent, snapshot: SnapshotInfo) {
  version.select(snapshot.id)
  menu.value = { x: e.clientX, y: e.clientY, snapshot }
}

async function onMenuSelect(id: string) {
  const s = menu.value?.snapshot
  if (!s) return
  if (id === 'copy-id') {
    await navigator.clipboard?.writeText(s.id)
    copied.value = true
    setTimeout(() => (copied.value = false), 1200)
  } else if (id === 'rollback') {
    const ok = await feedback.confirm({
      header: 'Confirm rollback',
      message: `Roll back to ${s.alias ?? s.id}?`,
      acceptLabel: 'Roll back',
      rejectLabel: 'Cancel',
      danger: true,
    })
    if (ok) await version.rollback(s.id)
  }
}
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="sticky top-0 z-10 flex items-center justify-between px-3 py-1.5">
      <span class="panel-heading px-0!">Snapshots</span>
      <span class="text-[10.5px] text-subtle">{{ version.snapshots.length }}</span>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
      <ol v-if="sortedSnapshots.length" class="relative ml-2 space-y-1 border-l border-divider pl-4">
        <li v-for="s in sortedSnapshots" :key="s.id" class="relative">
          <span
            class="absolute -left-5.25 top-2 h-2 w-2 rounded-full transition-colors duration-150"
            :style="{ background: s.alias ? 'var(--primary)' : 'var(--subtle)' }"
          />
          <button
            class="flex w-full flex-col gap-0.5 rounded-md px-2 py-1.5 text-left transition-colors duration-150 hover:bg-hover"
            :class="version.selectedId === s.id ? 'bg-accent' : ''"
            type="button"
            @click="version.select(s.id)"
            @contextmenu.prevent="onContext($event, s)"
          >
            <span class="flex items-center gap-1.5 text-[12px] font-medium">
              <span class="shrink-0 font-mono text-[11px] text-muted-foreground">{{ s.id }}</span>
              <span v-if="s.alias" class="chip py-0!" style="color: var(--primary)">{{ s.alias }}</span>
            </span>
            <span class="truncate text-[11px] text-muted-foreground">{{ s.message }}</span>
            <span class="text-[10px] text-subtle">{{ format(s.createdAt) }}</span>
          </button>
        </li>
      </ol>
      <p v-else class="px-2 py-1 text-[12px] text-muted-foreground">No snapshots yet.</p>
    </div>

    <ContextMenu
      v-if="menu"
      :x="menu.x"
      :y="menu.y"
      :groups="[{
        label: 'Snapshot',
        items: [
          { id: 'rollback', label: 'Roll back to this snapshot', hint: '↺' },
          { id: 'copy-id', label: 'Copy snapshot id' },
        ],
      }]"
      @select="onMenuSelect"
      @close="menu = null"
    />

    <span v-if="copied" class="pointer-events-none fixed z-50 flex items-center gap-1 rounded-md px-2 py-1 text-[11px] text-primary">
      <Check class="h-3 w-3" /> Copied
    </span>
  </div>
</template>