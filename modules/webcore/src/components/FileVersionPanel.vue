<script setup lang="ts">
import { onMounted, watch } from 'vue'
import { FileClock, History } from '@lucide/vue'
import { useVersionStore } from '@/stores/version'

/**
 * Version history for a single file, shown in the right-hand panel (opened
 * from an editor's Version button). Reuses the timeline store's per-file
 * history so the diffs match what the Version view shows.
 */
const props = defineProps<{ filePath?: string }>()

const version = useVersionStore()

async function load() {
  const p = props.filePath
  if (p) await version.pickFile(p)
}

onMounted(() => void load())
watch(() => props.filePath, () => void load())

function opTone(op: string): string {
  switch (op) {
    case 'add':
      return 'text-emerald-600 dark:text-emerald-400'
    case 'delete':
      return 'text-danger'
    default:
      return 'text-muted-foreground'
  }
}
</script>

<template>
  <div class="flex h-full flex-col bg-sidebar">
    <div class="sticky top-0 z-10 flex items-center gap-2 px-3 py-1.5">
      <FileClock class="h-3.5 w-3.5 text-muted-foreground" />
      <span class="panel-heading px-0!">Version history</span>
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
      <p
        v-if="props.filePath"
        class="mb-2 truncate px-1 text-[11px] text-subtle"
        :title="props.filePath"
      >
        {{ props.filePath }}
      </p>

      <div v-if="version.history.length" class="divide-y divide-divider">
        <div v-for="(h, i) in version.history" :key="i" class="px-1 py-2.5">
          <div class="flex items-center gap-2">
            <History class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            <span class="min-w-0 flex-1 truncate font-mono text-[12px]">{{ h.path }}</span>
            <span class="chip shrink-0" :class="opTone(h.op)">{{ h.op }} | {{ h.snapshotId }}</span>
          </div>
          <pre
            v-if="h.diff"
            class="mt-2 overflow-x-auto rounded-md bg-surface-muted p-2 font-mono text-[11px] leading-5 text-muted-foreground"
          >{{ h.diff }}</pre>
        </div>
      </div>

      <p v-else class="px-2 py-4 text-[12px] text-muted-foreground">
        No recorded history for this file yet.
      </p>
    </div>
  </div>
</template>