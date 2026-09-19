/**
 * Server-sent events over `fetch`.
 *
 * `EventSource` cannot send a request body, and a chat turn is a POST, so the
 * stream is read from the response body by hand. The parsing rules that matter
 * here are the ones a naive splitter gets wrong: a chunk boundary can land in
 * the middle of a line, `data:` may repeat, and comments (`:` lines) carry the
 * keep-alives that must not surface as events.
 */

/** One complete SSE message. */
export interface SseMessage {
  /** Concatenated `data:` lines, newline-separated. */
  data: string
}

/** Parser state of one stream: the `data:` lines seen so far. */
interface ParserState {
  data: string[]
}

/**
 * Reads an SSE response body as a message stream.
 *
 * The generator ends when the server closes the stream; aborting `signal`
 * rejects the pending read, which the caller sees as a cancelled turn.
 */
export async function* readSse(
  response: Response,
  signal?: AbortSignal,
): AsyncGenerator<SseMessage, void, void> {
  const body = response.body
  if (!body) return
  const reader = body.getReader()
  const decoder = new TextDecoder()
  const state: ParserState = { data: [] }
  let buffer = ''
  try {
    for (;;) {
      if (signal?.aborted) return
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      let boundary = buffer.indexOf('\n')
      while (boundary >= 0) {
        const message = consumeLine(state, buffer.slice(0, boundary).replace(/\r$/, ''))
        buffer = buffer.slice(boundary + 1)
        if (message) yield message
        boundary = buffer.indexOf('\n')
      }
    }
    // A final line without a trailing newline still counts.
    const tail = consumeLine(state, buffer.replace(/\r$/, ''))
    if (tail) yield tail
  } finally {
    reader.releaseLock()
  }
}

/**
 * Folds one raw line into the pending message.
 *
 * Returns a message on the blank line that separates events, and `null`
 * otherwise. Comments (keep-alives) and unknown fields are ignored; repeated
 * `data:` fields join with newlines, as the specification requires.
 */
function consumeLine(state: ParserState, line: string): SseMessage | null {
  if (line === '') {
    if (!state.data.length) return null
    const data = state.data.join('\n')
    state.data = []
    return { data }
  }
  if (line.startsWith(':')) return null
  const colon = line.indexOf(':')
  const field = colon < 0 ? line : line.slice(0, colon)
  if (field !== 'data') return null
  let value = colon < 0 ? '' : line.slice(colon + 1)
  if (value.startsWith(' ')) value = value.slice(1)
  state.data.push(value)
  return null
}
