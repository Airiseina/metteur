<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { editor as MonacoEditor } from 'monaco-editor'
import { useThemeStore } from '@/stores/theme'

/**
 * Code editor backed by Monaco Editor, with a CodeMirror fallback for coarse
 * pointers (mobile). Both runtimes are imported lazily so the heavy editor
 * payload is fetched only when actually needed.
 *
 * The parent binds `modelValue` and receives edits via `update:modelValue`;
 * `undo()` / `redo()` are exposed so the shell toolbar can drive them.
 */
const props = defineProps<{
  modelValue: string
  language: string
}>()
const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
  (e: 'history', canUndo: boolean, canRedo: boolean): void
}>()

const themeStore = useThemeStore()

const container = ref<HTMLDivElement | null>(null)
const mode = ref<'loading' | 'monaco' | 'cm'>('loading')
/** Whether the active editor has anything to undo/redo (drives tool buttons). */
const canUndo = ref(false)
const canRedo = ref(false)

function syncUndoState() {
  // Monaco exposes canUndo/canRedo/isDisposed on the text *model* (absent from
  // IStandaloneCodeEditor), so drive the toolbar state from it.
  const model = monoModel as unknown as {
    isDisposed(): boolean
    canUndo(): boolean
    canRedo(): boolean
  }
  if (monoEditor && monoModel && !model.isDisposed()) {
    canUndo.value = model.canUndo()
    canRedo.value = model.canRedo()
    emit('history', canUndo.value, canRedo.value)
  }
}

function monacoLanguage(lang: string): string {
  if (lang === 'json') return 'json'
  if (lang.startsWith('ts')) return 'typescript'
  if (lang === 'js' || lang === 'jsx' || lang === 'mjs' || lang === 'cjs') return 'javascript'
  if (lang === 'html' || lang === 'htm') return 'html'
  if (lang === 'css' || lang === 'scss' || lang === 'less') return 'css'
  return 'plaintext'
}

let monacoMod: typeof import('@/lib/monaco').default | null = null
let monoEditor: MonacoEditor.IStandaloneCodeEditor | null = null
let monoModel: MonacoEditor.ITextModel | null = null

function applyMonacoTheme() {
  monacoMod?.editor.setTheme(themeStore.mode === 'dark' ? 'vs-dark' : 'vs')
}

async function setupMonaco() {
  const mod = (await import('@/lib/monaco')).default
  monacoMod = mod
  applyMonacoTheme()
  const model = mod.editor.createModel(props.modelValue, monacoLanguage(props.language))
  monoModel = model
  monoEditor = mod.editor.create(container.value!, {
    model,
    automaticLayout: true,
    minimap: { enabled: false },
    fontSize: 12.5,
    lineHeight: 20,
    tabSize: 2,
    scrollBeyondLastLine: false,
    padding: { top: 8, bottom: 8 },
    fontFamily: "'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace",
    renderWhitespace: 'selection',
    fixedOverflowWidgets: true,
  })
  monoEditor.onDidChangeModelContent(() => {
    const value = monoEditor!.getValue()
    if (value !== props.modelValue) emit('update:modelValue', value)
    syncUndoState()
  })
  syncUndoState()
}

let cmView: { dispatch(t: unknown): void; destroy(): void; state: { doc: { toString(): string } } } | null = null
let cmUndo: ((v: unknown) => boolean) | null = null
let cmRedo: ((v: unknown) => boolean) | null = null

async function cmLanguage() {
  const jsmod = await import('@codemirror/lang-javascript')
  const jsonmod = await import('@codemirror/lang-json')
  // v6 only ships `javascript`; TypeScript files fall back to it for the
  // mobile CodeMirror path.
  const lang = props.language
  if (lang === 'json') return jsonmod.json()
  if (lang === 'js' || lang === 'jsx' || lang === 'mjs' || lang === 'cjs' || lang.startsWith('ts'))
    return jsmod.javascript()
  return null
}

