import { defineStore } from 'pinia'
import { ref } from 'vue'
import { gateway } from '@/core'
import type { WatchEvent } from '@/core'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * Subscribes to the daemon's live file-change stream for the active workspace
 * and fans events out to surfaces (explorer refresh, editor disk sync).
 */
export const useFileWatchStore = defineStore('filewatch', () => {
  const workspaceStore = useWorkspaceStore()
  /** Most recent events, newest last; consumers watch this (capped buffer). */
  const events = ref<WatchEvent[]>([])
  const active = ref(false)
  let abort: AbortController | null = null

  /** Subscribe to the active workspace's fs events (idempotent). */
  function start() {
    const ws = workspaceStore.active
    if (!ws || active.value) return
    active.value = true
    abort = new AbortController()
    const signal = abort.signal
    void gateway.watchWorkspace(ws.path, (e) => {
      if (!active.value) return
      events.value = [...events.value.slice(-64), e]
    }, signal)
  }

  /** Unsubscribe; the in-flight stream is cancelled via the abort signal. */
  function stop() {
    active.value = false
    abort?.abort()
    abort = null
    events.value = []
  }

  return { events, active, start, stop }
})