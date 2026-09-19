/**
 * Liveness: what the agent is doing right now.
 *
 * A spinner alone cannot answer "is it working or is it stuck", so the chat
 * derives a phase from the event stream and pairs it with an elapsed time. The
 * phase is deliberately coarse — four states a user can act on — and is always
 * rendered with text, never with colour alone.
 */

/** What the agent is currently doing. */
export type ChatPhase = 'idle' | 'thinking' | 'generating' | 'tool' | 'waiting'

/** How long without an event before "working" reads as "waiting". */
export const STALL_AFTER_MS = 1500

/** How long before the UI says out loud that the model is slow. */
export const SLOW_AFTER_MS = 20_000

/** The label shown in the status line. */
export function phaseLabel(phase: ChatPhase): string {
  switch (phase) {
    case 'thinking':
      return 'Thinking'
    case 'generating':
      return 'Writing'
    case 'tool':
      return 'Running a tool'
    case 'waiting':
      return 'Waiting for the model'
    case 'idle':
      return ''
  }
}

/**
 * Refines a phase using how long ago the last event arrived.
 *
 * A tool phase with no events for a while is really the model being slow, and
 * the status line should say so instead of naming a tool that already finished.
 */
export function effectivePhase(phase: ChatPhase, sinceLastEventMs: number): ChatPhase {
  if (phase === 'idle') return phase
  if (phase !== 'waiting' && sinceLastEventMs >= STALL_AFTER_MS) return 'waiting'
  return phase
}

/** Whether the status line should admit that the model is taking a while. */
export function isSlow(sinceLastEventMs: number): boolean {
  return sinceLastEventMs >= SLOW_AFTER_MS
}

/** Formats a token count as `1.2k` / `12k` / `845`. */
export function formatTokens(count: number): string {
  if (count < 1000) return String(count)
  if (count < 10_000) return `${(count / 1000).toFixed(1)}k`
  return `${Math.round(count / 1000)}k`
}

/** Cache hit rate of a turn as `62%`, or an empty string when unreported. */
export function cacheHitRate(cached: number, input: number): string {
  if (!cached || !input || input <= 0) return ''
  return `${Math.round((cached / input) * 100)}%`
}
