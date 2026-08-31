import type { BlueprintPin, FunctionItem, NodeCategory } from '@/core'
import type { Node as FlowNode } from '@vue-flow/core'

/**
 * Shared blueprint vocabulary: node presets used by both the palette and the
 * canvas. Kept in one place so the pins a preset declares match what a newly
 * added node actually renders.
 */

/**
 * Data-pin colours keyed by pin type, mirroring Unreal's type-coloured pins so
 * you can tell a `string` from a `bool` at a glance. Object pins get a neutral
 * accent; unknown types fall back to it.
 */
export const DATA_COLORS: Record<string, string> = {
  string: '#ec6b7e',
  number: '#e2a13c',
  int: '#e2a13c',
  bool: '#b78be0',
  object: '#6aa7ec',
  any: '#6aa7ec',
}

/** A category plus explicit labelled input/output pins for one node kind. */
export interface NodePreset {
  category: NodeCategory
  /** Input data pins in drawn order (exec-in is always prepended when present). */
  inputs: Array<{ id: string; label: string; type: string; choices?: string[] }>
  /** Output data pins in drawn order (exec-out is always prepended when present). */
  outputs: Array<{ id: string; label: string; type: string }>
  hasExecIn: boolean
  hasExecOut: boolean
  /** Extra exec output outlets (e.g. Branch's `true`/`false`), Unreal-style. */
  execOutputs?: string[]
}

/** Registry for the node kinds the daemon currently exposes. */
export const NODE_PRESETS: Record<string, NodePreset> = {
  Start: { category: 'event', inputs: [], outputs: [], hasExecIn: false, hasExecOut: true },
  End: { category: 'event', inputs: [], outputs: [], hasExecIn: true, hasExecOut: false },
  CallLLM: {
    category: 'module',
    inputs: [
      {
        id: 'reasoning_effort',
        label: 'Reasoning Effort',
        type: 'choice',
        choices: ['none', 'low', 'medium', 'high'],
      },
      { id: 'model', label: 'Model', type: 'string' },
      { id: 'prompt', label: 'Prompt', type: 'string' },
      { id: 'system', label: 'System', type: 'string' },
      { id: 'temperature', label: 'Temperature', type: 'number' },
      { id: 'top_p', label: 'Top P', type: 'number' },
      { id: 'max_tokens', label: 'Max Tokens', type: 'number' },
      { id: 'max_iterations', label: 'Max Iterations', type: 'number' },
      { id: 'seed', label: 'Seed', type: 'number' },
    ],
    outputs: [
      { id: 'result', label: 'Result', type: 'string' },
      { id: 'context', label: 'Context', type: 'object' },
    ],
    hasExecIn: true,
    hasExecOut: true,
  },
  Tool: {
    category: 'action',
    inputs: [
      { id: 'tool_name', label: 'Tool Name', type: 'string' },
      { id: 'command', label: 'Command', type: 'string' },
    ],
    outputs: [{ id: 'out', label: 'Output', type: 'string' }],
    hasExecIn: true,
    hasExecOut: true,
  },
  Validator: {
    category: 'flow',
    inputs: [{ id: 'target', label: 'Target', type: 'string' }],
    outputs: [],
    hasExecIn: true,
    hasExecOut: true,
  },
  Judge: {
    category: 'flow',
    inputs: [{ id: 'result', label: 'Result', type: 'string' }],
    outputs: [],
    hasExecIn: true,
    hasExecOut: true,
  },
  Arithmetic: {
    category: 'flow',
    inputs: [
      { id: 'a', label: 'A', type: 'number' },
      { id: 'b', label: 'B', type: 'number' },
    ],
    outputs: [{ id: 'out', label: 'Out', type: 'number' }],
    hasExecIn: true,
    hasExecOut: true,
  },
  Branch: {
    category: 'flow',
    inputs: [{ id: 'cond', label: 'Condition', type: 'bool' }],
    outputs: [],
    hasExecIn: true,
    hasExecOut: false,
    execOutputs: ['true', 'false'],
  },
  FileReference: {
    category: 'module',
    inputs: [{ id: 'path', label: 'Path', type: 'string' }],
    outputs: [],
    hasExecIn: false,
    hasExecOut: false,
  },
}

