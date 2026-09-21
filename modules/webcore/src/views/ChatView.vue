<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { Sparkles } from '@lucide/vue'
import { useChatStore } from '@/stores/chat'
import { useWorkspaceStore } from '@/stores/workspace'
import { useConfigStore } from '@/stores/config'
import { useTabsStore } from '@/stores/tabs'
import { useFeedbackStore } from '@/stores/feedback'
import { fileRoute } from '@/lib/file-token'
import { onHighlightReady, warmHighlighter } from '@/lib/markdown'
import { clearRenderCache } from '@/lib/chat/stream-render'
import ChatHeader from '@/components/chat/ChatHeader.vue'
import ChatThread from '@/components/chat/ChatThread.vue'
import ChatComposer from '@/components/chat/ChatComposer.vue'
import ChatStatusBar from '@/components/chat/ChatStatusBar.vue'
import PlanBar from '@/components/chat/PlanBar.vue'
import type { ChatOptions, FileTreeNode, LlmModelConfig } from '@/core'

/**
 * The ReAct chat surface.
 *
 * Orchestration only: the transcript, composer, header and status line own their
 * own behaviour, and the store owns the conversation. What lives here is the
 * wiring between them — model selection, the keyboard contract, and the
 * parameter dialog.
 */
const chat = useChatStore()
const workspace = useWorkspaceStore()
const config = useConfigStore()
const tabs = useTabsStore()
const feedback = useFeedbackStore()
const router = useRouter()

const composer = ref<InstanceType<typeof ChatComposer>>()

/* Models ---------------------------------------------------------------- */

const modelTable = computed<Record<string, LlmModelConfig>>(
  () => (config.effective.llm?.models as Record<string, LlmModelConfig> | undefined) ?? {},
)
const modelKeys = computed(() => {
  const keys = Object.keys(modelTable.value)
  const fallback = config.effective.llm?.default_model
  if (fallback && keys.includes(fallback)) return [fallback, ...keys.filter((k) => k !== fallback)]
  return keys
})
const hasModel = computed(() => modelKeys.value.length > 0)

const model = ref('')
const reasoning = ref<ChatOptions['reasoning_effort']>('medium')
const temperature = ref(0.7)
const topP = ref(0.9)
const maxTokens = ref(4096)
const parametersOpen = ref(false)

const chatOptions = computed<ChatOptions>(() => ({
  // The selection can lag the configuration (a workspace that just opened, a
  // model removed from settings); the first configured key is what the
  // selector would show anyway.
  model: model.value || modelKeys.value[0] || '',
  reasoning_effort: reasoning.value,
  temperature: temperature.value,
  top_p: topP.value,
  max_tokens: maxTokens.value,
  permission_mode: chat.permissionMode,
}))

/* Turn lifecycle -------------------------------------------------------- */

const running = computed(() => chat.streaming)

/** Sends a turn, or reports why it cannot. */
async function send(text: string): Promise<void> {
  if (!hasModel.value) {
    feedback.toast('error', 'No model configured', 'Add a model under Settings → LLM & Models.')
    openSettings()
    return
  }
  await chat.send(text, chatOptions.value)
}

/** Queues a message into the running turn (Enter while the agent works). */
async function queue(text: string): Promise<void> {
  const ok = await chat.queue(text, chat.pendingFiles ?? [])
  if (!ok) feedback.toast('error', 'Could not queue the message', 'The turn may have just ended.')
  if (chat.pendingFiles.length) for (const file of [...chat.pendingFiles]) chat.removeQueuedFile(file.path)
}

/** Sends a message into the running turn before the next model call. */
async function steer(text: string): Promise<void> {
  const ok = await chat.steer(text)
  if (!ok) feedback.toast('error', 'Could not send the message', 'The turn may have just ended.')
}

function stop(): void {
  void chat.abort()
}

function copy(text: string): void {
  void navigator.clipboard.writeText(text)
}

/** Puts a turn back in the composer for editing. */
function edit(text: string): void {
  composer.value?.setDraft(text)
}

/** Re-sends the user turn that produced this answer. */
function retry(assistantId: string): void {
  const index = chat.messages.findIndex((m) => m.id === assistantId)
  for (let i = index - 1; i >= 0; i--) {
    const candidate = chat.messages[i]
    if (candidate.role === 'user') {
      edit(candidate.content)
      return
    }
  }
}

