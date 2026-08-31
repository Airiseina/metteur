import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { gateway } from '@/core'
import type { Blueprint, BlueprintEdge, BlueprintNode, FunctionItem } from '@/core'

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

/** A fresh Start → End graph used to seed a brand-new blueprint file. */
function seedGraph(): GraphData {
  const nodeId = () => crypto.randomUUID()
  const pinId = () => crypto.randomUUID()
  const start = nodeId()
  const end = nodeId()
  const startOut = pinId()
  const endIn = pinId()
  return {
    id: crypto.randomUUID(),
    nodes: [
      {
        id: start,
        type: 'Start',
        category: 'event',
        title: 'Start',
        position: { x: 40, y: 220 },
        inputs: [],
        outputs: [{ id: startOut, key: 'x-out', name: '', kind: 'exec-out' }],
      },
      {
        id: end,
        type: 'End',
        category: 'event',
        title: 'End',
        position: { x: 320, y: 220 },
        inputs: [{ id: endIn, key: 'x-in', name: '', kind: 'exec-in' }],
        outputs: [],
      },
    ],
    edges: [
      { id: crypto.randomUUID(), source: start, sourceHandle: startOut, target: end, targetHandle: endIn },
    ],
  }
}

/** The on-disk name of a blueprint derived from its file path. */
function fileNameOf(filePath: string): string {
  return filePath.split(/[\\/]/).pop()?.replace(/\.blueprint$/i, '') || 'blueprint'
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
      nodes.value = cached.nodes
      edges.value = cached.edges
      ids.value[filePath] = cached.id
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