import type { BlueprintPin, FunctionItem, NodeCategory } from '@/core'
import type { Node as FlowNode } from '@vue-flow/core'

/**
 * Shared blueprint vocabulary: node presets used by both the palette and the
 * canvas. Kept in one place so the pins a preset declares match what a newly
 * added node actually renders.
 */

/**
 * Data-pin colours keyed by the pin's root type, mirroring Unreal's
 * type-coloured pins so you can tell a `string` from a `bool` at a glance.
 * Object pins get a neutral accent; unknown types fall back to it.
 */
export const DATA_COLORS: Record<string, string> = {
  string: '#ec6b7e',
  number: '#e2a13c',
  int: '#e2a13c',
  bool: '#b78be0',
  object: '#6aa7ec',
  json: '#6aa7ec',
  context: '#4ec9a4',
  choice: '#b78be0',
  any: '#8a8f98',
}

/** The root of a type expression: `list<int>` -> `list`, `object{a:int}` -> `object`. */
const ROOT_OF = (type?: string): string => (type ?? 'any').split(/[<{]/)[0]

/** Whether a value of `src` type may connect into a `dst` pin. */
export function isPinCompatible(src?: string, dst?: string): boolean {
  if (!src || !dst) return true
  const a = ROOT_OF(src)
  const b = ROOT_OF(dst)
  if (a === 'any' || b === 'any') return true
  if ((a === 'number' && b === 'int') || (a === 'int' && b === 'number')) return true
  return a === b
}

/** A category plus explicit labelled input/output pins for one node kind. */
export interface NodePreset {
  category: NodeCategory
  /** Input data pins in drawn order (exec-in is always prepended when present). */
  inputs: Array<{ id: string; label: string; type: string; choices?: string[] }>
  /** Output data pins in drawn order (exec-out is always prepended when present). */
  outputs: Array<{ id: string; label: string; type: string; choices?: string[] }>
  hasExecIn: boolean
  hasExecOut: boolean
  /** Extra exec output outlets (e.g. Branch's `true`/`false`), Unreal-style. */
  execOutputs?: string[]
}

/** Builds a standard node preset: exec in/out plus labelled data pins. */
function node(
  category: NodeCategory,
  inputs: Array<[string, string]>,
  outputs: Array<[string, string]> = [['out', 'string']],
  extra: Partial<NodePreset> = {},
): NodePreset {
  return {
    category,
    inputs: inputs.map(([id, type]) => ({ id, label: id, type })),
    outputs: outputs.map(([id, type]) => ({ id, label: id, type })),
    hasExecIn: true,
    hasExecOut: true,
    ...extra,
  }
}

/** Convenience: a binary math/comparison node with `A`/`B` -> `Result`.
 *  Pin names mirror the daemon executor lookups so DSL round-trips resolve. */
const bin = (category: NodeCategory, type: string, output = type): NodePreset =>
  node(category, [['A', type], ['B', type]], [['Result', output]])

/** Convenience: a unary node with `In` -> `Result`. */
const un = (category: NodeCategory, type: string, output = type): NodePreset =>
  node(category, [['In', type]], [['Result', output]])

/** Registry for the node kinds the daemon currently exposes. */
export const NODE_PRESETS: Record<string, NodePreset> = {
  Start: {
    category: 'event',
    inputs: [],
    outputs: [{ id: 'context', label: 'Context', type: 'context' }],
    hasExecIn: false,
    hasExecOut: true,
  },
  End: { category: 'event', inputs: [], outputs: [], hasExecIn: true, hasExecOut: false },
  CallLLM: {
    category: 'module',
    // Pin names must stay free of whitespace: the DSL parser splits wires on
    // spaces, so `Top P` would break exported data wires.
    inputs: [
      { id: 'context', label: 'Context', type: 'context' },
      {
        id: 'reasoning_effort',
        label: 'ReasoningEffort',
        type: 'choice',
        choices: ['none', 'low', 'medium', 'high'],
      },
      { id: 'model', label: 'Model', type: 'string' },
      { id: 'prompt', label: 'Prompt', type: 'string' },
      { id: 'system', label: 'System', type: 'string' },
      { id: 'temperature', label: 'Temperature', type: 'number' },
      { id: 'top_p', label: 'TopP', type: 'number' },
      { id: 'max_tokens', label: 'MaxTokens', type: 'number' },
      { id: 'max_iterations', label: 'MaxIterations', type: 'number' },
      { id: 'seed', label: 'Seed', type: 'number' },
    ],
    outputs: [
      { id: 'result', label: 'Result', type: 'string' },
      { id: 'context', label: 'Context', type: 'context' },
    ],
    hasExecIn: true,
    hasExecOut: true,
  },
  Tool: {
    category: 'action',
    inputs: [
      { id: 'tool_name', label: 'ToolName', type: 'string' },
      { id: 'command', label: 'Command', type: 'string' },
    ],
    outputs: [{ id: 'result', label: 'Result', type: 'string' }],
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
  Add: bin('flow', 'number'),
  Subtract: bin('flow', 'number'),
  Multiply: bin('flow', 'number'),
  Divide: bin('flow', 'number'),
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
  // Extended math.
  Modulo: bin('flow', 'number'),
  Power: bin('flow', 'number'),
  Min: bin('flow', 'number'),
  Max: bin('flow', 'number'),
  Abs: un('flow', 'number'),
  Round: un('flow', 'number'),
  // Comparison.
  Equal: bin('flow', 'any', 'bool'),
  NotEqual: bin('flow', 'any', 'bool'),
  Greater: bin('flow', 'number', 'bool'),
  Less: bin('flow', 'number', 'bool'),
  GreaterEqual: bin('flow', 'number', 'bool'),
  LessEqual: bin('flow', 'number', 'bool'),
  // Logic.
  And: bin('flow', 'bool', 'bool'),
  Or: bin('flow', 'bool', 'bool'),
  Xor: bin('flow', 'bool', 'bool'),
  Not: un('flow', 'bool', 'bool'),
  // Strings.
  Concat: bin('flow', 'string'),
  Length: un('flow', 'string', 'int'),
  Upper: un('flow', 'string'),
  Lower: un('flow', 'string'),
  Trim: un('flow', 'string'),
  Contains: bin('flow', 'string', 'bool'),
  Replace: node('flow', [['Input', 'string'], ['Find', 'string'], ['ReplaceWith', 'string']], [['Result', 'string']]),
  Substring: node('flow', [['In', 'string'], ['Start', 'int'], ['Length', 'int']], [['Result', 'string']]),
  // Conversion.
  ToString: un('flow', 'any'),
  ToInt: un('flow', 'any', 'int'),
  ToFloat: un('flow', 'any', 'number'),
  ToBool: un('flow', 'any', 'bool'),
  ToJson: un('flow', 'any', 'json'),
  ParseJson: un('flow', 'string', 'json'),
  // Collections.
  ListCreate: node('flow', [['ItemA', 'any'], ['ItemB', 'any'], ['ItemC', 'any']], [['Result', 'list<any>']]),
  ListAppend: node('flow', [['List', 'list<any>'], ['Item', 'any']], [['Result', 'list<any>']]),
  ListGet: node('flow', [['List', 'list<any>'], ['Index', 'int']], [['Result', 'any']]),
  ListLength: node('flow', [['List', 'list<any>']], [['Result', 'int']]),
  ListContains: node('flow', [['List', 'list<any>'], ['Item', 'any']], [['Result', 'bool']]),
  JsonGet: node('flow', [['Object', 'json'], ['Path', 'string']], [['Result', 'any']]),
  JsonSet: node('flow', [['Object', 'json'], ['Path', 'string'], ['Value', 'any']], [['Result', 'json']]),
  // Context manager (threads the conversation context through the graph).
  ContextCreate: node('module', [['System', 'string'], ['Prompt', 'string']], [['Result', 'context']]),
  ContextClone: un('module', 'context', 'context'),
  ContextMerge: {
    category: 'module',
    inputs: [
      { id: 'context', label: 'Context', type: 'context' },
      { id: 'text', label: 'Text', type: 'string' },
      { id: 'role', label: 'Role', type: 'choice', choices: ['user', 'assistant', 'tool'] },
    ],
    outputs: [{ id: 'result', label: 'Result', type: 'context' }],
    hasExecIn: true,
    hasExecOut: true,
  },
  ContextFilter: {
    category: 'module',
    inputs: [
      { id: 'context', label: 'Context', type: 'context' },
      { id: 'role', label: 'Role', type: 'choice', choices: ['user', 'assistant', 'tool', 'system'] },
    ],
    outputs: [{ id: 'result', label: 'Result', type: 'context' }],
    hasExecIn: true,
    hasExecOut: true,
  },
  ContextTrim: node('module', [['Context', 'context'], ['Keep', 'int']], [['Result', 'context']]),
  ContextToText: un('module', 'context', 'string'),
  // Flow support.
  Delay: node('flow', [['Ms', 'int']], []),
  RequestApproval: node('action', [['Message', 'string']], [['Allowed', 'bool']]),
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