function openFile(path: string | undefined): void {
  if (!path) return
  tabs.openFile(path)
  void router.push(fileRoute(path))
}

function openSettings(): void {
  void router.push('/settings')
}

/**
 * Rolls the workspace back to the checkpoint a turn started from.
 *
 * Restoring rewrites files, so it is confirmed first and reported afterwards;
 * the conversation itself is left alone (only the workspace moves).
 */
async function restore(snapshotId: string): Promise<void> {
  const confirmed = await feedback.confirm({
    header: 'Restore the workspace?',
    message:
      'Files changed since this turn started will be rolled back. The conversation stays as it is.',
    acceptLabel: 'Restore',
    danger: true,
  })
  if (!confirmed) return
  const ok = await workspace.restoreCheckpoint(snapshotId)
  if (ok) feedback.toast('success', 'Workspace restored', 'Files are back to the state before that turn.')
  else feedback.toast('error', 'Restore failed', 'The checkpoint may have been removed.')
}

function attach(file: FileTreeNode): void {
  chat.queueFile(file)
}

/**
 * Hands the current plan to the turn.
 *
 * The plan is already in the agent's context when it wrote it; attaching it as
 * a message is what makes it explicit for a turn that follows a compaction.
 */
function attachPlan(): void {
  const plan = chat.todos
  if (!plan.length) return
  const lines = plan.map((todo, i) => `${i + 1}. [${todo.status}] ${todo.content}`)
  composer.value?.setDraft(
    `Current plan:
${lines.join('\n')}
\n
`,
  )
}

/* Keyboard ------------------------------------------------------------- */

/**
 * Global keys for the surface: `Esc` interrupts, `Ctrl+O` folds every detail
 * back in, and `Ctrl+K` focuses the composer from anywhere in the page.
 */
function onKeydown(event: KeyboardEvent): void {
  const mod = event.ctrlKey || event.metaKey
  if (event.key === 'Escape' && running.value) {
    // The draft is otherwise lost on interrupt, which is the opposite of what
    // "stop" should cost the user.
    event.preventDefault()
    stop()
    return
  }
  if (!mod) return
  if (event.key.toLowerCase() === 'k') {
    event.preventDefault()
    composer.value?.focus()
    return
  }
  if (event.key.toLowerCase() === 'o') {
    event.preventDefault()
    // Collapsing everything is a re-render, not a state change: tool rows and
    // reasoning blocks keep their own state, so a remount resets them.
    rerender.value += 1
  }
}

/** Bumped to remount the transcript (folds every expanded detail). */
const rerender = ref(0)

onMounted(() => {
  void chat.loadFiles()
  void chat.loadAddons()
  window.addEventListener('keydown', onKeydown)
  warmHighlighter()
  onHighlightReady(() => {
    clearRenderCache()
    rerender.value += 1
  })
  void nextTick(() => composer.value?.focus())
})
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))

const starterPrompts = [
  { label: 'Explain this workspace', text: 'Explain how this workspace is organised and what the main entry points are.' },
  { label: 'Find and fix a bug', text: 'Find the most likely bug in the modified files and fix it.' },
  { label: 'Plan a change', text: 'Plan how to add a new feature end to end, then implement the first step.' },
]
</script>

