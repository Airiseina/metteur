<script setup lang="ts">
import { Check, FileCode, Image, ListChecks, Plug, Plus, Sparkles, X } from '@lucide/vue'
import type { AddonInfo, FileTreeNode, TodoItem } from '@/core'

/**
 * Add context.
 *
 * Everything that can travel with a turn is reachable from one menu, and each
 * entry describes itself by what the agent will see: a file becomes an `@`
 * reference, an addon makes its capabilities available to the turn, the plan
 * hands over the current task list. Entries that have nothing to offer are
 * disabled rather than hidden, so the menu does not change shape under the
 * pointer.
 */
const props = defineProps<{
  files: FileTreeNode[]
  addons: AddonInfo[]
  todos: TodoItem[]
  attachedPaths: string[]
  open: boolean
  onToggle: () => void
  onPickFile: (file: FileTreeNode) => void
  onAttachPlan: () => void
  onOpenPlugins: () => void
  onRemove: (path: string) => void
}>()

/** The three most recently listed files, as one-click suggestions. */
const suggestions = props.files.slice(0, 3)
</script>

<template>
  <div class="relative">
    <button
      class="chat-control chat-control-icon"
      type="button"
      :aria-expanded="open"
      aria-label="Add context"
      @click="onToggle"
    >
      <Plus class="h-4 w-4 shrink-0" />
    </button>

    <div v-if="open" class="chat-menu chat-menu-up w-72">
      <p class="chat-menu-head">Add context</p>

      <button class="chat-menu-item" type="button" @click="onToggle">
        <FileCode class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">
          <span class="block">Files and folders</span>
          <span class="block text-[11.5px] text-subtle">Type @ in the message to search</span>
        </span>
      </button>

      <button
        class="chat-menu-item"
        type="button"
        :disabled="!todos.length"
        :class="!todos.length ? 'opacity-50' : ''"
        @click="onAttachPlan"
      >
        <ListChecks class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">
          <span class="block">Current plan</span>
          <span class="block text-[11.5px] text-subtle">
            {{ todos.length ? `${todos.length} task(s) as context` : 'No plan yet' }}
          </span>
        </span>
      </button>

      <button class="chat-menu-item" type="button" @click="onOpenPlugins">
        <Plug class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">
          <span class="block">Capabilities</span>
          <span class="block text-[11.5px] text-subtle">
            {{ addons.length ? `${addons.filter((a) => a.enabled).length} of ${addons.length} plugins enabled` : 'Manage plugins' }}
          </span>
        </span>
      </button>

      <button class="chat-menu-item opacity-50" type="button" disabled>
        <Image class="h-4 w-4 shrink-0 text-muted-foreground" />
        <span class="min-w-0 flex-1">
          <span class="block">Image</span>
          <span class="block text-[11.5px] text-subtle">Paste an image into the message</span>
        </span>
      </button>

      <template v-if="suggestions.length">
        <div class="my-1.5 border-t border-divider" />
        <p class="chat-menu-head">
          <Sparkles class="h-3 w-3" /> From this workspace
        </p>
        <button
          v-for="file in suggestions"
          :key="file.path"
          class="chat-menu-item"
          type="button"
          @click="onPickFile(file)"
        >
          <FileCode class="h-4 w-4 shrink-0 text-subtle" />
          <span class="min-w-0 flex-1 truncate font-mono text-[12px]">{{ file.path }}</span>
          <Check v-if="attachedPaths.includes(file.path)" class="h-3.5 w-3.5 shrink-0 text-primary" />
        </button>
      </template>

      <template v-if="attachedPaths.length">
        <div class="my-1.5 border-t border-divider" />
        <p class="chat-menu-head">Attached</p>
        <div v-for="path in attachedPaths" :key="path" class="chat-menu-item">
          <span class="min-w-0 flex-1 truncate font-mono text-[12px]">{{ path }}</span>
          <button
            class="rounded p-0.5 text-subtle hover:text-foreground"
            type="button"
            :aria-label="`Remove ${path}`"
            @click="onRemove(path)"
          >
            <X class="h-3.5 w-3.5" />
          </button>
        </div>
      </template>
    </div>
  </div>
</template>
