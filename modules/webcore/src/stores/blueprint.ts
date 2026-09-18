import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { gateway } from '@/core'
import { NODE_PRESETS, pinsFor } from '@/lib/blueprint'
import type { Blueprint, BlueprintEdge, BlueprintNode, BlueprintPin, FunctionItem } from '@/core'

/**
 * Blueprint graph state shared across the editor.
 *
 * Each blueprint *file* keeps its own graph, keyed by the file path, so
 * switching tabs loads and preserves a different canvas. The file is the
 * source of truth: load reads it, save writes it back and mirrors the graph
 * into the daemon's blueprint store (keyed by the file's stable UUID).
 */

interface GraphData {
  id: string
  nodes: BlueprintNode[]
  edges: BlueprintEdge[]
}

/** Edge fields participating in the dirty key. */
type KeyEdge = Pick<BlueprintEdge, 'id' | 'source' | 'target' | 'sourceHandle' | 'targetHandle' | 'label'>

/** Canonical dirty key for a graph, shared with the editor (BlueprintView).
 *  Pin connected-state (`filledIns`/`connectedIn`/`connectedOut`) is derived
 *  from wires and excluded so wires never phantom-dirty a pristine file. */
function keyFor(nodes: BlueprintNode[], edges: KeyEdge[]): string {
  return JSON.stringify({
    nodes: nodes.map((n) => ({
      id: n.id,
      position: n.position,
      title: n.title,
      category: n.category,
      inputs: n.inputs,
      outputs: n.outputs,
      values: n.values ?? {},
    })),
    edges: edges.map((e) => ({
      id: e.id,
      source: e.source,
      sourceHandle: e.sourceHandle,
      target: e.target,
      targetHandle: e.targetHandle,
      label: e.label,
    })),
  })
}

/** A fresh Start → End graph used to seed a brand-new blueprint file.
 *  Pins come from the presets so the seed stays in sync with the palette. */
function seedGraph(): GraphData {
  const nodeId = () => crypto.randomUUID()
  const start = nodeId()
  const end = nodeId()
  const startPins = pinsFor(NODE_PRESETS['Start'])
  const endPins = pinsFor(NODE_PRESETS['End'])
  return {
    id: crypto.randomUUID(),
    nodes: [
      {
        id: start,
        type: 'Start',
        category: 'event',
        title: 'Start',
        position: { x: 40, y: 220 },
        inputs: startPins.inputs,
        outputs: startPins.outputs,
      },
      {
        id: end,
        type: 'End',
        category: 'event',
        title: 'End',
        position: { x: 320, y: 220 },
        inputs: endPins.inputs,
        outputs: endPins.outputs,
      },
    ],
    edges: [
      {
        id: crypto.randomUUID(),
        source: start,
        // The first output pin of Start is its exec outlet, driving the wire.
        sourceHandle: startPins.outputs[0]?.id,
        target: end,
        targetHandle: endPins.inputs[0]?.id,
      },
    ],
  }
}

