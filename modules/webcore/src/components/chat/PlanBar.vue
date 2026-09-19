<script setup lang="ts">
import { computed, ref } from 'vue'
import { Check, ChevronRight, ListChecks } from '@lucide/vue'
import type { TodoItem } from '@/core'

/**
 * The agent's plan, pinned above the composer.
 *
 * Living next to the input (rather than scrolling away in the transcript) means
 * the reader always knows what the current step is for. The card shows the
 * steps, a progress bar and the counter; once everything is done it folds into
 * a single line, and it hides entirely when there is no plan.
 */
const props = defineProps<{ todos: TodoItem[] }>()

const collapsed = ref(false)

const done = computed(() => props.todos.filter((t) => t.status === 'completed').length)
const complete = computed(() => props.todos.length > 0 && done.value === props.todos.length)
const ratio = computed(() => (props.todos.length ? done.value / props.todos.length : 0))
const open = computed(() => !collapsed.value && !complete.value)
</script>

<template>
  <div v-if="todos.length" class="chat-plan">
    <button
      class="chat-plan-head"
      type="button"
      :aria-expanded="open"
      @click="collapsed = !collapsed"
    >
      <ChevronRight
        class="chat-plan-chevron"
        :class="open ? 'rotate-90' : ''"
      />
      <ListChecks class="h-4 w-4 shrink-0" />
      <span class="chat-plan-title">Plan</span>
      <span class="chat-plan-count">{{ done }}/{{ todos.length }}</span>
      <span class="chat-plan-bar" aria-hidden="true">
        <span class="chat-plan-bar-fill" :style="{ width: `${Math.round(ratio * 100)}%` }" />
      </span>
      <Check v-if="complete" class="chat-plan-done" />
    </button>
    <ul v-if="open" class="chat-plan-list">
      <li v-for="(todo, index) in todos" :key="index" class="chat-plan-item">
        <Check v-if="todo.status === 'completed'" class="chat-plan-icon text-status-success" />
        <span
          v-else-if="todo.status === 'in_progress'"
          class="chat-plan-icon"
          aria-hidden="true"
        >
          <span class="chat-plan-dot" />
        </span>
        <span v-else class="chat-plan-icon" aria-hidden="true">
          <span class="chat-plan-ring" />
        </span>
        <span class="chat-plan-text" :class="`chat-plan-text-${todo.status}`">
          {{ todo.status === 'in_progress' && todo.activeForm ? todo.activeForm : todo.content }}
        </span>
      </li>
    </ul>
  </div>
</template>
