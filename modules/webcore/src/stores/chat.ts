import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { gateway } from '@/core'
import type { AddonInfo, ChatMessage, ChatOptions, FileTreeNode } from '@/core'
import { useWorkspaceStore } from './workspace'

/**
 * ReAct conversation for the current workspace.
 *
 * Holds the message list and the sending state so the chat surface stays a thin
 * view. Messages are upserted by id; streamed deltas fill a bubble in place
 * (append while `pending`, replace on the final event) instead of appending
 * duplicates. Agent helpers (workspace files for the attach/@-menu and the
 * installed addons) live here so the composer stays stateless and the context
 * survives tab switches.
 *
 * The conversation is persisted daemon-side per workspace: the store restores
 * it when the workspace opens (surviving daemon restarts) and tracks the
 * session id so later turns resume the same session.
 */
export const useChatStore = defineStore('chat', () => {
  const workspace = useWorkspaceStore()
  const messages = ref<ChatMessage[]>([])
  const streaming = ref(false)
  const busy = ref(false)
  /** id of the persisted session, passed back on every turn. */
  const sessionId = ref('')

  // Files / addons driving the composer's attach, @-mention and plugin toggles.
  const files = ref<FileTreeNode[]>([])
  const filesLoading = ref(false)
  const addons = ref<AddonInfo[]>([])
  const addonsLoading = ref(false)
  /** Files queued in the composer (chips) but not yet sent as a turn. */
  const pendingFiles = ref<FileTreeNode[]>([])

  // Switching workspaces resets the surface and restores the new session.
  watch(
    () => workspace.active?.path,
    async () => {
      messages.value = []
      streaming.value = false
      busy.value = false
      pendingFiles.value = []
      sessionId.value = ''
      if (workspace.active?.path) await restore()
    },
  )

  /** Reload the persisted conversation of the active workspace, if any. */
  async function restore(): Promise<void> {
    const ws = workspace.active
    if (!ws) return
    const target = ws.path
    const sessions = await gateway.listChatSessions(target)
    if (!sessions.ok || sessions.data.length === 0) return
    const snap = await gateway.getChatSession(target)
    if (!snap.ok) return
    // The user may have switched workspaces while loading; discard stale data.
    if (workspace.active?.path !== target) return
    sessionId.value = snap.data.sessionId
    messages.value = snap.data.history
  }

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
      const r = await gateway.sendChat(
        ws.path,
        text,
        messages.value,
        (m) => upsert(m),
        options,
        (id) => {
          sessionId.value = id
        },
        sessionId.value,
      )
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

  /** Insert a message, appending deltas to an open bubble of the same id. */
  function upsert(m: ChatMessage) {
    const idx = messages.value.findIndex((x) => x.id === m.id)
    if (idx < 0) {
      messages.value.push(m)
      return
    }
    const existing = messages.value[idx]
    // Deltas append into the pending bubble; final events replace it.
    if (existing.pending && m.pending) existing.content += m.content
    else messages.value[idx] = m
  }

  /** Drop the conversation (used by the "New chat" action). */
  async function clear() {
    if (streaming.value) await abort()
    const ws = workspace.active
    if (ws) await gateway.deleteChatSession(ws.path)
    messages.value = []
    sessionId.value = ''
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
    sessionId,
    files,
    filesLoading,
    addons,
    addonsLoading,
    pendingFiles,
    send,
    abort,
    clear,
    restore,
    attach,
    loadFiles,
    loadAddons,
    toggleAddon,
    queueFile,
    removeQueuedFile,
  }
})