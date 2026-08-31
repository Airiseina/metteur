<script setup lang="ts">
import { onBeforeUnmount, onMounted, provide, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { FilePlus2, FolderPlus, RefreshCw } from '@lucide/vue'
import { gateway } from '@/core'
import type { FileTreeNode } from '@/core'
import { useWorkspaceStore } from '@/stores/workspace'
import { useTabsStore } from '@/stores/tabs'
import { useChatStore } from '@/stores/chat'
import { useBlueprintStore } from '@/stores/blueprint'
import { useFeedbackStore } from '@/stores/feedback'
import { useFileWatchStore } from '@/stores/filewatch'
import { fileRoute } from '@/lib/file-token'
import { useSurfaceNavigation } from '@/lib/surface'
import { TREE_API, TREE_CREATE } from '@/lib/tree'
import TreeItem from '@/components/TreeItem.vue'
import ContextMenu, { type MenuGroup, type MenuItem } from '@/components/ContextMenu.vue'

/**
 * Left-side resource explorer.
 *
 * Loads the top-level listing lazily and exposes an API (expand, open file,
 * active highlight) through TREE_API. Every row — at any depth — is the
 * recursive TreeItem component. The toolbar offers New File / New Folder /
 * Refresh; those plus in-place creation also live on the blank-area and
 * folder context menus.
 */

const workspace = useWorkspaceStore()
const tabs = useTabsStore()
const chat = useChatStore()
const blueprint = useBlueprintStore()
const router = useRouter()
const feedback = useFeedbackStore()
const fileWatch = useFileWatchStore()

const root = ref<FileTreeNode[]>([])
const expanded = ref<Set<string>>(new Set())
const loading = ref(false)
const menu = ref<{ x: number; y: number; node: FileTreeNode | null } | null>(null)
/** New-file/folder name input, anchored to the toolbar. */
const creating = ref<{ kind: 'file' | 'folder'; dir: string } | null>(null)
const newName = ref('')
const { openSurface } = useSurfaceNavigation()

/** Loads (or reuses the cached) listing for `dir`; fills `into.children` too. */
async function loadDir(dir: string, into: FileTreeNode | null): Promise<FileTreeNode[]> {
  const wsPath = workspace.active?.path ?? ''
  const cached = workspace.cachedDir(wsPath, dir)
  if (cached) return cached
  const r = await gateway.listFiles(wsPath, dir)
  if (!r.ok) return []
  if (into) into.children = r.data
  workspace.cacheDir(wsPath, dir, r.data)
  return r.data
}

async function loadRoot() {
  loading.value = true
  try {
    root.value = await loadDir('', null)
  } finally {
    loading.value = false
  }
}

async function toggle(node: FileTreeNode) {
  if (node.kind !== 'dir') return
  const willOpen = !expanded.value.has(node.path)
  if (willOpen && node.children === undefined) await loadDir(node.path, node)
  if (expanded.value.has(node.path)) expanded.value.delete(node.path)
  else expanded.value.add(node.path)
  expanded.value = new Set(expanded.value)
}

function openFile(node: FileTreeNode) {
  if (node.kind !== 'file') return
  tabs.openFile(node.path)
  router.push(fileRoute(node.path))
}

/** Row highlight classset for the file currently being edited. */
function activeCls(path: string): string {
  return tabs.activeFilePath === path ? 'bg-accent text-foreground' : ''
}

/** Refresh listing for the root and every expanded folder. */
async function refreshTree() {
  const ws = workspace.active
  if (!ws) return
  workspace.invalidateTree(ws.path)
  await loadRoot()
  await reloadExpanded(root.value)
}

async function reloadExpanded(nodes: FileTreeNode[]) {
  for (const n of nodes) {
    if (n.kind !== 'dir' || !expanded.value.has(n.path)) continue
    await loadDir(n.path, n)
    await reloadExpanded(n.children ?? [])
  }
}

function findNode(path: string, nodes: FileTreeNode[]): FileTreeNode | null {
  for (const n of nodes) {
    if (n.path === path) return n
    if (n.children) {
      const hit = findNode(path, n.children)
      if (hit) return hit
    }
  }
  return null
}

function startCreate(kind: 'file' | 'folder', dir: string) {
  creating.value = { kind, dir }
  newName.value = 'untitled'
  // Reveal the target folder so the inline row is visible at its level.
  if (dir) {
    const node = findNode(dir, root.value)
    if (node && !expanded.value.has(dir)) void toggle(node)
  }
}

function cancelCreate() {
  creating.value = null
}

/** Create at `dir`; conflicts and empty names are rejected with a toast. */
async function confirmCreate() {
  const ws = workspace.active
  if (!ws || !creating.value) return
  const { kind, dir } = creating.value
  const name = newName.value.trim()
  creating.value = null
  if (!name || /[\\/]/.test(name)) {
    feedback.toast('error', 'Invalid name', 'Name must be non-empty and contain no path separators')
    return
  }
  const path = dir ? `${dir}/${name}` : name
  if (await exists(ws.path, dir, path)) {
    feedback.toast('error', `${name} already exists`)
    return
  }
  const r = kind === 'folder' ? await gateway.createDir(ws.path, path) : await gateway.writeFile(ws.path, path, '')
  if (!r.ok) {
    feedback.toast('error', `Cannot create ${kind}`, r.error)
    return
  }
  if (kind === 'file') {
    tabs.openFile(path)
    router.push(fileRoute(path))
  }
  await refreshTree()
}

/** Whether a path already exists in the (possibly cached) listing of `dir`. */
async function exists(wsPath: string, dir: string, path: string): Promise<boolean> {
  const cached = workspace.cachedDir(wsPath, dir)
  if (cached) return cached.some((n) => n.path === path)
  const r = await gateway.listFiles(wsPath, dir)
  return r.ok ? r.data.some((n) => n.path === path) : false
}

function openMenu(x: number, y: number, node: FileTreeNode | null) {
  menu.value = { x, y, node }
}

/** Right-click on the tree's blank area (not on a row). */
function onBlankContext(e: MouseEvent) {
  if ((e.target as HTMLElement).closest('.tree-row')) return
  openMenu(e.clientX, e.clientY, null)
}

/** Row right-click forwarded by TreeItem. */
function onRowContextMenu(node: FileTreeNode, event: MouseEvent) {
  openMenu(event.clientX, event.clientY, node)
}

function menuGroupsOf(node: FileTreeNode | null): MenuGroup[] {
  const items: MenuItem[] = []
  if (node?.kind === 'file') {
    items.push({ id: 'open', label: 'Open' })
    items.push({ id: 'add-to-chat', label: 'Add to Conversation' })
    items.push({ id: 'add-to-blueprint', label: 'Add to Blueprint' })
  } else {
    items.push({ id: 'new-file', label: 'New File…' })
    items.push({ id: 'new-folder', label: 'New Folder…' })
    items.push({ id: 'refresh', label: 'Refresh' })
  }
  return [
    { label: node?.kind === 'file' ? 'File' : 'Create', items },
    { label: 'Actions', items: [{ id: 'copy-path', label: 'Copy Path' }] },
  ]
}

async function onMenuSelect(id: string) {
  const node = menu.value?.node ?? null
  menu.value = null
  switch (id) {
    case 'copy-path':
      if (node) await navigator.clipboard.writeText(node.path)
      return
    case 'new-file':
      startCreate('file', node?.kind === 'dir' ? node.path : '')
      return
    case 'new-folder':
      startCreate('folder', node?.kind === 'dir' ? node.path : '')
      return
    case 'refresh':
      void refreshTree()
      return
  }
  if (!node) return
  switch (id) {
    case 'open':
      openFile(node)
      break
    case 'add-to-chat':
      chat.attach(node.path)
      openSurface('chat')
      break
    case 'add-to-blueprint':
      blueprint.requestAddReference(node.path)
      break
  }
}

provide(TREE_API, {
  isOpen: (path) => expanded.value.has(path),
  toggle,
  openFile,
  activeCls,
  onRowContextMenu,
})
/** In-tree creation state, consumed by TreeItem to host the inline input. */
const createState = reactive({
  active: creating,
  name: newName,
  confirm: () => void confirmCreate(),
  cancel: cancelCreate,
})
provide(TREE_CREATE, createState)

onMounted(() => {
  void loadRoot()
  window.addEventListener('focus', onWindowFocus)
  // Refresh on live fs events (debounced bursts from external editors/CLI).
  watch(
    () => fileWatch.events,
    () => {
      if (!fileWatch.active || !workspace.hasActive) return
      clearTimeout(refreshTimer)
      refreshTimer = setTimeout(() => void refreshTree(), 150)
    },
  )
})
onBeforeUnmount(() => {
  window.removeEventListener('focus', onWindowFocus)
  clearTimeout(refreshTimer)
})

/** Re-sync the listing with the disk when the window regains focus. */
function onWindowFocus() {
  if (workspace.hasActive) void refreshTree()
}

function onCreateFocus(e: FocusEvent) {
  const el = e.target as HTMLInputElement
  el.select()
}

let refreshTimer: ReturnType<typeof setTimeout> | undefined
</script>

<template>
  <div class="flex h-full flex-col bg-sidebar">
    <!-- Toolbar (VSCode-style explorer header) -->
    <div class="flex shrink-0 items-center gap-0.5 border-b border-divider px-2 py-1">
      <span class="panel-heading flex-1 px-0!">Explorer</span>
      <button class="btn-icon h-6! w-6!" type="button" title="New File" :aria-label="'New File'" @click="startCreate('file', '')">
        <FilePlus2 class="h-3.5 w-3.5" />
      </button>
      <button class="btn-icon h-6! w-6!" type="button" title="New Folder" :aria-label="'New Folder'" @click="startCreate('folder', '')">
        <FolderPlus class="h-3.5 w-3.5" />
      </button>
      <button class="btn-icon h-6! w-6!" type="button" title="Refresh" :aria-label="'Refresh'" @click="refreshTree">
        <RefreshCw class="h-3.5 w-3.5" />
      </button>
    </div>

    <!-- Inline name input for creating a file/folder (rendered inside the
         tree at the target level; tree folder rows host their own). -->
    <div v-if="createState.active && createState.active.dir === ''" class="px-2 py-0.5">
      <input
        v-model="createState.name"
        class="input h-7! w-full text-[12px]"
        autofocus
        @focus="onCreateFocus"
        @keydown.enter="createState.confirm()"
        @keydown.esc="createState.cancel()"
        @blur="createState.confirm()"
      />
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto p-1" @contextmenu.prevent="onBlankContext">
      <p v-if="loading" class="px-2 py-1 text-[11px] text-subtle">Loading…</p>
      <TreeItem v-for="node in root" v-else :key="node.path" :node="node" :depth="0" />
    </div>

    <ContextMenu
      v-if="menu"
      :x="menu.x"
      :y="menu.y"
      :groups="menuGroupsOf(menu.node)"
      @select="onMenuSelect"
      @close="menu = null"
    />
  </div>
</template>