function cmBaseTheme() {
  return {
    '&': {
      color: 'var(--foreground)',
      backgroundColor: 'var(--background)',
      height: '100%',
      fontSize: '12.5px',
    },
    '.cm-content': {
      caretColor: 'var(--primary)',
      fontFamily: "'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace",
      padding: '8px 0',
    },
    '&.cm-focused': { outline: 'none' },
    '.cm-line': { padding: '0 12px' },
    '.cm-gutters': {
      backgroundColor: 'transparent',
      color: 'var(--subtle)',
      border: 'none',
    },
    '.cm-activeLine': { backgroundColor: 'var(--hover)' },
    '.cm-activeLineGutter': { backgroundColor: 'transparent' },
    '.cm-cursor': { borderLeftColor: 'var(--primary)' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': {
      backgroundColor: 'var(--selection)',
    },
    '.cm-matchingBracket': {
      backgroundColor: 'color-mix(in srgb, var(--primary) 22%, transparent)',
      outline: '1px solid var(--grid-dot)',
    },
  }
}

async function setupCm() {
  const viewMod = await import('@codemirror/view')
  const stateMod = await import('@codemirror/state')
  const langMod = await import('@codemirror/language')
  const cmdsMod = await import('@codemirror/commands')
  cmUndo = (v) => (cmdsMod.undo(v as never) ?? true)
  cmRedo = (v) => (cmdsMod.redo(v as never) ?? true)

  const stateSupport = (await cmLanguage()) as unknown
  const extensions = [
    viewMod.lineNumbers(),
    viewMod.highlightActiveLine(),
    viewMod.highlightActiveLineGutter(),
    viewMod.highlightSpecialChars(),
    viewMod.drawSelection(),
    viewMod.dropCursor(),
    stateMod.EditorState.allowMultipleSelections.of(true),
    langMod.indentOnInput(),
    langMod.bracketMatching(),
    langMod.syntaxHighlighting(langMod.defaultHighlightStyle, { fallback: true }),
    cmdsMod.history(),
    viewMod.keymap.of([...cmdsMod.defaultKeymap, ...cmdsMod.historyKeymap, cmdsMod.indentWithTab]),
    viewMod.EditorView.theme(cmBaseTheme(), { dark: themeStore.mode === 'dark' }),
    viewMod.EditorView.updateListener.of((update: { docChanged: boolean; state: { doc: { toString(): string } } }) => {
      if (update.docChanged) {
        const text = update.state.doc.toString()
        if (text !== props.modelValue) emit('update:modelValue', text)
      }
    }),
  ]
  if (stateSupport) extensions.push(stateSupport as never)

  const view = new viewMod.EditorView({
    parent: container.value!,
    extensions,
  })
  cmView = view
}

const isCoarsePointer = () => window.matchMedia?.('(pointer: coarse)').matches ?? false

onMounted(async () => {
  if (isCoarsePointer()) {
    mode.value = 'cm'
    await setupCm()
  } else {
    mode.value = 'monaco'
    await setupMonaco()
  }
})

onBeforeUnmount(() => {
  if (monoEditor) monoEditor.dispose()
  if (monoModel) monoModel.dispose()
  if (cmView) cmView.destroy()
})

watch(
  () => themeStore.mode,
  () => applyMonacoTheme(),
)

// Push externally-set content (reload-from-disk / tab restore) into the live
// editor instead of leaving the model stale.
watch(
  () => props.modelValue,
  (value) => {
    if (monoEditor && monoModel && !monoModel.isDisposed()) {
      if (monoEditor.getValue() !== value) monoEditor.setValue(value)
    } else if (cmView) {
      const cur = cmView.state.doc.toString()
      if (cur !== value) cmView.dispatch({ changes: { from: 0, to: cur.length, insert: value } })
    }
  },
)

defineExpose({
  undo() {
    if (monoEditor) {
      monoEditor.trigger('toolbar', 'undo', undefined)
      setTimeout(syncUndoState, 0)
    } else if (cmView && cmUndo) cmUndo(cmView)
  },
  redo() {
    if (monoEditor) {
      monoEditor.trigger('toolbar', 'redo', undefined)
      setTimeout(syncUndoState, 0)
    } else if (cmView && cmRedo) cmRedo(cmView)
  },
  canUndo,
  canRedo,
})
</script>

<template>
  <div class="h-full w-full min-h-0 overflow-hidden bg-background">
    <div v-if="mode === 'loading'" class="flex h-full items-center justify-center text-[12px] text-subtle">
      Loading editor…
    </div>
    <div v-else ref="container" class="h-full w-full" />
  </div>
</template>
