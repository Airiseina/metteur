import { defineStore } from 'pinia'
import { ref, watch } from 'vue'
import { gateway } from '@/core'
import type {
  AddonInfo,
  ChatMessage,
  ChatOptions,
  ChatSessionInfo,
  ChatUsage,
  FileTreeNode,
} from '@/core'
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
  /** All persisted threads, newest first. */
  const threads = ref<ChatSessionInfo[]>([])
  /** Token usage of the most recently completed turn. */
  const lastUsage = ref<ChatUsage | null>(null)

  // Files / addons driving the composer's attach, @-mention and plugin toggles.
  const files = ref<FileTreeNode[]>([])
  const filesLoading = ref(false)
  const addons = ref<AddonInfo[]>([])
  const addonsLoading = ref(false)
  /** Files queued in the composer (chips) but not yet sent as a turn. */
  const pendingFiles = ref<FileTreeNode[]>([])

  // Switching workspaces resets the surface and restores the new session.
  // Immediate so a store created after the workspace was already opened still
  // loads the persisted conversation (otherwise restore would never run).
  watch(
    () => workspace.active?.path,
    async () => {
      messages.value = []
      streaming.value = false
      busy.value = false
      pendingFiles.value = []
      sessionId.value = ''
      lastUsage.value = null
      if (workspace.active?.path) await restore()
    },
    { immediate: true },
  )

  /** Reload the persisted conversation of the active workspace, if any. */
  async function restore(): Promise<void> {
    const ws = workspace.active
    if (!ws) return
    const target = ws.path
    const sessions = await gateway.listChatSessions(target)
    if (!sessions.ok || sessions.data.length === 0) {
      threads.value = []
      return
    }
    threads.value = sessions.data
    const latest = sessions.data[0]
    await openThread(target, latest.sessionId, latest.title)
  }

  /** Reload the thread list without touching the open conversation. */
  async function refreshThreads(): Promise<void> {
    const ws = workspace.active
    if (!ws) return
    const sessions = await gateway.listChatSessions(ws.path)
    if (sessions.ok) threads.value = sessions.data
  }

  /** Switch the surface to an existing thread. */
  async function switchTo(sessionId: string): Promise<void> {
    const ws = workspace.active
    if (!ws) return
    await openThread(ws.path, sessionId, '')
  }

  /** Load one thread into the surface (empty history on failure). */
  async function openThread(wsPath: string, id: string, title: string): Promise<void> {
    if (streaming.value) await abort()
    const snap = await gateway.getChatSession(wsPath, id)
    // The user may have switched workspaces while loading; discard stale data.
    if (workspace.active?.path !== wsPath) return
    sessionId.value = id
    if (snap.ok) {
      messages.value = snap.data.history
      sessionId.value = snap.data.sessionId
    } else {
      messages.value = []
    }
    if (title) {
      const info = threads.value.find((t) => t.sessionId === id)
      if (info) info.title = title
    }
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
    // A "new chat" turn creates a fresh thread; sending into an existing one
    // keeps its id (non-empty below).
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
    lastUsage.value = null
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
        (usage) => {
          lastUsage.value = usage
        },
      )
      await refreshThreads()
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

  /** Start a fresh conversation without touching the previous thread.
   *
   * The prior thread stays on disk and reappears in the session list; deleting
   * one is an explicit action (`deleteThread`). */
  async function clear() {
    if (streaming.value) await abort()
    messages.value = []
    sessionId.value = ''
    lastUsage.value = null
    await refreshThreads()
  }

  /** Delete one persisted thread (explicit user action). */
  async function deleteThread(id: string) {
    const ws = workspace.active
    if (!ws) return
    if (streaming.value && sessionId.value === id) await abort()
    await gateway.deleteChatSession(ws.path, id)
    if (sessionId.value === id) {
      messages.value = []
      sessionId.value = ''
      lastUsage.value = null
    }
    await refreshThreads()
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
    threads,
    lastUsage,
    files,
    filesLoading,
    addons,
    addonsLoading,
    pendingFiles,
    send,
    abort,
    clear,
    deleteThread,
    restore,
    switchTo,
    refreshThreads,
    attach,
    loadFiles,
    loadAddons,
    toggleAddon,
    queueFile,
    removeQueuedFile,
  }
})