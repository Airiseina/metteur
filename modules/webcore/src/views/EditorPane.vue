<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { GitBranch, Redo2, Save, TriangleAlert, Undo2, Workflow } from '@lucide/vue'
import { gateway } from '@/core'
import { useWorkspaceStore } from '@/stores/workspace'
import { useTabsStore } from '@/stores/tabs'
import { useRightPanelStore } from '@/stores/right-panel'
import { useFileWatchStore } from '@/stores/filewatch'
import { useFeedbackStore } from '@/stores/feedback'
import CodeEditor from '@/components/CodeEditor.vue'
import BlueprintView from './BlueprintView.vue'
import FileVersionPanel from '@/components/FileVersionPanel.vue'
import { dirOf } from '@/lib/path'
import { fileRoute, pathByFileParam } from '@/lib/file-token'
import { wurl } from '@/lib/workspace-url'

const route = useRoute()
const router = useRouter()
const workspace = useWorkspaceStore()
const tabs = useTabsStore()
const rightPanel = useRightPanelStore()
const fileWatch = useFileWatchStore()
const feedback = useFeedbackStore()

/** The file path comes from the `?f=` hash, resolved via the persisted
 *  hash→path map — never from raw URL bytes (no path traversal surface). */
const filePath = computed(() => pathByFileParam(String(route.query.f ?? '')))
const isBlueprint = computed(() => filePath.value.endsWith('.blueprint'))
/** A `.mbp` file is a text-drawn blueprint (the DSL); a toolbar action can
 *  compile it straight into a visual blueprint file. */
const isDsl = computed(() => filePath.value.toLowerCase().endsWith('.mbp'))

const content = ref('')
const loaded = ref(false)
/** Non-empty when the current file could not be opened (binary/too large). */
const error = ref('')
/** Whether the current file is shown as dirty (drives save + tab dot). */
const dirty = computed(() => tabs.isDirty(filePath.value))
/** Last content observed on disk, kept for the conflict banner's reload. */
const onDiskContent = ref('')
/** Whether the active file changed on disk while local edits are pending. */
const conflicted = computed(() => tabs.isConflict(filePath.value))

const editorRef = ref<InstanceType<typeof CodeEditor>>()
const fileName = computed(() => {
  const seg = filePath.value.split(/[\\/]/).filter(Boolean)
  return seg[seg.length - 1] ?? filePath.value
})

/** Map a file path to an editor language token consumed by `CodeEditor`. */
function languageOf(path: string): string {
  const ext = path.split('.').pop() ?? ''
  const map: Record<string, string> = {
    mbp: 'mbp',
    json: 'json',
    toml: 'toml',
    ts: 'ts',
    tsx: 'ts',
    js: 'js',
    jsx: 'js',
    mjs: 'js',
    cjs: 'js',
    html: 'html',
    htm: 'html',
    css: 'css',
    scss: 'scss',
    less: 'less',
  }
  return map[ext] ?? 'plaintext'
}

/** Pretty-print JSON when opening an editor for a `.json` file. */
function normalize(language: string, text: string): string {
  if (language !== 'json') return text
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return text
  }
}

async function load() {
  const ws = workspace.active
  if (!ws || isBlueprint.value) {
    loaded.value = true
    error.value = ''
    return
  }
  loaded.value = false
  error.value = ''
  const r = await gateway.readFile(ws.path, filePath.value)
  if (r.ok) {
    content.value = normalize(languageOf(filePath.value), r.data.content)
  } else {
    // Never show a stale buffer for a file that failed to open (binary or
    // too large to decode as UTF-8 and render).
    content.value = ''
    error.value = r.error
  }
  loaded.value = true
}

/** Save the active file and refresh the explorer listing of its folder. */
async function handleSave() {
  const ws = workspace.active
  if (!ws || isBlueprint.value) return
  const r = await gateway.writeFile(ws.path, filePath.value, content.value)
  if (r.ok) {
    tabs.markDirty(filePath.value, false)
    tabs.markConflict(filePath.value, false)
    onDiskContent.value = content.value
    workspace.invalidateDir(ws.path, dirOf(filePath.value))
  }
}

/** Discard local edits and adopt the on-disk content. */
function reloadFromDisk() {
  content.value = onDiskContent.value
  tabs.markDirty(filePath.value, false)
  tabs.markConflict(filePath.value, false)
}

/** Detect disk changes for the active file (auto-adopt when clean, flag when
 *  the buffer is dirty so the user can resolve and save manually). */
async function syncWithDisk() {
  const ws = workspace.active
  if (!ws || isBlueprint.value || !filePath.value || !loaded.value) return
  const r = await gateway.readFile(ws.path, filePath.value)
  if (!r.ok) return
  const disk = normalize(languageOf(filePath.value), r.data.content)
  if (disk === content.value) return
  onDiskContent.value = disk
  if (dirty.value) tabs.markConflict(filePath.value, true)
  else {
    tabs.markConflict(filePath.value, false)
    content.value = disk
  }
}

function onWindowFocus() {
  void syncWithDisk()
}
function onVisibility() {
  if (document.visibilityState === 'visible') void syncWithDisk()
}

/** Take over Ctrl/Cmd+S so the browser's "save page" dialog never appears. */
function onGlobalKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
    e.preventDefault()
    if (dirty.value && !isBlueprint.value) void handleSave()
  }
}

onMounted(() => {
  // Unknown/stale file hash: leave the paneless editor for the Explorer.
  if (!filePath.value) {
    router.replace(wurl('/explorer'))
    return
  }
  // Deep-link refresh: re-register the tab so the strip matches the editor.
  tabs.openFile(filePath.value)
  void load()
  window.addEventListener('keydown', onGlobalKeydown)
  window.addEventListener('focus', onWindowFocus)
  document.addEventListener('visibilitychange', onVisibility)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onGlobalKeydown)
  window.removeEventListener('focus', onWindowFocus)
  document.removeEventListener('visibilitychange', onVisibility)
})
watch(filePath, () => void load())

