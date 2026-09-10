import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { gateway } from '@/core'
import type { DaemonConfig } from '@/core'
import { useWorkspaceStore } from './workspace'

/**
 * Layered daemon configuration (VSCode user/workspace model).
 *
 * The daemon persists two layers — the *user* layer (`~/.metteur/config.toml`)
 * and the *workspace* layer (`<ws>/.metteur/config.toml`). The effective value
 * is the workspace layer merged over the user layer per key, mirroring
 * `metteur_shared::config::Config::merge` (empty collections fall back to the
 * user layer). Edits land in exactly one layer; saving persists it through
 * `daemon.setConfig`, which also re-merges the affected workspace.
 */

/** Effective-config merge: workspace overrides user per (nested) key. */
export function mergeConfig(user: DaemonConfig, ws: DaemonConfig): DaemonConfig {
  const out: DaemonConfig = { ...user }
  for (const key of Object.keys(ws)) {
    const wv = ws[key]
    if (wv === undefined || wv === null) continue
    if (Array.isArray(wv)) {
      if (wv.length > 0) out[key] = wv
    } else if (typeof wv === 'object') {
      const uv = user[key]
      if (uv && typeof uv === 'object' && !Array.isArray(uv)) {
        out[key] = mergeSection(uv as Record<string, unknown>, wv as Record<string, unknown>)
      } else if (Object.keys(wv as Record<string, unknown>).length > 0) {
        out[key] = wv
      }
    } else {
      out[key] = wv
    }
  }
  return out
}

/** Field-level merge inside one section; empty collections fall back. */
function mergeSection(
  user: Record<string, unknown>,
  ws: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = { ...user }
  for (const key of Object.keys(ws)) {
    const wv = ws[key]
    if (wv === undefined || wv === null) continue
    if (Array.isArray(wv)) {
      if (wv.length > 0) out[key] = wv
    } else if (typeof wv === 'object') {
      if (Object.keys(wv as Record<string, unknown>).length > 0) out[key] = wv
    } else {
      out[key] = wv
    }
  }
  return out
}

export type ConfigLayer = 'user' | 'workspace'

export const useConfigStore = defineStore('config', () => {
  const workspace = useWorkspaceStore()
  /** User (global) layer, edited in place before `save('user')`. */
  const user = ref<DaemonConfig>({})
  /** Workspace layer, edited in place before `save('workspace')`. */
  const ws = ref<DaemonConfig>({})
  /** Whether both layers were fetched from the daemon. */
  const loaded = ref(false)
  const saving = ref(false)
  const lastError = ref('')

  const hasWorkspace = computed(() => !!workspace.active)
  /** User + workspace merged per key (what actually applies). */
  const effective = computed(() => mergeConfig(user.value, ws.value))

  const read = async <T>(p: Promise<{ ok: boolean; data?: T; error?: string }>): Promise<T | null> => {
    const r = await p
    return r.ok ? (r.data as T) : null
  }

  /** Fetch both layers from the daemon (no-op fail keeps last values). */
  async function load(): Promise<void> {
    const activePath = workspace.active?.path
    const [u, w] = await Promise.all([
      read(gateway.getConfig('')),
      activePath ? read(gateway.getConfig(activePath)) : Promise.resolve(null),
    ])
    if (u) user.value = u
    // Clear the workspace layer when there is no workspace (or its read
    // failed) so a previous workspace's values cannot leak into this one.
    if (activePath) {
      if (w) ws.value = w
      else ws.value = {}
    } else {
      ws.value = {}
    }
    loaded.value = true
  }

  /** Persist one layer; re-fetches both layers afterwards. Returns an error
   *  message, or `''` on success. */
  async function save(layer: ConfigLayer): Promise<string> {
    saving.value = true
    lastError.value = ''
    try {
      const cfg = layer === 'user' ? user.value : ws.value
      const path = layer === 'workspace' ? workspace.active?.path ?? '' : ''
      const r = await gateway.setConfig(cfg, path)
      if (!r.ok) {
        lastError.value = r.error
        return r.error
      }
      await load()
      return ''
    } finally {
      saving.value = false
    }
  }

  /** Remove a key (or the whole section) from one layer, in place. */
  function resetKey(layer: ConfigLayer, section: string, key?: string) {
    const cfg = layer === 'user' ? user.value : ws.value
    const sec = cfg[section]
    if (!sec || typeof sec !== 'object' || Array.isArray(sec)) return
    const rec = sec as Record<string, unknown>
    if (key === undefined) delete cfg[section]
    else delete rec[key]
  }

  return {
    user,
    ws,
    effective,
    hasWorkspace,
    loaded,
    saving,
    lastError,
    load,
    save,
    resetKey,
  }
})