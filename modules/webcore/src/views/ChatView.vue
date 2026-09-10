<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  TriangleAlert,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  FileCode,
  Loader2,
  MessageSquarePlus,
  Paperclip,
  Plug,
  Send,
  Settings2,
  SlidersHorizontal,
  Sparkles,
  Square,
  User,
  X,
} from '@lucide/vue'
import { useChatStore } from '@/stores/chat'
import { useConfigStore } from '@/stores/config'
import { useTabsStore } from '@/stores/tabs'
import { fileRoute } from '@/lib/file-token'
import type { ChatOptions, FileTreeNode, LlmModelConfig } from '@/core'

const chat = useChatStore()
const tabs = useTabsStore()
const config = useConfigStore()
const router = useRouter()
const draft = ref('')
const scrollEl = ref<HTMLElement | null>(null)

/* Models --------------------------------------------------------------- */

/** Configured models, keyed by config key, in declaration order. */
const modelTable = computed<Record<string, LlmModelConfig>>(
  () => (config.effective.llm?.models as Record<string, LlmModelConfig> | undefined) ?? {},
)

/** Model keys with the default first. Empty when none are configured. */
const modelKeys = computed(() => {
  const keys = Object.keys(modelTable.value)
  const def = config.effective.llm?.default_model
  if (def && keys.includes(def)) return [def, ...keys.filter((k) => k !== def)]
  return keys
})

/** Whether a usable model exists; drives the "configure a model" state. */
const hasModel = computed(() => modelKeys.value.length > 0)

/** Human label for a model key (display name when set). */
function modelLabel(key: string): string {
  return modelTable.value[key]?.display_name || key
}

const model = ref('')
watch(
  modelKeys,
  (keys) => {
    if (!model.value || !keys.includes(model.value)) model.value = keys[0] ?? ''
  },
  { immediate: true },
)

const reasoning = ref<ChatOptions['reasoning_effort']>('medium')
const temperature = ref(0.7)
const topP = ref(0.9)
const maxTokens = ref(4096)
const paramsOpen = ref(false)

/* Popovers ------------------------------------------------------------- */

const filePickerOpen = ref(false)
const pluginOpen = ref(false)
const sessionOpen = ref(false)
const modelOpen = ref(false)

/** Collapsed/expanded tool-call cards, keyed by message id. */
const expandedTools = ref<Set<string>>(new Set())

/* Flattened file list (attach + @-mention) ------------------------------ */

interface FileEntry {
  name: string
  path: string
  depth: number
}

/** Recursively flatten the workspace tree into a searchable file list. */
function flattenTree(nodes: FileTreeNode[], depth: number): FileEntry[] {
  const out: FileEntry[] = []
  for (const n of nodes) {
    if (n.kind === 'file') out.push({ name: n.name, path: n.path, depth })
    if (n.children) out.push(...flattenTree(n.children, depth + 1))
  }
  return out
}

const fileEntries = computed<FileEntry[]>(() => flattenTree(chat.files, 0))

/* Slash commands ------------------------------------------------------- */

const COMMANDS = [
  { label: '/build', desc: 'Build the workspace' },
  { label: '/analyze', desc: 'Analyze the workspace' },
  { label: '/plan', desc: 'Plan a task step by step' },
  { label: '/blueprint', desc: 'Generate a blueprint' },
  { label: '/fix', desc: 'Diagnose and fix an issue' },
]

/** The last whitespace-delimited token the user is currently typing. */
function currentToken(): string {
  return draft.value.split(/[\s\n]/).pop() ?? ''
}

const mentionQuery = computed(() => (currentToken().startsWith('@') ? currentToken().slice(1).toLowerCase() : ''))
const commandQuery = computed(() => (currentToken().startsWith('/') ? currentToken().slice(1).toLowerCase() : ''))
const mentionOpen = computed(() => mentionQuery.value !== '')
const commandOpen = computed(() => commandQuery.value !== '')

const filteredFiles = computed(() => {
  const q = mentionQuery.value
  if (!q) return fileEntries.value.slice(0, 40)
  return fileEntries.value.filter((e) => e.name.toLowerCase().includes(q)).slice(0, 40)
})
const filteredCommands = computed(() => {
  const q = commandQuery.value
  if (!q) return COMMANDS
  return COMMANDS.filter((c) => c.label.slice(1).includes(q))
})

