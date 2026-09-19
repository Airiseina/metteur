/**
 * Human-readable summaries of tool calls.
 *
 * The daemon sends the raw summary it already uses for its own activity line
 * (the subject argument, truncated) plus a `detail_json`. This module turns that
 * into the two parts a one-line activity row needs: a verb the reader recognizes
 * and the subject, so `EditFile` renders as "Edit src/api.ts" rather than as a
 * tool identifier.
 */

import type { ChatMessage } from '@/core'

/**
 * Icon per tool family, named rather than imported so the summary module stays
 * free of UI dependencies; the card resolves the name to a lucide component.
 */
export type ToolIcon =
  | 'read'
  | 'write'
  | 'search'
  | 'command'
  | 'job'
  | 'agent'
  | 'check'
  | 'plan'
  | 'snapshot'
  | 'blueprint'
  | 'context'
  | 'tool'

const ICONS: Record<string, ToolIcon> = {
  ReadFile: 'read',
  WriteFile: 'write',
  EditFile: 'write',
  ListDirectory: 'read',
  SearchFile: 'search',
  Grep: 'search',
  Glob: 'search',
  ExecuteCommand: 'command',
  StartCommand: 'command',
  JobStatus: 'job',
  WaitJob: 'job',
  KillJob: 'job',
  GetDependencies: 'search',
  SpawnSubAgent: 'agent',
  CheckDiagnostics: 'check',
  GetHover: 'check',
  FindDefinition: 'search',
  ReplanBlueprint: 'blueprint',
  DraftBlueprint: 'blueprint',
  SnapshotTake: 'snapshot',
  TodoWrite: 'plan',
  TodoRead: 'plan',
  ReleaseContext: 'context',
}

/** The icon family of a tool call. */
export function toolIcon(name: string | undefined): ToolIcon {
  return ICONS[name ?? ''] ?? 'tool'
}

/** Verbs by tool name; anything unlisted falls back to the name itself. */
const VERBS: Record<string, string> = {
  ReadFile: 'Read',
  WriteFile: 'Write',
  EditFile: 'Edit',
  ListDirectory: 'List',
  SearchFile: 'Find',
  Grep: 'Search',
  Glob: 'Glob',
  ExecuteCommand: 'Run',
  StartCommand: 'Start',
  JobStatus: 'Check job',
  WaitJob: 'Wait for job',
  KillJob: 'Stop job',
  GetDependencies: 'Inspect deps',
  SpawnSubAgent: 'Subagent',
  CheckDiagnostics: 'Check',
  GetHover: 'Inspect',
  FindDefinition: 'Find definition',
  ReplanBlueprint: 'Replan',
  SnapshotTake: 'Snapshot',
  DraftBlueprint: 'Draft blueprint',
  TodoWrite: 'Update plan',
  TodoRead: 'Read plan',
  ReleaseContext: 'Release context',
}

/** The verb of a tool row, e.g. `Read` or `Run`. */
export function toolVerb(name: string | undefined): string {
  const tool = name ?? ''
  return VERBS[tool] ?? tool
}

/** Structured fields a tool message may carry. */
interface ToolDetail {
  callId?: string
  summary?: string
  ok?: boolean
  elapsedMs?: number
  running?: boolean
  progress?: boolean
  file?: string
}

/** Reads the structured payload of a tool message. */
export function toolDetail(message: ChatMessage): ToolDetail {
  return (message.detail ?? {}) as ToolDetail
}

/** The subject of a tool row (a path, a command, a pattern). */
export function toolSubject(message: ChatMessage): string {
  const detail = toolDetail(message)
  if (detail.summary) return detail.summary
  const first = (message.content ?? '').split('\n')[0]?.trim() ?? ''
  return first.length > 96 ? `${first.slice(0, 95)}…` : first
}

/** Duration rendered as `840ms` / `4.2s` / `1m 12s`. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return ''
  if (ms < 1000) return `${Math.round(ms)}ms`
  const seconds = ms / 1000
  if (seconds < 60) return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)}s`
  const minutes = Math.floor(seconds / 60)
  return `${minutes}m ${Math.round(seconds % 60)}s`
}

/** Elapsed seconds since `startedAt`, for live timers. */
export function elapsedSeconds(startedAt: number, now: number): number {
  return Math.max(0, Math.round((now - startedAt) / 1000))
}

/** Whether a tool result is a unified diff (an edit result). */
export function isDiffLike(content: string): boolean {
  return /^(diff --git |--- a|@@ )/m.test(content)
}

/** One line of a unified diff, with the tone class it renders with. */
export interface DiffLine {
  text: string
  tone: string
}

/** Splits a unified diff into tone-tagged lines. */
export function diffLines(content: string): DiffLine[] {
  return content.split(/\r?\n/).map((line) => {
    let tone = ''
    if (line.startsWith('@@')) tone = 'diff-hunk'
    else if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('diff ')) {
      tone = 'diff-meta'
    } else if (line.startsWith('+')) tone = 'diff-add'
    else if (line.startsWith('-')) tone = 'diff-del'
    return { text: line, tone }
  })
}

/** Added/removed line counts of a diff, for the row's `+N −M` badge. */
export function diffStat(content: string): { added: number; removed: number } | null {
  if (!isDiffLike(content)) return null
  let added = 0
  let removed = 0
  for (const line of content.split(/\r?\n/)) {
    if (line.startsWith('+++') || line.startsWith('---')) continue
    if (line.startsWith('+')) added += 1
    else if (line.startsWith('-')) removed += 1
  }
  return { added, removed }
}

/** Lines of a command's output, capped for the collapsed view. */
export function outputLines(content: string, limit = 400): string[] {
  const lines = content.split(/\r?\n/)
  return lines.length <= limit ? lines : [...lines.slice(0, limit), `… ${lines.length - limit} more lines`]
}
