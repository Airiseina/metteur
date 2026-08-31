import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { gateway } from '@/core'
import type { AddonInfo, ChatMessage, ChatOptions, FileTreeNode } from '@/core'
import { useWorkspaceStore } from './workspace'

/**
 * ReAct conversation for the current workspace.
 *
 * Holds the message list and the sending state so the chat surface stays a thin
 * view. Messages are upserted by id so streamed deltas fill a bubble in place
 * instead of appending duplicates. Agent helpers (workspace files for the
 * attach/@-menu and the installed addons) live here so the composer stays
 * stateless and the context survives tab switches.
 */
export const useChatStore = defineStore('chat', () => {
  const workspace = useWorkspaceStore()
  const messages = ref<ChatMessage[]>([])
  const streaming = ref(false)
  const busy = ref(false)

  // Files / addons driving the composer's attach, @-mention and plugin toggles.
  const files = ref<FileTreeNode[]>([])
  const filesLoading = ref(false)
  const addons = ref<AddonInfo[]>([])
  const addonsLoading = ref(false)
  /** Files queued in the composer (chips) but not yet sent as a turn. */
  const pendingFiles = ref<FileTreeNode[]>([])

  // Holding on to a message from a previous workspace pollutes the next one.
  watch(
    () => workspace.active?.path,
    () => {
      messages.value = []
      streaming.value = false
      busy.value = false
      pendingFiles.value = []
    },
  )

  /** Load the workspace file list into the attach / @-mention menu. */
  async function loadFiles(): Promise<void> {
    const ws = workspace.active
    if (!ws) return
    filesLoading.value = true
    try {
      const r = await gateway.listFiles(ws.path, '')
      if (r.ok) files.value = r.data
    } finally {
      filesLoading.value = false
    }
  }

  /** Load the installed addons (plugin toggles in the action bar). */
  async function loadAddons(): Promise<void> {
    addonsLoading.value = true
    try {
      const r = await gateway.listAddons()
      if (r.ok) addons.value = r.data
    } finally {
      addonsLoading.value = false
    }
  }

  /** Persist a plugin switch and refresh the list. */
  async function toggleAddon(id: string, enabled: boolean): Promise<void> {
    const r = await gateway.setAddonEnabled(id, enabled)
    if (r.ok) await loadAddons()
  }

  /** Queue a file chip in the composer (deduped by path). */
  function queueFile(f: FileTreeNode): void {
    if (f.kind !== 'file' || pendingFiles.value.some((x) => x.path === f.path)) return
    pendingFiles.value = [...pendingFiles.value, f]
  }

  /** Drop a queued chip. */
  function removeQueuedFile(path: string): void {
    pendingFiles.value = pendingFiles.value.filter((x) => x.path !== path)
  }

  /** Append a new user turn and stream the assistant reply. */
  async function send(content: string, options?: ChatOptions): Promise<boolean> {
    const ws = workspace.active
    const text = content.trim()
    if (!ws || !text || streaming.value) return false
    // Fold any queued composer chips into the turn as @file references so the
    // agent sees them on the first reply, then clear the chip queue.
    const refs = pendingFiles.value.map((f) => `@file ${f.path}`)
    const body = refs.length ? `${[...refs, text].join('\n')}` : text
    messages.value.push({
      id: `u-${Date.now()}`,
      role: 'user',
      content: body,
      createdAt: Date.now(),
    })
    pendingFiles.value = []
    streaming.value = true
    busy.value = true
    try {
      const r = await gateway.sendChat(ws.path, text, messages.value, (m) => upsert(m), options)
      return r.ok
    } finally {
      if (streaming.value) {
        for (const m of messages.value) m.pending = false
        streaming.value = false
      }
      busy.value = false
    }
  }

  async function abort() {
    const ws = workspace.active
    if (!ws) return
    await gateway.abortChat(ws.path)
    for (const m of messages.value) m.pending = false
    streaming.value = false
    busy.value = false
  }

  /** Insert or replace a streamed message (matched by id). */
  function upsert(m: ChatMessage) {
    const idx = messages.value.findIndex((x) => x.id === m.id)
    if (idx >= 0) messages.value[idx] = m
    else messages.value.push(m)
  }

  /** Drop the conversation (used by the "New chat" action). */
  async function clear() {
    if (streaming.value) await abort()
    messages.value = []
  }

  /** Attach a file reference to the conversation without sending a turn
   *  (explorer "Add to Conversation"). The agent sees it on the next reply. */
  function attach(filePath: string) {
    messages.value.push({
      id: `ref-${Date.now()}`,
      role: 'user',
      content: `@file ${filePath}`,
      createdAt: Date.now(),
    })
  }

  return {
    messages,
    streaming,
    busy,
    files,
    filesLoading,
    addons,
    addonsLoading,
    pendingFiles,
    send,
    abort,
    clear,
    attach,
    loadFiles,
    loadAddons,
    toggleAddon,
    queueFile,
    removeQueuedFile,
  }
})