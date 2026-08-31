import { hash } from 'ohash'
import { useWorkspaceStore } from '@/stores/workspace'

/**
 * Workspace-scoped URL helpers.
 *
 * Every workspace surface lives under `/work/{id}/…`, where `id` is a
 * deterministic short XXH3 hash of the workspace root path (stable across
 * reloads). All navigations go through `wurl()` so the prefix never drifts.
 */

const ID_PATH_KEY = 'metteur.workspace-ids'

/** Normalized, platform-independent form of a path. */
function normalizePath(path: string): string {
  return path.replace(/[\\/]+/g, '/')
}

/** Stable, URL-safe short hash (first 11 xxh3 chars) of a path. */
function shortHash(path: string): string {
  return hash(normalizePath(path)).slice(0, 11)
}

/** Stable, URL-safe id derived from a workspace root path. */
export function workspaceIdOf(path: string): string {
  return `w${shortHash(path)}`
}

/** Remember the `id → path` mapping so ids stay resolvable across sessions. */
function rememberId(id: string, path: string) {
  try {
    const map = JSON.parse(localStorage.getItem(ID_PATH_KEY) ?? '{}')
    map[id] = path
    localStorage.setItem(ID_PATH_KEY, JSON.stringify(map))
  } catch {
    // Storage unavailable: the mapping just won't persist.
  }
}

/** `id`-lookup for the currently active workspace, if any. */
export function currentWorkspaceId(): string {
  const ws = useWorkspaceStore().active
  if (!ws) return ''
  const id = workspaceIdOf(ws.path)
  rememberId(id, ws.path)
  return id
}

/** Base URL prefix for the active workspace (empty when none is open). */
export function baseUrl(): string {
  const id = currentWorkspaceId()
  return id ? `/work/${id}` : ''
}

/** Resolves a workspace-relative path (e.g. `/explorer`) against the base. */
export function wurl(surface: string): string {
  return `${baseUrl()}${surface}`
}

/**
 * Default route into the IDE shell for the active workspace
 * (`/work/{id}/explorer`), or `''` when none is open.
 */
export function rootRoute(): string {
  const id = currentWorkspaceId()
  return id ? `/work/${id}/explorer` : ''
}