/** Fallback preset applied to any kind not in the registry. */
const FALLBACK: NodePreset = {
  category: 'module',
  inputs: [],
  outputs: [],
  hasExecIn: true,
  hasExecOut: true,
}

/** Generate a fresh UUID usable as a node/pin/edge id. */
export const uuid = (): string => crypto.randomUUID()

/** Find a node's pin by its semantic key (e.g. `x-in`, `prompt`). */
export function pinByKey(pins: BlueprintPin[], key: string): BlueprintPin | undefined {
  return pins.find((p) => p.key === key)
}

/** The exec-input pin of a node, if any. */
export function execInOf(pins: BlueprintPin[]): BlueprintPin | undefined {
  return pins.find((p) => p.kind === 'exec-in')
}

/** The exec-output pin of a node, if any. */
export function execOutOf(pins: BlueprintPin[]): BlueprintPin | undefined {
  return pins.find((p) => p.kind === 'exec-out')
}

/** Build the labelled `inputs`/`outputs` pin lists for a preset in draw order.
 *  Pin ids are fresh UUIDs; each pin keeps its preset key for semantics. */
export function pinsFor(preset: NodePreset): { inputs: BlueprintPin[]; outputs: BlueprintPin[] } {
  const outputs: BlueprintPin[] = []
  if (preset.hasExecOut) outputs.push({ id: uuid(), key: 'x-out', name: '', kind: 'exec-out' })
  for (const ep of preset.execOutputs ?? []) outputs.push({ id: uuid(), key: `x-out-${ep}`, name: ep, kind: 'exec-out' })
  for (const d of preset.outputs) outputs.push({ id: uuid(), key: d.id, name: d.label, kind: 'data-out', type: d.type })

  const inputs: BlueprintPin[] = []
  if (preset.hasExecIn) inputs.push({ id: uuid(), key: 'x-in', name: '', kind: 'exec-in' })
  for (const d of preset.inputs) {
    inputs.push({
      id: uuid(),
      key: d.id,
      name: d.label,
      kind: 'data-in',
      type: d.type,
      choices: d.choices,
    })
  }

  return { inputs, outputs }
}

/** Build a flow node from a kind, labelled with the preset's pins. */
export function makeFlowNode(
  kind: string,
  position: { x: number; y: number },
  id: string,
): FlowNode {
  const preset = NODE_PRESETS[kind] ?? FALLBACK
  return {
    id,
    type: 'blueprint',
    position,
    data: { ...pinsFor(preset), title: kind, category: preset.category, values: {} },
  }
}

/** Build a `CallFunction` node whose pins mirror the function's signature.
 *  The function name rides on a `function` data pin so the daemon can resolve
 *  the body at execution time (the interpreter reads `data.function`). */
export function makeCallFunctionNode(
  entry: FunctionItem,
  position: { x: number; y: number },
  id: string,
): FlowNode {
  const inputs: BlueprintPin[] = [{ id: uuid(), key: 'x-in', name: '', kind: 'exec-in' }]
  const fn = { id: uuid(), key: 'function', name: 'Function', kind: 'data-in' as const, type: 'string' }
  inputs.push(fn)
  for (const p of entry.inputs) {
    inputs.push({ id: uuid(), key: p.name, name: p.name, kind: 'data-in', type: p.type })
  }
  const outputs: BlueprintPin[] = [{ id: uuid(), key: 'x-out', name: '', kind: 'exec-out' }]
  for (const p of entry.outputs) {
    outputs.push({ id: uuid(), key: p.name, name: p.name, kind: 'data-out', type: p.type })
  }
  return {
    id,
    type: 'blueprint',
    position,
    data: {
      inputs,
      outputs,
      title: entry.name,
      category: 'module',
      values: { [fn.id]: entry.name },
    },
  }
}

/** Category ordering + label used by the palette grouping. */
export const CATEGORIES: Array<{ key: NodeCategory; label: string }> = [
  { key: 'event', label: 'Events' },
  { key: 'module', label: 'Module' },
  { key: 'action', label: 'Actions' },
  { key: 'flow', label: 'Flow' },
]