/** Cut the trailing `@xx` / `/xx` word and any leading delimiter space. */
function stripTrailingWord() {
  const i = draft.value.search(/[\s@/][^\s]*$/)
  draft.value = i < 0 ? '' : draft.value.slice(0, i)
}

/** @-mention select: queue the file as a composer chip, drop the token. */
function pickFile(e: FileEntry) {
  stripTrailingWord()
  chat.queueFile({ name: e.name, path: e.path, kind: 'file' })
  textarea?.focus()
}

/** `/`-command select: inline the command label, drop the typed token. */
function pickCommand(cmd: { label: string }) {
  stripTrailingWord()
  draft.value = draft.value ? `${draft.value.trim()} ${cmd.label}` : cmd.label
  textarea?.focus()
}

/** Select a file from the attach (`+`) popover and queue it as a chip. */
function attachFile(e: FileEntry) {
  filePickerOpen.value = false
  chat.queueFile({ name: e.name, path: e.path, kind: 'file' })
}

function closeMenus() {
  filePickerOpen.value = false
  pluginOpen.value = false
  sessionOpen.value = false
  modelOpen.value = false
}

const enabledAddons = computed(() => chat.addons.filter((a) => a.enabled))

async function toggleAddon(name: string, enabled: boolean) {
  await chat.toggleAddon(name, enabled)
}

