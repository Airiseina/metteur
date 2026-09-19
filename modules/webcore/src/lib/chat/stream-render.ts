import { renderMarkdown } from '@/lib/markdown'

/**
 * Streaming-friendly markdown rendering.
 *
 * Two costs dominate a naive implementation while tokens arrive: re-parsing the
 * whole message on every delta, and re-highlighting every code block (Shiki
 * runs a grammar over the whole fence, so a growing block is quadratic). This
 * module addresses both:
 *
 * - the render is throttled to one per animation frame, so a burst of deltas
 *   produces a single DOM update;
 * - unfinished messages render code fences without syntax highlighting, and the
 *   highlighted version is produced once, when the message settles.
 *
 * Finished messages are cached by content, so scrolling and re-renders are free.
 */

/** How long a finished render stays cached before the store is dropped. */
const CACHE_LIMIT = 240

const cache = new Map<string, string>()

/** Whether `text` ends inside an open code fence. */
function hasOpenFence(text: string): boolean {
  let open = false
  for (const line of text.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) open = !open
  }
  return open
}

/**
 * Renders `text` for the transcript.
 *
 * `streaming` marks a message that is still receiving deltas: its code blocks
 * are left unhighlighted so each delta stays cheap. Everything else is rendered
 * (and cached) in full.
 */
export function renderStreaming(text: string, streaming: boolean): string {
  const source = text ?? ''
  if (!source) return ''
  const key = `${streaming ? 'p' : 'f'}:${source}`
  const hit = cache.get(key)
  if (hit !== undefined) return hit
  // A fence that is still open would be highlighted on every delta; the plain
  // form is what the reader sees anyway until the block closes.
  const html = renderMarkdown(source, { highlight: !streaming && !hasOpenFence(source) })
  if (cache.size > CACHE_LIMIT) cache.clear()
  cache.set(key, html)
  return html
}

/** Drops the cache, e.g. when the highlighter finished loading or the theme changed. */
export function clearRenderCache(): void {
  cache.clear()
}

/**
 * Batches rapid calls into one per animation frame.
 *
 * Returns a function that schedules `run` and a `cancel` for teardown. The
 * latest arguments win, which is what a delta stream wants.
 */
export function rafThrottle<A extends unknown[]>(run: (...args: A) => void): {
  schedule: (...args: A) => void
  cancel: () => void
} {
  let handle: number | null = null
  let latest: A | null = null
  const flush = () => {
    handle = null
    if (latest) {
      const args = latest
      latest = null
      run(...args)
    }
  }
  return {
    schedule: (...args: A) => {
      latest = args
      if (handle === null) handle = requestAnimationFrame(flush)
    },
    cancel: () => {
      if (handle !== null) cancelAnimationFrame(handle)
      handle = null
      latest = null
    },
  }
}