// Disk sync is event-driven: react only when the daemon reports this exact
// file changed on disk (plus window focus as a fallback).
watch(
  () => fileWatch.events,
  (list) => {
    if (!fileWatch.active || isBlueprint.value) return
    if (list.some((e) => e.path === filePath.value)) void syncWithDisk()
  },
)

/** Editor change → update content and mark the tab dirty (per path). */
function onEdit(value: string) {
  content.value = value
  tabs.markDirty(filePath.value, true)
}

function handleUndo() {
  editorRef.value?.undo()
}
function handleRedo() {
  editorRef.value?.redo()
}

/** Editor undo/redo availability, forwarded from CodeEditor (item 9). */
const canUndo = ref(false)
const canRedo = ref(false)
function onHistory(u: boolean, r: boolean) {
  canUndo.value = u
  canRedo.value = r
}

/** Open this file's version history in the closable right-hand panel (item 11). */
function openVersionPanel() {
  if (filePath.value) rightPanel.show(FileVersionPanel, 'Version History', { filePath: filePath.value })
}

/** Compile the open `.mbp` DSL text into a visual blueprint file next to it
 *  and open that file on the canvas. The DSL file itself is left untouched. */
async function compileToBlueprint() {
  const ws = workspace.active
  if (!ws || !filePath.value) return
  const r = await gateway.compileDsl(content.value)
  if (!r.ok) {
    feedback.toast('error', 'DSL compile failed', r.error)
    return
  }
  const name = fileName.value.replace(/\.mbp$/i, '') || 'blueprint'
  const dir = dirOf(filePath.value)
  const target = dir ? `${dir}/${name}.blueprint` : `${name}.blueprint`
  const ok = await gateway.writeFile(
    ws.path,
    target,
    JSON.stringify({ ...r.data, name }, null, 2),
  )
  if (!ok) {
    feedback.toast('error', 'Write blueprint failed', target)
    return
  }
  workspace.invalidateDir(ws.path, dirOf(target))
  tabs.openFile(target)
  feedback.toast('success', 'Blueprint created', target)
  router.push(fileRoute(target))
}
</script>

<template>
  <div class="flex h-full flex-col bg-background">
    <!-- Blueprint files are edited on the visual canvas. -->
    <template v-if="isBlueprint && loaded">
      <BlueprintView :file-path="filePath" />
    </template>

    <!-- Unsupported (binary) or oversized files show a plain hint instead of
         a stale or broken editor buffer. -->
    <template v-else-if="loaded && error">
      <div class="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
        <p class="text-[13px] font-medium text-foreground">Cannot open {{ fileName }}</p>
        <p class="max-w-sm text-[12px] text-muted-foreground">{{ error }}</p>
      </div>
    </template>

    <!-- Everything else: a code editor with an icon-only toolbar. -->
    <template v-else-if="loaded">
      <div class="flex h-10 shrink-0 items-center gap-3 border-b border-divider px-2">
        <span class="ml-1 mr-auto min-w-0 truncate text-[13px] font-medium">{{ fileName }}</span>
        <span
          v-if="dirty"
          class="mr-1 h-1.5 w-1.5 shrink-0 rounded-full"
          style="background: var(--primary)"
          title="Unsaved changes"
        />
        <button
          v-if="isDsl"
          class="btn btn-primary h-7! shrink-0"
          type="button"
          title="Compile DSL into a visual blueprint"
          :aria-label="'Compile to blueprint'"
          @click="compileToBlueprint"
        >
          <Workflow class="h-3.5 w-3.5" /> To Blueprint
        </button>
        <button
          class="editor-tool-icon"
          type="button"
          title="Save"
          :aria-label="'Save'"
          :disabled="!dirty"
          @click="handleSave"
        >
          <Save class="h-4 w-4" />
        </button>
        <button class="editor-tool-icon" type="button" title="Undo" aria-label="Undo" :disabled="!canUndo" @click="handleUndo">
          <Undo2 class="h-4 w-4" />
        </button>
        <button class="editor-tool-icon" type="button" title="Redo" aria-label="Redo" :disabled="!canRedo" @click="handleRedo">
          <Redo2 class="h-4 w-4" />
        </button>
        <button
          class="editor-tool-icon"
          type="button"
          title="Version history"
          aria-label="Version"
          @click="openVersionPanel"
        >
          <GitBranch class="h-4 w-4" />
        </button>
      </div>

      <!-- Conflict banner: local edits + disk changed → resolve manually. -->
      <div
        v-if="conflicted"
        class="flex shrink-0 items-center gap-2 border-b border-divider px-3 py-1.5 text-[11.5px]"
        style="background: var(--danger-soft); color: var(--danger)"
      >
        <TriangleAlert class="h-3.5 w-3.5 shrink-0" />
        <span class="min-w-0 truncate">File changed on disk while you have unsaved changes</span>
        <button class="shrink-0 underline underline-offset-2 hover:opacity-80" type="button" @click="reloadFromDisk">Reload</button>
        <button class="shrink-0 underline underline-offset-2 hover:opacity-80" type="button" @click="handleSave">Overwrite</button>
      </div>

      <div class="min-h-0 flex-1 overflow-hidden">
        <CodeEditor
          :key="filePath"
          ref="editorRef"
          v-model="content"
          :language="languageOf(filePath)"
          @update:model-value="onEdit"
          @history="onHistory"
        />
      </div>
    </template>
  </div>
</template>