/** The on-disk name of a blueprint derived from its file path. */
export function fileNameOf(filePath: string): string {
  return filePath.split(/[\\/]/).pop()?.replace(/\.blueprint$/i, '') || 'blueprint'
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i

/** Pin ids used by older canvases, mapped to their current canonical names.
 *  Presets renamed pins over time (`out` → `Result`, `a` → `A`, …); graphs
 *  saved with the old keys still need name alignment to round-trip through the
 *  DSL, whose parser also cannot express whitespace in pin names. */
const LEGACY_PIN_NAMES: Record<string, string> = {
  a: 'A',
  b: 'B',
  in: 'In',
  out: 'Result',
  input: 'Input',
  find: 'Find',
  replaceWith: 'ReplaceWith',
  start: 'Start',
  length: 'Length',
  list: 'List',
  item: 'Item',
  index: 'Index',
  object: 'Object',
  path: 'Path',
  value: 'Value',
  itemA: 'ItemA',
  itemB: 'ItemB',
  itemC: 'ItemC',
  system: 'System',
  prompt: 'Prompt',
  context: 'Context',
  keep: 'Keep',
  ms: 'Ms',
  message: 'Message',
  allowed: 'Allowed',
  tool_name: 'ToolName',
  command: 'Command',
  result: 'Result',
  cond: 'Condition',
  case: 'Case',
  iteration: 'Iteration',
  name: 'Name',
  task: 'Task',
  description: 'Description',
  alias: 'Alias',
  max_iterations: 'MaxIterations',
}

/** Renamed node kinds: older canvases used a single `Arithmetic` entry where
 *  the daemon knows the real `Add` kind. */
const KIND_ALIASES: Record<string, string> = { Arithmetic: 'Add' }

/**
 * Renames a node's data pins to the preset labels for its kind (older
 * canvases stored lowercase/`out` names that no longer match the daemon's
 * executor lookups or the DSL templates). Whitespace is stripped so exported
 * data wires never break the DSL parser.
 */
function canonicalizePinNames(n: BlueprintNode): BlueprintNode {
  const kind = KIND_ALIASES[n.type] ?? n.type
  const preset = NODE_PRESETS[kind]
  const labelOf = new Map<string, string>()
  if (preset) {
    for (const d of preset.inputs) labelOf.set(d.id, d.label)
    for (const d of preset.outputs) labelOf.set(d.id, d.label)
  }
  const byName = (p: BlueprintPin): BlueprintPin => {
    const label = p.key ? (labelOf.get(p.key) ?? LEGACY_PIN_NAMES[p.key]) : undefined
    const name = (label ?? p.name ?? '').replace(/\s+/g, '')
    return name !== p.name ? { ...p, name } : p
  }
  return { ...n, type: n.type === kind ? n.type : kind, inputs: (n.inputs ?? []).map(byName), outputs: (n.outputs ?? []).map(byName) }
}

/**
 * Rewrites ids that are not UUIDs (older canvases used `n-…` node ids) so the
 * graph satisfies the daemon model, which keys nodes/edges/pins by `Uuid`, and
 * renames pins to the current preset labels. Idempotent; works for cached
 * graphs too, since name canonicalization never depends on id migration.
 */
function migrateIds(g: GraphData): GraphData {
  const nodeIds = new Map<string, string>()
  const pinIds = new Map<string, string>()
  for (const n of g.nodes) {
    if (!UUID_RE.test(n.id)) nodeIds.set(n.id, crypto.randomUUID())
    for (const p of [...(n.inputs ?? []), ...(n.outputs ?? [])]) {
      if (!UUID_RE.test(p.id)) pinIds.set(p.id, crypto.randomUUID())
    }
  }
  return {
    id: UUID_RE.test(g.id) ? g.id : crypto.randomUUID(),
    nodes: g.nodes.map((n) => {
      const remapped = {
        ...n,
        id: nodeIds.get(n.id) ?? n.id,
        inputs: (n.inputs ?? []).map((p) => ({ ...p, id: pinIds.get(p.id) ?? p.id })),
        outputs: (n.outputs ?? []).map((p) => ({ ...p, id: pinIds.get(p.id) ?? p.id })),
      }
      return canonicalizePinNames(remapped as BlueprintNode)
    }),
    edges: g.edges.map((e) => ({
      ...e,
      id: UUID_RE.test(e.id) ? e.id : crypto.randomUUID(),
      source: nodeIds.get(e.source) ?? e.source,
      target: nodeIds.get(e.target) ?? e.target,
      sourceHandle: e.sourceHandle ? (pinIds.get(e.sourceHandle) ?? e.sourceHandle) : e.sourceHandle,
      targetHandle: e.targetHandle ? (pinIds.get(e.targetHandle) ?? e.targetHandle) : e.targetHandle,
    })),
  }
}

export const useBlueprintStore = defineStore('blueprint', () => {
  const loaded = ref(false)
  const saved = ref(false)
  const nodes = ref<BlueprintNode[]>([])
  const edges = ref<BlueprintEdge[]>([])
  const nodeKinds = ref<string[]>([])
  /** Registered blueprint functions surfaced by the daemon library. */
  const functions = ref<FunctionItem[]>([])
  /** Unsaved graph per blueprint file path, so different files stay independent. */
  const graphs = ref<Record<string, GraphData>>({})
  /** Serialized graph as last written to disk per file path. The editor
   *  compares its live graph against this to decide the unsaved marker, so a
   *  cached-but-edited file stays dirty when you switch tabs and come back. */
  const savedKeys = ref<Record<string, string>>({})
  /** Stable blueprint UUID per file path, persisted into the file itself. */
  const ids = ref<Record<string, string>>({})
  const currentFile = ref('')

  /** A file-reference the explorer asked to drop into the current blueprint.
   *  The mounted editor consumes it via {@link consumeReference}; the salt makes
   *  repeated requests for the same file each trigger the editor's watcher. */
  const pendingRef = ref<{ path: string; token: number } | null>(null)
  let refSalt = 0

  function requestAddReference(path: string) {
    pendingRef.value = { path, token: ++refSalt }
  }

  /** Take the pending reference (clearing it) if the editor is mounted. */
  function consumeReference() {
    const p = pendingRef.value
    pendingRef.value = null
    return p
  }

  const byId = computed(() => new Map(nodes.value.map((n) => [n.id, n])))

  async function listKinds() {
    const r = await gateway.listNodeKinds()
    if (r.ok) nodeKinds.value = r.data
  }

  /** Load the function library (`path` empty = builtin + global). */
  async function loadFunctions(path = '') {
    const r = await gateway.listFunctions(path)
    if (r.ok) functions.value = r.data
  }

  /** The blueprint UUID of a file path, if it has been loaded. */
  function uuidFor(filePath: string): string | undefined {
    return ids.value[filePath]
  }

  async function load(path: string, filePath: string) {
    currentFile.value = filePath
    const cached = graphs.value[filePath]
    if (cached) {
      const graph = migrateIds(cached)
      graphs.value[filePath] = graph
      nodes.value = graph.nodes
      edges.value = graph.edges
      ids.value[filePath] = graph.id
    } else {
      const [file, _] = await Promise.all([gateway.readFile(path, filePath), listKinds()])
      let graph = seedGraph()
      if (file.ok && file.data.content.trim()) {
        try {
          const parsed = JSON.parse(file.data.content) as Blueprint
          if (Array.isArray(parsed.nodes) && Array.isArray(parsed.edges)) {
            graph = { id: parsed.id || crypto.randomUUID(), nodes: parsed.nodes, edges: parsed.edges }
          }
        } catch {
          // Corrupt files fall back to a fresh graph.
        }
      }
      graph = migrateIds(graph)
      graphs.value[filePath] = graph
      ids.value[filePath] = graph.id
      // The freshly loaded graph is, by definition, the on-disk baseline.
      savedKeys.value[filePath] = keyFor(graph.nodes, graph.edges)
      nodes.value = graph.nodes
      edges.value = graph.edges
    }
    loaded.value = true
    saved.value = true
  }

  /** Persist the currently open graph (the one keyed by `filePath`): write the
   *  file and mirror it into the daemon blueprint store for execution. The
   *  graph comes from the live `nodes`/`edges` refs, which the editor assigns
   *  before saving, so nothing falls back to a stale cached snapshot.
   *
   *  The on-disk file is the source of truth for the dirty baseline: it is
   *  updated as soon as the file write succeeds, even if the daemon mirror
   *  fails, so a successful save never leaves a phantom unsaved dot. */
  async function save(path: string, filePath: string): Promise<boolean> {
    const id = ids.value[filePath]
    if (!id) return false
    const graph: GraphData = {
      id,
      nodes: nodes.value,
      edges: edges.value,
    }
    graphs.value[filePath] = graph
    const blueprint: Blueprint = {
      id,
      name: fileNameOf(filePath),
      entryNodeId: nodes.value[0]?.id,
      nodes: nodes.value,
      edges: edges.value,
    }
    const file = await gateway.writeFile(path, filePath, JSON.stringify(blueprint, null, 2))
    if (file.ok) {
      saved.value = true
      savedKeys.value[filePath] = keyFor(nodes.value, edges.value)
    }
    try {
      await gateway.saveBlueprint(path, blueprint)
    } catch {
      // Daemon mirror failures are non-fatal for the editor.
    }
    return file.ok
  }

  function select(id: string | null) {
    for (const node of nodes.value) node.selected = node.id === id
  }

  function setNodePosition(id: string, position: { x: number; y: number }) {
    const node = byId.value.get(id)
    if (node) node.position = position
  }

  return {
    loaded,
    saved,
    nodes,
    edges,
    nodeKinds,
    functions,
    graphs,
    savedKeys,
    ids,
    byId,
    currentFile,
    pendingRef,
    requestAddReference,
    consumeReference,
    uuidFor,
    listKinds,
    loadFunctions,
    load,
    save,
    select,
    setNodePosition,
    keyFor,
  }
})