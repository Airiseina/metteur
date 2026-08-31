/**
 * Small path helpers for display only. Workspace paths come from the daemon as
 * OS-native strings (backslashes on Windows, slashes elsewhere), so split on
 * both separators before picking the trailing segment.
 */

/** Last path segment, used as the project/tab label. Falls back to the path itself. */
export function projectName(path: string): string {
  const segments = path.split(/[\\/]/).filter(Boolean)
  return segments[segments.length - 1] ?? path
}

/** Parent directory of a workspace-relative path (`""` for a root child). */
export function dirOf(path: string): string {
  const idx = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
  return idx < 0 ? '' : path.slice(0, idx)
}