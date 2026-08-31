import { defineStore } from 'pinia'
import { ref } from 'vue'
import { gateway } from '@/core'
import type { FileHistoryEntry, SnapshotInfo } from '@/core'
import { useWorkspaceStore } from './workspace'

/**
 * Version timeline.
 *
 * Lists snapshots of the active workspace, supports rollback to any of them,
 * and shows the file history (with diffs) for a chosen file.
 */
export const useVersionStore = defineStore('version', () => {
  const workspace = useWorkspaceStore()
  const snapshots = ref<SnapshotInfo[]>([])
  const history = ref<FileHistoryEntry[]>([])
  const selectedFile = ref<string | null>(null)
  const selectedId = ref<string | null>(null)

  async function refresh() {
    const ws = workspace.active
    if (!ws) return
    const r = await gateway.listSnapshots(ws.path)
    if (r.ok) {
      snapshots.value = r.data
      // Keep the selection valid across refreshes.
      if (selectedId.value && !r.data.some((s) => s.id === selectedId.value)) selectedId.value = null
    }
  }

  function select(id: string | null) {
    selectedId.value = id
  }

  async function pickFile(path: string) {
    selectedFile.value = path
    const ws = workspace.active
    if (!ws) return
    const r = await gateway.listFileHistory(ws.path, path)
    if (r.ok) history.value = r.data
  }

  async function rollback(snapshotId: string) {
    const ws = workspace.active
    if (!ws) return
    const r = await gateway.rollback(ws.path, snapshotId)
    if (r.ok) await refresh()
  }

  /** Create a snapshot of the workspace with an optional alias. */
  async function create(description: string, alias?: string): Promise<boolean> {
    const ws = workspace.active
    if (!ws) return false
    const r = await gateway.createSnapshot(ws.path, description || 'snapshot', alias)
    if (r.ok) await refresh()
    return r.ok
  }

  return { snapshots, history, selectedFile, selectedId, refresh, select, pickFile, rollback, create }
})