<template>
  <div class="flex h-full flex-col bg-background">
    <ChatHeader
      :threads="chat.threads"
      :session-id="chat.sessionId"
      :active-title="chat.threads.find((t) => t.sessionId === chat.sessionId)?.title || 'New chat'"
      :addons="chat.addons"
      :addons-loading="chat.addonsLoading"
      :busy="chat.busy"
      :on-select-session="(id) => void chat.switchTo(id)"
      :on-delete-session="(id) => void chat.deleteThread(id)"
      :on-new-session="() => void chat.clear()"
      :on-toggle-addon="(id, enabled) => void chat.toggleAddon(id, enabled)"
    />

    <ChatThread
      :key="rerender"
      :messages="chat.messages"
      :on-open-file="openFile"
      :on-copy="copy"
      :on-edit="edit"
      :on-retry="retry"
      :on-restore="(id) => void restore(id)"
    >
      <template #empty>
        <div class="flex flex-col items-center pt-14 text-center">
          <span class="chat-badge-none mb-4 grid h-12 w-12 place-items-center rounded-2xl bg-primary-soft text-primary">
            <Sparkles class="h-6 w-6" />
          </span>
          <h2 class="text-[16px] font-semibold tracking-tight">What should we work on?</h2>
          <p class="mt-1.5 max-w-md text-[12.5px] leading-relaxed text-muted-foreground">
            Describe a task, reference files with
            <span class="mono rounded bg-surface-muted px-1 py-0.5 text-[11.5px]">@</span>, insert a template with
            <span class="mono rounded bg-surface-muted px-1 py-0.5 text-[11.5px]">/</span>. The agent plans, edits and
            verifies with tools, and every step is shown as it runs.
          </p>
          <div v-if="hasModel" class="mt-6 grid w-full max-w-lg grid-cols-1 gap-2 sm:grid-cols-3">
            <button
              v-for="starter in starterPrompts"
              :key="starter.label"
              class="panel-muted px-3 py-2.5 text-left text-[12px] leading-snug transition-colors duration-100 hover:bg-hover"
              type="button"
              @click="edit(starter.text)"
            >
              <span class="block font-medium text-foreground">{{ starter.label }}</span>
              <span class="mt-0.5 block text-muted-foreground">{{ starter.text }}</span>
            </button>
          </div>
        </div>
      </template>
    </ChatThread>

    <div class="shrink-0">
      <div class="chat-column chat-column-composer">
        <ChatStatusBar
          :phase="chat.phase"
          :last-event-at="chat.lastEventAt"
          :usage="chat.lastUsage"
          :pending-tool="chat.pendingTool"
        />
        <PlanBar :todos="chat.todos" />
        <ChatComposer
          ref="composer"
          :files="chat.files"
          :pending-files="chat.pendingFiles"
          :addons="chat.addons"
          :todos="chat.todos"
          :models="modelTable"
          :model-keys="modelKeys"
          :model="model"
          :effort="reasoning"
          :context-stats="chat.contextStats"
          :usage="chat.lastUsage"
          :permission-mode="chat.permissionMode"
          :running="running"
          :ready="hasModel"
          :on-send="(text) => void send(text)"
          :on-queue="(text) => void queue(text)"
          :on-steer="(text) => void steer(text)"
          :on-stop="stop"
          :on-remove-file="(path) => chat.removeQueuedFile(path)"
          :on-attach="attach"
          :on-attach-plan="attachPlan"
          :on-open-plugins="() => openSettings()"
          :on-select-model="(key) => (model = key)"
          :on-select-effort="(key) => (reasoning = key)"
          :on-select-permission="(mode) => (chat.permissionMode = mode)"
          :on-open-parameters="() => (parametersOpen = true)"
          :on-open-settings="openSettings"
        />
        <p class="mt-2 px-1 text-[10.5px] text-subtle">
          Enter sends · Shift+Enter for a newline · Esc stops the turn · ⌘/Ctrl+K focuses the input
        </p>
      </div>
    </div>

    <!-- Generation parameters -->
    <div v-if="parametersOpen" class="fixed inset-0 z-50 grid place-items-center" style="background: var(--overlay)" @mousedown.self="parametersOpen = false">
      <div class="chat-menu w-80 p-3!">
        <p class="mb-2 text-[13px] font-semibold">Generation parameters</p>
        <div class="space-y-2">
          <label class="flex items-center justify-between text-[12px] text-muted-foreground">
            Reasoning
            <select v-model="reasoning" class="input h-6 w-28! px-1.5! text-[11px]!">
              <option value="none">none</option>
              <option value="low">low</option>
              <option value="medium">medium</option>
              <option value="high">high</option>
            </select>
          </label>
          <label class="flex items-center justify-between text-[12px] text-muted-foreground">
            Temperature
            <input v-model.number="temperature" type="number" step="0.1" min="0" max="2" class="input h-6 w-28! px-1.5! text-[11px]!" />
          </label>
          <label class="flex items-center justify-between text-[12px] text-muted-foreground">
            Top P
            <input v-model.number="topP" type="number" step="0.05" min="0" max="1" class="input h-6 w-28! px-1.5! text-[11px]!" />
          </label>
          <label class="flex items-center justify-between text-[12px] text-muted-foreground">
            Max tokens
            <input v-model.number="maxTokens" type="number" step="256" min="256" class="input h-6 w-28! px-1.5! text-[11px]!" />
          </label>
        </div>
        <div class="mt-3 flex justify-end">
          <button class="btn btn-primary h-7! px-3! text-[12px]" type="button" @click="parametersOpen = false">
            Done
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
