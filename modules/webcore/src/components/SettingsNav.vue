<script setup lang="ts">
import { useSettingsStore, SETTINGS_GROUPS } from '@/stores/settings'
import { useWorkspaceStore } from '@/stores/workspace'
import { projectName } from '@/lib/path'

/**
 * Settings navigation rail, rendered inside the shared left activity panel.
 * Lists the setting groups the content pane renders; selecting one updates the
 * settings store so rail and content stay in sync across the shell.
 */

const settings = useSettingsStore()
const workspace = useWorkspaceStore()
</script>

<template>
  <div class="flex h-full flex-col gap-2 bg-sidebar p-2">
    <p class="panel-heading mb-0.5 px-2">Settings</p>
    <ul class="space-y-0.5">
      <li v-for="g in SETTINGS_GROUPS" :key="g.key">
        <button
          class="flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[12.5px] transition-colors duration-150"
          :class="g.key === settings.active ? 'bg-accent text-accent-foreground' : 'text-muted-foreground hover:bg-hover hover:text-foreground'"
          type="button"
          @click="settings.select(g.key)"
        >
          <component :is="g.icon" class="h-4 w-4" />
          <span class="font-medium">{{ g.label }}</span>
        </button>
      </li>
    </ul>

    <div class="mt-auto px-2 text-[10.5px] text-subtle">
      {{ workspace.active ? projectName(workspace.active.path) : 'No workspace' }}
    </div>
  </div>
</template>