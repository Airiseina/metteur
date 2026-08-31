<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Cpu,
  FileCode,
  Loader2,
  MessageSquarePlus,
  Paperclip,
  Plug,
  Send,
  SlidersHorizontal,
  Square,
  User,
  X,
} from '@lucide/vue'
import { useChatStore } from '@/stores/chat'
import { useConfigStore } from '@/stores/config'
import { useTabsStore } from '@/stores/tabs'
import { fileRoute } from '@/lib/file-token'
import type { ChatOptions, FileTreeNode } from '@/core'

const chat = useChatStore()
const tabs = useTabsStore()
const config = useConfigStore()
const router = useRouter()
const draft = ref('')
const scrollEl = ref<HTMLElement | null>(null)

/** Model ids from the daemon model table (`llm.models`), falling back to a
 *  sane pair until the config has loaded. */
const MODELS = computed(() => {
  const keys = Object.keys((config.effective.llm?.models as Record<string, unknown> | undefined) ?? {})
  const def = config.effective.llm?.default_model
  const list = def ? [def, ...keys.filter((k) => k !== def)] : keys
  return list.length ? list : ['claude-4', 'deepseek-v4']
})
const model = ref('')
watch(
  MODELS,
  (list) => {
    if (!model.value || !list.includes(model.value)) model.value = list[0] ?? ''
  },
  { immediate: true },
)
const reasoning = ref<ChatOptions['reasoning_effort']>('medium')
const temperature = ref(0.7)
const topP = ref(0.9)
const maxTokens = ref(4096)
const paramsOpen = ref(false)

/* Popovers ----------------------------------------------------------- */

const filePickerOpen = ref(false)
const pluginOpen = ref(false)

/** Collapsed/expanded tool-call cards, keyed by message id. */
const expandedTools = ref<Set<string>>(new Set())

/* Flattened file list (attach + @-mention) --------------------------- */

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
/** True while the cursor is inside an `@` token (show the file menu). */
const mentionOpen = computed(() => mentionQuery.value !== '')
/** True while the cursor is inside a `/` token (show the command menu). */
const commandOpen = computed(() => commandQuery.value !== '')

