import type { editor as MonacoEditor, IRange, languages as MonacoLanguages } from 'monaco-editor'
import { gateway } from '@/core'
import { NODE_PRESETS } from '@/lib/blueprint'

/**
 * Monaco language service for the blueprint DSL (`.mbp` files).
 *
 * Registers the language once per window with:
 * - a monarch tokenizer: `#` comments, strings, numbers, nodes and the arrow
 *   operators stand out while wiring reads like Unreal's editor;
 * - completion for the header keywords and every built-in node kind;
 * - live compile diagnostics: the text is compiled (debounced) through the
 *   daemon's `compileDsl`, and grammar/schema errors are marked inline at the
 *   reported line/column.
 */

/** The Monaco namespace as exported by the lazy loader. */
type Monaco = typeof import('@/lib/monaco').default

let registered = false

/** Monarch tokenizer for the DSL. */
const tokenizer: MonacoLanguages.IMonarchLanguage = {
  tokenizer: {
    root: [
      [/"([^"\\]|\\.)*"/, 'string'],
      [/#[^\n]*/, 'comment'],
      [/\b(?:blueprint|entry|true|false)\b/, 'keyword'],
      [/\d+(\.\d+)?([eE][+-]?\d+)?/, 'number'],
      [/->|<-/, 'operator'],
      [/[(){},:=$]/, 'delimiter'],
      [/[a-zA-Z_][\w-]*/, 'identifier'],
      [/\s+/, 'white'],
    ],
  },
}

/** Registers the `mbp` language with Monaco (idempotent across models). */
export function ensureMbpLanguage(monaco: Monaco): void {
  if (registered) return
  registered = true
  monaco.languages.register({ id: 'mbp' })
  monaco.languages.setMonarchTokensProvider('mbp', tokenizer)

  const { Keyword, Class } = monaco.languages.CompletionItemKind
  const kw = (label: string, insertText: string, detail: string) => ({
    kind: Keyword,
    label,
    insertText,
    detail,
  })
  // Tool presets (SubAgentTool, ...) are canvas aliases for kind `Tool`;
  // only real daemon kinds complete in DSL headers.
  const kindEntries = Object.keys(NODE_PRESETS)
    .filter((kind) => !NODE_PRESETS[kind].kind)
    .map((kind) => ({
      kind: Class,
      label: kind,
      insertText: `${kind}()`,
      detail: 'Add a node of this kind',
    }))
  monaco.languages.registerCompletionItemProvider('mbp', {
    triggerCharacters: ['b', 'e', 'a', 'B', 'E', 'A'],
    provideCompletionItems(model, position) {
      // Suggest node kinds only while a node header (`alias: Kind`) is being
      // typed; keywords are always offered. Items replace the word under the
      // cursor, which Monaco requires every suggestion to declare.
      const line = model.getLineContent(position.lineNumber).slice(0, position.column)
      const inHeader = /^[\s]*[a-zA-Z_][\w-]*\s*:\s*$/.test(line)
      const word = model.getWordUntilPosition(position)
      const range: IRange = {
        startLineNumber: position.lineNumber,
        endLineNumber: position.lineNumber,
        startColumn: word.startColumn,
        endColumn: word.endColumn,
      }
      const suggestions = [
        kw('blueprint', 'blueprint ""', 'Start the document with a blueprint name'),
        kw('entry', 'entry ', 'Mark a node as the entry node'),
        ...(inHeader ? kindEntries : []),
      ].map((item) => ({ ...item, range }))
      return { suggestions }
    },
  })
}

/** Matches the `(line L, column C)` suffix appended to DSL compile errors. */
const POSITION_RE = /\(line (\d+), column (\d+)\)/

/** Compiles the model's text on edits (debounced) and marks errors inline. */
export function bindMbpDiagnostics(monaco: Monaco, model: MonacoEditor.ITextModel): void {
  const update = async () => {
    const text = model.getValue()
    const r = await gateway.compileDsl(text)
    if (!r.ok && r.error) {
      const m = POSITION_RE.exec(r.error)
      if (m) {
        const line = Number(m[1])
        const col = Number(m[2])
        monaco.editor.setModelMarkers(model, 'mbp-dsl', [
          {
            severity: monaco.MarkerSeverity.Error,
            startLineNumber: line,
            startColumn: col,
            endLineNumber: line,
            endColumn: col + 1,
            message: r.error,
          },
        ])
        return
      }
    }
    monaco.editor.setModelMarkers(model, 'mbp-dsl', [])
  }
  let timer: ReturnType<typeof setTimeout> | undefined
  model.onDidChangeContent(() => {
    clearTimeout(timer)
    timer = setTimeout(() => void update(), 350)
  })
  void update()
}