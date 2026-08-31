import { defineStore } from 'pinia'
import { ref } from 'vue'
import { gateway } from '@/core'
import type { AddonInfo, UsageSummary } from '@/core'
import { useWorkspaceStore } from './workspace'

/**
 * Addon registry.
 *
 * Lists installed addons, toggles each one's enabled state and shows the usage
 * summary of the workspace's most recent run on refresh.
 */
export const useAddonStore = defineStore('addon', () => {
  const workspace = useWorkspaceStore()
  const addons = ref<AddonInfo[]>([])
  const usage = ref<UsageSummary | null>(null)

  async function refresh() {
    const [a, runs] = await Promise.all([gateway.listAddons(), loadRuns()])
    if (a.ok) addons.value = a.data
    usage.value = runs
  }

  async function loadRuns(): Promise<UsageSummary | null> {
    const ws = workspace.active
    if (!ws) return null
    const runs = await gateway.listExecutions(ws.path)
    const runId = runs.ok ? runs.data.find((r) => r.status === 'Running')?.runId ?? runs.data[0]?.runId : undefined
    if (!runId) return null
    const u = await gateway.getExecutionUsage(ws.path, runId)
    return u.ok ? u.data : null
  }

  async function setEnabled(id: string, enabled: boolean) {
    const r = await gateway.setAddonEnabled(id, enabled)
    if (r.ok) await refresh()
  }

  return { addons, usage, refresh, setEnabled }
})