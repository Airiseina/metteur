import { parse as tomlParse, stringify as tomlStringify } from 'smol-toml'

/**
 * Shared TOML utilities for the layered configuration.
 *
 * Config is a plain JSON-able object (mirrors `metteur_shared::config::Config`)
 * that the daemon persists as `.metteur/config.toml`. Views serialize it to
 * TOML for editing and parse user input back; the gateway exposes the two
 * config files as *virtual documents* (`metteur://config/user.toml` and
 * `metteur://config/workspace.toml`) so they open in the main editor tab.
 */

export const parseToml = tomlParse
export const stringifyToml = tomlStringify

/** Drop `null`/`undefined` leaves — TOML has no representation for them. */
export function dropNulls(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(dropNulls)
  if (v && typeof v === 'object') {
    const out: Record<string, unknown> = {}
    for (const [key, val] of Object.entries(v as Record<string, unknown>)) {
      if (val === null || val === undefined) continue
      out[key] = dropNulls(val)
    }
    return out
  }
  return v
}

/** Serialize a config layer to TOML defensively (empty/non-object → `''`). */
export function configToToml(cfg: Record<string, unknown>): string {
  const clean = dropNulls(cfg)
  if (!clean || typeof clean !== 'object' || Array.isArray(clean)) return ''
  try {
    return tomlStringify(clean)
  } catch {
    return ''
  }
}

/** Parse a TOML string, returning `null` when it is not a valid object. */
export function tryParseToml(text: string): Record<string, unknown> | null {
  try {
    const parsed = tomlParse(text)
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>
    }
  } catch {
    // fall through
  }
  return null
}

/* Virtual config documents opened in the main editor tab ------------------ */

/** Virtual path of the user (global) config file. */
export const USER_CONFIG_PATH = 'metteur://config/user.toml'
/** Virtual path of the active workspace's config file. */
export const WORKSPACE_CONFIG_PATH = 'metteur://config/workspace.toml'

/** Whether `path` is a virtual config document. */
export function isConfigDoc(path: string): boolean {
  return path.startsWith('metteur://config/')
}

/** Which scope a virtual config document targets. */
export function configScopeOf(path: string): 'user' | 'workspace' {
  return path === WORKSPACE_CONFIG_PATH ? 'workspace' : 'user'
}