const filteredFiles = computed(() => {
  const q = mentionQuery.value
  if (!q) return fileEntries.value
  return fileEntries.value.filter((e) => e.name.toLowerCase().includes(q))
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

function enabledAddons() {
  return chat.addons.filter((a) => a.enabled)
}

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

async function send() {
  const text = draft.value.trim()
  if (!text || chat.streaming) return
  filePickerOpen.value = false
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

let textarea: HTMLTextAreaElement | null = null
function onTextareaMount(el: unknown) {
  textarea = el instanceof HTMLTextAreaElement ? el : null
}

async function onComposerKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    await send()
    return
  }
  if (e.key === 'Escape') {
    filePickerOpen.value = false
    pluginOpen.value = false
  }
}

async function copyMessage(content: string) {
  await navigator.clipboard.writeText(content)
}

onMounted(() => {
  void chat.loadFiles()
  void chat.loadAddons()
})
</script>

<template>
  <div class="flex h-full flex-col bg-background">
    <!-- ── Agent action bar ─────────────────────────────────────────────── -->
    <div class="flex h-12 shrink-0 items-center gap-2 border-b border-divider px-3">
      <span class="flex h-6 w-6 items-center justify-center rounded-lg text-primary-foreground" style="background: var(--primary)">
        <Bot class="h-3.5 w-3.5" />
      </span>
      <span class="text-[13px] font-semibold">Agent</span>
      <span class="chip">ReAct</span>

      <div class="ml-auto flex items-center gap-1.5">
        <!-- Model selector -->
        <label class="relative inline-flex items-center">
          <select v-model="model" class="input h-7 w-auto! cursor-pointer pr-6 text-[12px]!" :aria-label="'Model'">
            <option v-for="m in MODELS" :key="m" :value="m">{{ m }}</option>
          </select>
          <ChevronDown class="pointer-events-none absolute right-1.5 h-3 w-3 text-subtle" />
        </label>

        <!-- Plugin toggles -->
        <div class="relative">
          <button
            class="btn-icon relative h-7! w-7!"
            type="button"
            title="Plugins"
            :aria-label="'Plugins'"
            :class="pluginOpen ? 'text-foreground!' : ''"
            @click="pluginOpen = !pluginOpen"
          >
            <Plug class="h-3.5 w-3.5" />
            <span
              v-if="enabledAddons().length"
              class="absolute -right-0.5 -top-0.5 flex h-3.5 min-w-3.5 items-center justify-center rounded-full px-0.5 text-[9px] font-semibold text-primary-foreground"
              style="background: var(--primary)"
            >
              {{ enabledAddons().length }}
            </span>
          </button>

          <div v-if="pluginOpen" class="menu-card absolute right-0 top-9 w-72">
            <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Plugins</p>
            <div v-if="chat.addonsLoading" class="flex items-center gap-1.5 px-2 py-2 text-[12px] text-subtle">
              <Loader2 class="h-3.5 w-3.5 animate-spin" /> Loading…
            </div>
            <button v-if="!chat.addons.length && !chat.addonsLoading" class="px-2 py-2 text-[12px] text-subtle" type="button">
              No plugins installed.
            </button>
            <div
              v-for="a in chat.addons"
              :key="a.name"
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
                class="flex h-4 w-7 items-center rounded-full p-0.5 transition-colors duration-150"
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

        <!-- Model parameters -->
        <button
          class="btn-icon h-7! w-7!"
          type="button"
          title="Model parameters"
          :aria-label="'Model parameters'"
          :class="paramsOpen ? 'text-foreground!' : ''"
          @click="paramsOpen = !paramsOpen"
        >
          <SlidersHorizontal class="h-3.5 w-3.5" />
        </button>

        <button
          class="btn btn-ghost h-7! px-2! text-[12px]"
          type="button"
          :disabled="chat.busy"
          title="New chat (clears history)"
          @click="chat.clear()"
        >
          <MessageSquarePlus class="h-4 w-4" /> New
        </button>
      </div>
    </div>

    <!-- Model parameters (collapsible) -->
    <div v-if="paramsOpen" class="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-divider bg-surface-muted/50 px-4 py-2">
      <label class="flex items-center gap-2 text-[11px] text-muted-foreground">
        Reasoning
        <select v-model="reasoning" class="input h-6 w-auto! px-1.5! text-[11px]!">
          <option value="none">none</option>
          <option value="low">low</option>
          <option value="medium">medium</option>
          <option value="high">high</option>
        </select>
      </label>
      <label class="flex items-center gap-2 text-[11px] text-muted-foreground">
        Temperature
        <input v-model.number="temperature" type="number" step="0.1" min="0" max="2" class="input h-6 w-16 px-1.5! text-[11px]!" />
      </label>
      <label class="flex items-center gap-2 text-[11px] text-muted-foreground">
        Top P
        <input v-model.number="topP" type="number" step="0.05" min="0" max="1" class="input h-6 w-16 px-1.5! text-[11px]!" />
      </label>
      <label class="flex items-center gap-2 text-[11px] text-muted-foreground">
        Max tokens
        <input v-model.number="maxTokens" type="number" step="256" min="256" class="input h-6 w-20 px-1.5! text-[11px]!" />
      </label>
    </div>

    <!-- ── Message thread (ReAct) ───────────────────────────────────────── -->
    <div ref="scrollEl" class="min-h-0 flex-1 overflow-y-auto">
      <div class="mx-auto flex max-w-2xl flex-col gap-4 p-5 pb-8">
        <div v-if="!chat.messages.length" class="py-10 text-center">
          <span
            class="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-2xl text-primary-foreground"
            style="background: var(--primary)"
          >
            <Bot class="h-6 w-6" />
          </span>
          <p class="text-[14px] font-medium">What can I build for you?</p>
          <p class="mt-1 text-[12px] text-muted-foreground">
            Describe a task, attach files, or pick a plugin — I'll run tools in the
            sandbox and can generate blueprints.
          </p>
        </div>

        <template v-for="m in chat.messages" :key="m.id">
          <!-- User turn -->
          <div v-if="m.role === 'user'" class="group flex flex-row-reverse gap-3">
            <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground">
              <User class="h-4 w-4" />
            </span>
            <div class="min-w-0 max-w-[85%]">
              <div class="rounded-xl bg-primary px-3.5 py-2.5 text-[13px] leading-relaxed text-primary-foreground">
                <template v-for="(part, i) in renderUserContent(m.content)" :key="i">
                  <span v-if="part.kind === 'file'">
                    <button
                      class="mb-0.5 mr-1 inline-flex items-center gap-1 rounded-md bg-white/20 px-1.5 py-0.5 font-mono text-[11.5px] underline decoration-dotted underline-offset-2 hover:bg-white/30"
                      type="button"
                      @click="openFile(part.path)"
                    >
                      <FileCode class="h-3 w-3" /> {{ part.path }}
                    </button>
                  </span>
                  <template v-else>{{ part.text }}</template>
                </template>
                <span v-if="m.pending" class="ml-0.5 inline-block h-3 w-1.75 animate-pulse align-middle bg-white/80" />
              </div>
              <div class="mt-1 flex justify-end">
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

          <!-- Tool call card (collapsible) -->
          <div v-else-if="m.role === 'tool'" class="group flex gap-3">
            <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg" style="background: var(--primary-soft)">
              <Cpu class="h-4 w-4" style="color: var(--primary)" />
            </span>
            <div class="min-w-0 max-w-[85%] flex-1">
              <div class="overflow-hidden rounded-xl border border-divider" style="background: var(--surface-muted)">
                <button
                  class="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors duration-150 hover:bg-hover"
                  type="button"
                  @click="toggleTool(m.id)"
                >
                  <ChevronRight class="h-3.5 w-3.5 text-subtle transition-transform duration-150" :class="isToolOpen(m.id) ? 'rotate-90' : ''" />
                  <span class="font-mono text-[12px] font-semibold" style="color: var(--primary)">{{ m.actor }}</span>
                  <Cpu v-if="m.pending" class="h-3 w-3 animate-pulse text-subtle" style="color: var(--primary)" />
                  <span class="ml-auto flex items-center gap-1 text-[11px] text-subtle">
                    <span v-if="!m.pending" class="flex h-1.5 w-1.5 rounded-full" style="background: var(--primary)" />
                    {{ m.pending ? 'running…' : 'done' }}
                  </span>
                </button>
                <div v-if="isToolOpen(m.id)" class="border-t border-divider px-3 py-2.5">
                  <div class="mb-1.5 flex items-center justify-between">
                    <span class="text-[11px] font-medium uppercase tracking-wide text-subtle">Output</span>
                    <button
                      v-if="m.detail && (m.detail.file as string)"
                      class="text-[11px] text-primary underline decoration-dotted underline-offset-2 hover:no-underline"
                      type="button"
                      @click="openFile(m.detail!.file as string)"
                    >
                      open file
                    </button>
                  </div>
                  <pre class="whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-foreground/90">{{ m.content }}</pre>
                </div>
              </div>
              <div v-if="m.detail && m.detail.model" class="mt-1 pl-1 text-[10px] text-subtle">{{ m.detail.model }}</div>
              <div class="mt-1 flex">
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

          <!-- Assistant bubble -->
          <div v-else class="group flex gap-3">
            <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-surface-muted text-muted-foreground">
              <Bot class="h-4 w-4" />
            </span>
            <div class="min-w-0 max-w-[85%]">
              <div class="panel rounded-xl px-3.5 py-2.5 text-[13px] leading-relaxed">
                {{ m.content }}<span v-if="m.pending" class="ml-0.5 inline-block h-3 w-1.75 animate-pulse align-middle" style="background: var(--primary)" />
              </div>
              <div v-if="m.detail && m.detail.model" class="mt-1 pl-1 text-[10px] text-subtle">{{ m.detail.model }}</div>
              <div class="mt-1 flex">
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
        </template>

        <!-- Agent activity indicator -->
        <div v-if="chat.streaming" class="flex items-center gap-2 pl-10 text-[12px] text-subtle">
          <Loader2 class="h-3.5 w-3.5 animate-spin" style="color: var(--primary)" />
          Agent is working…
        </div>
      </div>
    </div>

    <!-- ── Agent composer ───────────────────────────────────────────────── -->
    <div class="shrink-0 border-t border-divider p-3">
      <div class="mx-auto max-w-2xl">
        <div class="composer relative panel flex flex-col p-2">
          <!-- @-mention / slash-command menu -->
          <div
            v-if="mentionOpen || commandOpen"
            class="menu-card absolute bottom-full left-2 mb-2 max-h-64 w-72 overflow-y-auto"
          >
            <template v-if="mentionOpen">
              <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Reference a file</p>
              <button v-if="!filteredFiles.length" class="px-2 py-1.5 text-[12px] text-subtle" type="button">No matches</button>
              <button
                v-for="e in filteredFiles"
                :key="e.path"
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                type="button"
                @mousedown.prevent="pickFile(e)"
              >
                <FileCode class="h-3.5 w-3.5 shrink-0 text-subtle" style="color: var(--primary)" />
                <span class="min-w-0 flex-1 truncate text-[12.5px]" :style="{ paddingLeft: e.depth * 10 + 'px' }">
                  {{ e.name }}
                </span>
                <Check v-if="chat.pendingFiles.some((f) => f.path === e.path)" class="h-3.5 w-3.5 text-subtle" />
              </button>
            </template>
            <template v-else>
              <p class="mb-1 px-2 text-[11px] font-medium text-muted-foreground">Commands</p>
              <button v-if="!filteredCommands.length" class="px-2 py-1.5 text-[12px] text-subtle" type="button">No commands</button>
              <button
                v-for="c in filteredCommands"
                :key="c.label"
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-hover"
                type="button"
                @mousedown.prevent="pickCommand(c)"
              >
                <span class="w-16 shrink-0 font-mono text-[12px]" style="color: var(--primary)">{{ c.label }}</span>
                <span class="truncate text-[12px] text-muted-foreground">{{ c.desc }}</span>
              </button>
            </template>
          </div>

          <!-- Pending file chips -->
          <div v-if="chat.pendingFiles.length" class="mb-1.5 flex flex-wrap gap-1.5">
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

          <!-- Composer row -->
          <div class="flex items-end gap-1.5">
            <!-- Attach file -->
            <div class="relative">
              <button
                class="btn-icon h-8! w-8! shrink-0"
                type="button"
                title="Attach a file"
                :aria-label="'Attach a file'"
                :class="filePickerOpen ? 'text-foreground!' : ''"
                @click="filePickerOpen = !filePickerOpen; pluginOpen = false"
              >
                <Paperclip class="h-4 w-4" />
              </button>

              <div v-if="filePickerOpen" class="menu-card absolute bottom-10 left-0 max-h-80 w-72 overflow-y-auto">
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
                  <FileCode class="h-3.5 w-3.5 shrink-0 text-subtle" style="color: var(--primary)" />
                  <span class="min-w-0 flex-1 truncate text-[12.5px]" :style="{ paddingLeft: e.depth * 10 + 'px' }">
                    {{ e.name }}
                  </span>
                  <Check v-if="chat.pendingFiles.some((f) => f.path === e.path)" class="h-3.5 w-3.5 text-subtle" />
                </button>
              </div>
            </div>

            <textarea
              :ref="onTextareaMount"
              v-model="draft"
              rows="1"
              class="min-h-9 max-h-40 flex-1 resize-none bg-transparent px-1 py-1.5 text-[13px] outline-none placeholder:text-subtle"
              placeholder="Describe a task, type @ for a file or / for a command…"
              :aria-label="'Message'"
              @keydown="onComposerKey"
            />

            <button
              v-if="chat.streaming"
              class="btn btn-danger-outline shrink-0 px-3!"
              type="button"
              title="Stop"
              aria-label="Stop"
              @click="chat.abort()"
            >
              <Square class="h-3.5 w-3.5" />
            </button>
            <button
              v-else
              class="btn btn-primary shrink-0 px-3!"
              type="button"
              :disabled="!draft.trim() || chat.streaming"
              title="Send"
              aria-label="Send"
              @click="send"
            >
              <Send class="h-4 w-4" />
            </button>
          </div>
        </div>

        <p class="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 pl-1 text-[10.5px] text-subtle">
          <span class="flex items-center gap-1"><Send class="h-3 w-3" /> Enter to send | Shift+Enter newline</span>
          <span class="flex items-center gap-1"><User class="h-3 w-3" /> @ mention a file</span>
          <span class="flex items-center gap-1"><SlidersHorizontal class="h-3 w-3" /> / commands</span>
          <span class="ml-auto font-mono">{{ model }}</span>
        </p>
      </div>
    </div>

    <!-- Overlay to dismiss popovers on outside click (kept under the .menu-card z-index). -->
    <div
      v-if="filePickerOpen || pluginOpen"
      class="fixed inset-0 z-20"
      @mousedown="filePickerOpen = false; pluginOpen = false"
    />
  </div>
</template>

<style scoped>
.menu-card {
  z-index: 40;
  border-radius: 10px;
  border: 1px solid var(--divider);
  background: var(--surface);
  box-shadow: var(--shadow-card);
  padding: 4px;
}

/* The composer is self-contained input chrome — no loud focus ring (beats the
   global :focus-visible outline). */
.composer textarea:focus-visible {
  outline: none;
}
</style>