function toggleTool(id: string) {
  const next = new Set(expandedTools.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expandedTools.value = next
}
function isToolOpen(id: string): boolean {
  return expandedTools.value.has(id)
}

/** Split a user message into a mix of `@file` chips and plain text lines. */
function renderUserContent(content: string): Array<{ kind: 'text'; text: string } | { kind: 'file'; path: string }> {
  const parts: Array<{ kind: 'text'; text: string } | { kind: 'file'; path: string }> = []
  let text = ''
  for (const line of content.split('\n')) {
    const m = line.match(/^@file\s+(.+)$/)
    if (m) {
      if (text) {
        parts.push({ kind: 'text', text })
        text = ''
      }
      parts.push({ kind: 'file', path: m[1].trim() })
    } else {
      text = text ? `${text}\n${line}` : line
    }
  }
  if (text) parts.push({ kind: 'text', text })
  return parts
}

/* Send / lifecycle ----------------------------------------------------- */

function chatOptions(): ChatOptions {
  return {
    model: model.value,
    reasoning_effort: reasoning.value,
    temperature: temperature.value,
    top_p: topP.value,
    max_tokens: maxTokens.value,
  }
}

/** Whether the composer can send right now. */
const canSend = computed(() => hasModel.value && draft.value.trim().length > 0 && !chat.streaming)

async function send() {
  if (!canSend.value) return
  const text = draft.value.trim()
  closeMenus()
  draft.value = ''
  await chat.send(text, chatOptions())
  scrollToBottom()
}

function scrollToBottom() {
  nextTick(() => scrollEl.value?.scrollTo({ top: scrollEl.value.scrollHeight }))
}

function openFile(path: string | undefined) {
  if (!path) return
  tabs.openFile(path)
  router.push(fileRoute(path))
}

function openSettings() {
  router.push('/settings')
}

let textarea: HTMLTextAreaElement | null = null
function onTextareaMount(el: unknown) {
  textarea = el instanceof HTMLTextAreaElement ? el : null
}

function autosize() {
  if (!textarea) return
  textarea.style.height = 'auto'
  textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`
}
watch(draft, () => nextTick(autosize))

async function onComposerKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    await send()
    return
  }
  if (e.key === 'Escape') closeMenus()
}

async function copyMessage(content: string) {
  await navigator.clipboard.writeText(content)
}

/** Short relative timestamp for message headers. */
function timeOf(ms: number): string {
  const d = new Date(ms)
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

onMounted(() => {
  void chat.loadFiles()
  void chat.loadAddons()
  nextTick(autosize)
})
</script>

<template>
  <div class="flex h-full flex-col bg-background">
    <!-- ── Header ──────────────────────────────────────────────────────── -->
    <header class="flex h-12 shrink-0 items-center gap-2 border-b border-divider px-3">
      <span
        class="flex h-6 w-6 items-center justify-center rounded-lg text-primary-foreground"
        style="background: var(--primary)"
      >
        <Bot class="h-3.5 w-3.5" />
      </span>
      <span class="text-[13px] font-semibold tracking-tight">Agent</span>
      <span class="chip" title="Execution paradigm">ReAct</span>

      <div class="ml-auto flex items-center gap-1">
        <!-- Session switcher -->
        <div class="relative">
          <button
            class="btn btn-ghost h-7! max-w-48! gap-1.5 px-2! text-[12px]"
            type="button"
            :disabled="chat.busy || chat.threads.length === 0"
            title="Chat sessions"
            @click="sessionOpen = !sessionOpen; modelOpen = false; pluginOpen = false"
          >
            <span class="truncate">
              {{ chat.threads.find((t) => t.sessionId === chat.sessionId)?.title || 'New chat' }}
            </span>
            <ChevronDown class="h-3 w-3 shrink-0 text-subtle" />
          </button>

          <div v-if="sessionOpen" class="menu-card absolute right-0 top-9 w-80">
            <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Sessions</p>
            <p v-if="!chat.threads.length" class="px-2 py-1.5 text-[12px] text-subtle">
              No saved conversations yet.
            </p>
            <div
              v-for="t in chat.threads"
              :key="t.sessionId"
              class="group/thread flex items-center gap-1 rounded-md hover:bg-hover"
            >
              <button
                class="flex min-w-0 flex-1 items-center gap-2 px-2 py-1.5 text-left"
                type="button"
                @click="chat.switchTo(t.sessionId); sessionOpen = false"
              >
                <Check v-if="t.sessionId === chat.sessionId" class="h-3.5 w-3.5 shrink-0" style="color: var(--primary)" />
                <span v-else class="w-3.5 shrink-0" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[12.5px]">{{ t.title || '(untitled)' }}</span>
                  <span class="block text-[10.5px] text-subtle">{{ t.messageCount }} messages · {{ t.turns }} turns</span>
                </span>
              </button>
              <button
                class="mr-1 grid h-6 w-6 shrink-0 place-items-center rounded text-subtle opacity-0 transition-opacity hover:text-danger group-hover/thread:opacity-100"
                type="button"
                :aria-label="`Delete ${t.title || 'conversation'}`"
                title="Delete conversation"
                @click="chat.deleteThread(t.sessionId)"
              >
                <X class="h-3.5 w-3.5" />
              </button>
            </div>
            <div class="my-1 border-t border-divider" />
            <button
              class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12.5px] hover:bg-hover"
              type="button"
              @click="chat.clear(); sessionOpen = false"
            >
              <MessageSquarePlus class="h-3.5 w-3.5" /> New chat
            </button>
          </div>
        </div>

        <!-- New chat -->
        <button
          class="btn-icon h-7! w-7!"
          type="button"
          :disabled="chat.busy"
          title="New chat"
          aria-label="New chat"
          @click="chat.clear()"
        >
          <MessageSquarePlus class="h-4 w-4" />
        </button>

        <!-- Plugins -->
        <div class="relative">
          <button
            class="btn-icon relative h-7! w-7!"
            type="button"
            title="Plugins"
            aria-label="Plugins"
            :class="pluginOpen ? 'text-foreground!' : ''"
            @click="pluginOpen = !pluginOpen; filePickerOpen = false; sessionOpen = false; modelOpen = false"
          >
            <Plug class="h-3.5 w-3.5" />
            <span
              v-if="enabledAddons.length"
              class="absolute -right-0.5 -top-0.5 flex h-3.5 min-w-3.5 items-center justify-center rounded-full px-0.5 text-[9px] font-semibold text-primary-foreground"
              style="background: var(--primary)"
            >
              {{ enabledAddons.length }}
            </span>
          </button>

          <div v-if="pluginOpen" class="menu-card absolute right-0 top-9 w-72">
            <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Plugins</p>
            <div v-if="chat.addonsLoading" class="flex items-center gap-1.5 px-2 py-2 text-[12px] text-subtle">
              <Loader2 class="h-3.5 w-3.5 animate-spin" /> Loading…
            </div>
            <p v-else-if="!chat.addons.length" class="px-2 py-2 text-[12px] text-subtle">
              No plugins installed.
            </p>
            <div
              v-for="a in chat.addons"
              :key="a.id"
              class="flex items-center gap-2.5 rounded-md px-2 py-1.5 hover:bg-hover"
            >
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-1.5">
                  <span class="truncate text-[12.5px] font-medium">{{ a.name }}</span>
                  <span v-if="a.scope" class="chip text-[9.5px]!">{{ a.scope }}</span>
                </div>
                <p class="text-[11px] text-subtle">v{{ a.version }}</p>
              </div>
              <button
                class="flex h-4 w-7 shrink-0 items-center rounded-full p-0.5 transition-colors duration-150"
                type="button"
                role="switch"
                :aria-checked="a.enabled"
                :style="a.enabled ? { background: 'var(--primary)' } : { background: 'var(--surface-muted)' }"
                @click="toggleAddon(a.id, !a.enabled)"
              >
                <span
                  class="h-3 w-3 rounded-full bg-white shadow transition-transform duration-150"
                  :class="a.enabled ? 'translate-x-3' : ''"
                />
              </button>
            </div>
          </div>
        </div>

        <!-- Model & parameters -->
        <div class="relative">
          <button
            class="btn btn-ghost h-7! max-w-56! gap-1.5 px-2! text-[12px]"
            type="button"
            title="Model and parameters"
            @click="modelOpen = !modelOpen; pluginOpen = false; sessionOpen = false; filePickerOpen = false"
          >
            <SlidersHorizontal class="h-3.5 w-3.5 shrink-0" />
            <span class="truncate font-medium">{{ hasModel ? modelLabel(model) : 'No model' }}</span>
            <ChevronDown class="h-3 w-3 shrink-0 text-subtle" />
          </button>

          <div v-if="modelOpen" class="menu-card absolute right-0 top-9 w-80">
            <template v-if="hasModel">
              <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Model</p>
              <div class="max-h-56 overflow-y-auto">
                <button
                  v-for="key in modelKeys"
                  :key="key"
                  class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                  type="button"
                  @click="model = key"
                >
                  <Check v-if="key === model" class="h-3.5 w-3.5 shrink-0" style="color: var(--primary)" />
                  <span v-else class="w-3.5 shrink-0" />
                  <span class="min-w-0 flex-1 truncate text-[12.5px]">{{ modelLabel(key) }}</span>
                  <span class="shrink-0 text-[10px] text-subtle">{{ modelTable[key]?.model_id || key }}</span>
                </button>
              </div>

              <div class="my-1.5 border-t border-divider" />
              <p class="mb-1.5 px-2 text-[11px] font-medium text-muted-foreground">Parameters</p>
              <div class="space-y-2 px-2 pb-1">
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
            </template>

            <!-- No configured model -->
            <template v-else>
              <p class="flex items-center gap-1.5 px-2 py-1 text-[12px] font-medium text-danger">
                <TriangleAlert class="h-3.5 w-3.5" /> No model configured
              </p>
              <p class="px-2 pb-2 text-[11.5px] leading-relaxed text-muted-foreground">
                Add a model under Settings → LLM &amp; Models to start chatting.
              </p>
              <button class="btn btn-primary ml-2 mb-1 h-7! w-[calc(100%-1rem)]! text-[12px]" type="button" @click="openSettings">
                Open settings
              </button>
            </template>
          </div>
        </div>
      </div>
    </header>

    <!-- ── Thread ──────────────────────────────────────────────────────── -->
    <div ref="scrollEl" class="min-h-0 flex-1 overflow-y-auto">
      <div class="mx-auto flex w-full max-w-3xl flex-col gap-5 px-4 py-6">
        <!-- Empty state -->
        <div v-if="!chat.messages.length" class="flex flex-col items-center pt-10 text-center">
          <span
            class="mb-4 flex h-14 w-14 items-center justify-center rounded-2xl text-primary-foreground"
            style="background: linear-gradient(140deg, var(--primary), var(--ring))"
          >
            <Sparkles class="h-7 w-7" />
          </span>
          <h2 class="text-[17px] font-semibold tracking-tight">What should we build?</h2>
          <p class="mt-1.5 max-w-md text-[12.5px] leading-relaxed text-muted-foreground">
            Describe a task, reference files with
            <span class="mono rounded bg-surface-muted px-1 py-0.5 text-[11.5px]">@</span>, or run a command with
            <span class="mono rounded bg-surface-muted px-1 py-0.5 text-[11.5px]">/</span>. Agents run tools in the sandbox
            and can draft blueprints for review.
          </p>

          <div v-if="hasModel" class="mt-6 grid w-full max-w-lg grid-cols-2 gap-2">
            <button
              v-for="c in COMMANDS"
              :key="c.label"
              class="panel-muted flex items-start gap-2.5 px-3 py-2.5 text-left transition-colors duration-150 hover:bg-hover"
              type="button"
              @click="draft = c.label + ' '; textarea?.focus()"
            >
              <span class="mono mt-0.5 shrink-0 text-[11.5px]" style="color: var(--primary)">{{ c.label }}</span>
              <span class="text-[11.5px] leading-snug text-muted-foreground">{{ c.desc }}</span>
            </button>
          </div>

          <!-- Blocking hint when nothing is configured -->
          <div v-else class="panel mt-6 flex max-w-md flex-col items-center gap-2 px-5 py-4">
            <p class="flex items-center gap-1.5 text-[12.5px] font-medium text-danger">
              <TriangleAlert class="h-4 w-4" /> No model configured
            </p>
            <p class="text-[12px] leading-relaxed text-muted-foreground">
              Chat needs at least one model. Add an endpoint, model id and API key in Settings.
            </p>
            <button class="btn btn-primary mt-1 h-8! px-4! text-[12.5px]" type="button" @click="openSettings">
              Open settings
            </button>
          </div>
        </div>

        <template v-for="m in chat.messages" :key="m.id">
          <!-- User turn -->
          <div v-if="m.role === 'user'" class="group flex flex-col items-end gap-1">
            <div
              class="max-w-[85%] rounded-2xl rounded-br-md px-3.5 py-2.5 text-[13px] leading-relaxed text-primary-foreground"
              style="background: var(--primary)"
            >
              <template v-for="(part, i) in renderUserContent(m.content)" :key="i">
                <button
                  v-if="part.kind === 'file'"
                  class="mb-0.5 mr-1 inline-flex items-center gap-1 rounded-md bg-white/20 px-1.5 py-0.5 font-mono text-[11.5px] underline decoration-dotted underline-offset-2 hover:bg-white/30"
                  type="button"
                  @click="openFile(part.path)"
                >
                  <FileCode class="h-3 w-3" /> {{ part.path }}
                </button>
                <template v-else>{{ part.text }}</template>
              </template>
              <span
                v-if="m.pending"
                class="ml-0.5 inline-block h-3 w-1.5 animate-pulse align-middle bg-white/80"
              />
            </div>
            <div class="flex items-center gap-1.5 pr-1 text-[10.5px] text-subtle">
              <span>{{ timeOf(m.createdAt) }}</span>
              <button
                class="rounded p-0.5 opacity-0 transition-opacity duration-150 hover:text-foreground group-hover:opacity-100"
                type="button"
                title="Copy"
                aria-label="Copy"
                @click="copyMessage(m.content)"
              >
                <Copy class="h-3 w-3" />
              </button>
            </div>
          </div>

          <!-- Assistant turn -->
          <div v-else-if="m.role === 'assistant'" class="group flex gap-3">
            <span
              class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg text-primary-foreground"
              style="background: var(--primary)"
            >
              <Bot class="h-4 w-4" />
            </span>
            <div class="min-w-0 flex-1">
              <div class="mb-1 flex items-center gap-2 text-[11px] text-subtle">
                <span class="font-medium text-foreground/80">Agent</span>
                <span>{{ timeOf(m.createdAt) }}</span>
              </div>
              <div class="whitespace-pre-wrap text-[13.5px] leading-6 text-foreground">{{ m.content }}</div>
              <span v-if="m.pending" class="ml-0.5 inline-block h-3.5 w-1.5 animate-pulse align-middle" style="background: var(--primary)" />
              <div v-if="m.detail && m.detail.model" class="mt-1 text-[10.5px] text-subtle">
                {{ m.detail.model }}
              </div>
              <div class="mt-1.5 flex gap-1">
                <button
                  class="flex h-6 items-center gap-1 rounded-md px-1.5 text-[11px] text-subtle opacity-0 transition-opacity duration-150 hover:bg-hover hover:text-foreground group-hover:opacity-100"
                  type="button"
                  title="Copy"
                  aria-label="Copy"
                  @click="copyMessage(m.content)"
                >
                  <Copy class="h-3 w-3" /> Copy
                </button>
              </div>
            </div>
          </div>

          <!-- Tool call card -->
          <div v-else class="group flex gap-3">
            <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg" style="background: var(--primary-soft)">
              <SlidersHorizontal class="h-4 w-4" style="color: var(--primary)" />
            </span>
            <div class="min-w-0 flex-1">
              <div class="overflow-hidden rounded-xl" style="background: var(--surface-muted); box-shadow: inset 0 0 0 1px var(--divider)">
                <button
                  class="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors duration-150 hover:bg-hover"
                  type="button"
                  @click="toggleTool(m.id)"
                >
                  <ChevronRight
                    class="h-3.5 w-3.5 shrink-0 text-subtle transition-transform duration-150"
                    :class="isToolOpen(m.id) ? 'rotate-90' : ''"
                  />
                  <span class="mono truncate text-[12px] font-semibold" style="color: var(--primary)">{{ m.actor }}</span>
                  <Loader2 v-if="m.pending" class="h-3 w-3 shrink-0 animate-spin text-subtle" />
                  <span class="ml-auto shrink-0 text-[10.5px] text-subtle">{{ m.pending ? 'running' : 'done' }}</span>
                </button>
                <div v-if="isToolOpen(m.id)" class="border-t border-divider px-3 py-2.5">
                  <div class="mb-1.5 flex items-center justify-between">
                    <span class="text-[10.5px] font-medium uppercase tracking-wide text-subtle">Output</span>
                    <button
                      v-if="m.detail && typeof m.detail.file === 'string'"
                      class="text-[11px] underline decoration-dotted underline-offset-2 hover:no-underline"
                      style="color: var(--primary)"
                      type="button"
                      @click="openFile(m.detail.file as string)"
                    >
                      open file
                    </button>
                  </div>
                  <pre class="max-h-72 overflow-auto whitespace-pre-wrap font-mono text-[11.5px] leading-relaxed text-foreground/90">{{ m.content }}</pre>
                </div>
              </div>
            </div>
          </div>
        </template>

        <!-- Working indicator -->
        <div v-if="chat.streaming" class="flex items-center gap-2 pl-10 text-[12px] text-subtle">
          <Loader2 class="h-3.5 w-3.5 animate-spin" style="color: var(--primary)" />
          Agent is working…
        </div>
      </div>
    </div>

    <!-- ── Composer ────────────────────────────────────────────────────── -->
    <div class="shrink-0 px-4 pb-4">
      <div class="mx-auto w-full max-w-3xl">
        <div class="composer relative rounded-2xl p-2.5" style="background: var(--surface); box-shadow: var(--shadow-card), inset 0 0 0 1px var(--divider)">
          <!-- @ / slash menu -->
          <div v-if="mentionOpen || commandOpen" class="menu-card absolute bottom-full left-2 mb-2 max-h-64 w-80 overflow-y-auto">
            <template v-if="mentionOpen">
              <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Reference a file</p>
              <p v-if="!filteredFiles.length" class="px-2 py-1.5 text-[12px] text-subtle">No matches</p>
              <button
                v-for="e in filteredFiles"
                :key="e.path"
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                type="button"
                @mousedown.prevent="pickFile(e)"
              >
                <FileCode class="h-3.5 w-3.5 shrink-0 text-subtle" />
                <span class="min-w-0 flex-1 truncate text-[12.5px]" :style="{ paddingLeft: e.depth * 10 + 'px' }">
                  {{ e.name }}
                </span>
                <Check v-if="chat.pendingFiles.some((f) => f.path === e.path)" class="h-3.5 w-3.5 shrink-0" style="color: var(--primary)" />
              </button>
            </template>
            <template v-else>
              <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Commands</p>
              <p v-if="!filteredCommands.length" class="px-2 py-1.5 text-[12px] text-subtle">No commands</p>
              <button
                v-for="c in filteredCommands"
                :key="c.label"
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                type="button"
                @mousedown.prevent="pickCommand(c)"
              >
                <span class="mono w-20 shrink-0 text-[12px]" style="color: var(--primary)">{{ c.label }}</span>
                <span class="truncate text-[12px] text-muted-foreground">{{ c.desc }}</span>
              </button>
            </template>
          </div>

          <!-- Queued file chips -->
          <div v-if="chat.pendingFiles.length" class="mb-2 flex flex-wrap gap-1.5">
            <span
              v-for="f in chat.pendingFiles"
              :key="f.path"
              class="inline-flex max-w-full items-center gap-1 rounded-md px-2 py-1 font-mono text-[11.5px]"
              style="background: var(--primary-soft); color: var(--primary)"
            >
              <FileCode class="h-3 w-3 shrink-0" />
              <span class="truncate">{{ f.path }}</span>
              <button
                class="ml-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded hover:bg-black/10"
                type="button"
                :aria-label="'Remove ' + f.path"
                @click="chat.removeQueuedFile(f.path)"
              >
                <X class="h-3 w-3" />
              </button>
            </span>
          </div>

          <!-- Input row -->
          <div class="flex items-end gap-1.5">
            <div class="relative">
              <button
                class="btn-icon h-8! w-8! shrink-0"
                type="button"
                title="Attach a file"
                aria-label="Attach a file"
                :class="filePickerOpen ? 'text-foreground!' : ''"
                @click="filePickerOpen = !filePickerOpen; pluginOpen = false; modelOpen = false"
              >
                <Paperclip class="h-4 w-4" />
              </button>

              <div v-if="filePickerOpen" class="menu-card absolute bottom-10 left-0 max-h-80 w-80 overflow-y-auto">
                <p class="mb-1 flex items-center gap-1.5 px-2 text-[11px] font-medium text-muted-foreground">
                  <Paperclip class="h-3 w-3" /> Attach a file
                </p>
                <div v-if="chat.filesLoading" class="flex items-center gap-1.5 px-2 py-2 text-[12px] text-subtle">
                  <Loader2 class="h-3.5 w-3.5 animate-spin" /> Loading…
                </div>
                <button
                  v-for="e in fileEntries"
                  :key="e.path"
                  class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                  type="button"
                  @mousedown.prevent="attachFile(e)"
                >
                  <FileCode class="h-3.5 w-3.5 shrink-0 text-subtle" />
                  <span class="min-w-0 flex-1 truncate text-[12.5px]" :style="{ paddingLeft: e.depth * 10 + 'px' }">
                    {{ e.name }}
                  </span>
                  <Check v-if="chat.pendingFiles.some((f) => f.path === e.path)" class="h-3.5 w-3.5 shrink-0" style="color: var(--primary)" />
                </button>
              </div>
            </div>

            <textarea
              :ref="onTextareaMount"
              v-model="draft"
              rows="1"
              :placeholder="hasModel ? 'Describe a task — @ to reference a file, / for a command' : 'Configure a model to start chatting'"
              :disabled="!hasModel"
              class="max-h-50 min-h-8 flex-1 resize-none bg-transparent px-1 py-1.5 text-[13px] leading-5 outline-none placeholder:text-subtle disabled:cursor-not-allowed"
              aria-label="Message"
              @keydown="onComposerKey"
            />

            <button
              v-if="chat.streaming"
              class="btn btn-danger-outline h-8! shrink-0 px-3!"
              type="button"
              title="Stop"
              aria-label="Stop"
              @click="chat.abort()"
            >
              <Square class="h-3.5 w-3.5" />
            </button>
            <button
              v-else
              class="btn btn-primary h-8! shrink-0 px-3!"
              type="button"
              :disabled="!canSend"
              title="Send"
              aria-label="Send"
              @click="send"
            >
              <Send class="h-4 w-4" />
            </button>
          </div>
        </div>

        <!-- Footer: hints, model, usage -->
        <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 px-1 text-[10.5px] text-subtle">
          <span v-if="hasModel" class="flex items-center gap-1">
            <Settings2 class="h-3 w-3" /> Enter to send · Shift+Enter for newline
          </span>
          <span v-if="chat.lastUsage" class="ml-auto tabular-nums">
            {{ chat.lastUsage.totalTokens.toLocaleString() }} tokens
            <span class="text-subtle/70">
              ({{ chat.lastUsage.inputTokens.toLocaleString() }} in ·
              {{ chat.lastUsage.outputTokens.toLocaleString() }} out)
            </span>
          </span>
          <span v-else-if="hasModel" class="ml-auto mono">{{ modelLabel(model) }}</span>
        </div>
      </div>
    </div>

    <!-- Outside-click catcher for popovers -->
    <div
      v-if="filePickerOpen || pluginOpen || sessionOpen || modelOpen"
      class="fixed inset-0 z-20"
      @mousedown="closeMenus"
    />
  </div>
</template>

<style scoped>
.menu-card {
  z-index: 40;
  border-radius: 10px;
  border: 1px solid var(--divider);
  background: var(--surface);
  box-shadow: var(--shadow-popover);
  padding: 4px;
}

/* Self-contained input chrome: no loud focus ring (beats the global outline). */
.composer textarea:focus-visible {
  outline: none;
}
</style>
