import { createClient, type Client } from '@connectrpc/connect'
import { createGrpcWebTransport } from '@connectrpc/connect-web'
import { ref, type Ref } from 'vue'
import type { NodeCategory, PinKind } from './types'
import type { DaemonGateway } from './gateway'
import type {
  AddonInfo,
  Blueprint,
  BlueprintEdge,
  BlueprintNode,
  BlueprintPin,
  ChatMessage,
  ChatOptions,
  ChatSessionInfo,
  ChatContextStats,
  ChatSessionSnapshot,
  ChatUsage,
  DaemonConfig,
  ExecTreeData,
  ExecutionEvent,
  ExecutionInfo,
  FileContent,
  FileHistoryEntry,
  FileInfo,
  FileTreeNode,
  FunctionItem,
  McpServerInfo,
  Result,
  SnapshotInfo,
  TodoItem,
  UsageSummary,
  JobInfo,
  JobNotice,
  WatchEvent,
  WorkspaceInfo,
} from './types'
import { err, ok } from './types'
import { readSse } from './sse'
import { Daemon } from '@/gen/metteur_pb'
import { NODE_PRESETS } from '@/lib/blueprint'
import { configScopeOf, configToToml, isConfigDoc, tryParseToml } from '@/lib/toml'

/** Reads a context-stats payload, tolerating a partial or unexpected shape. */
function parseContextStats(raw: unknown): ChatContextStats {
  const stats = (raw ?? {}) as Record<string, unknown>
  return {
    tokens: stats.tokens === null || stats.tokens === undefined ? null : Number(stats.tokens) || 0,
    limit: stats.limit === null || stats.limit === undefined ? null : Number(stats.limit),
    assumedLimit: stats.assumed_limit === true,
    regions: Array.isArray(stats.regions)
      ? (stats.regions as Array<{ region: string; tokens: number }>)
      : [],
  }
}

/**
 * Rebuilds the display transcript of a stored session.
 *
 * The daemon records what the conversation looked like — turns, tool calls with
 * their timing and outcome, notices, failures — which the model's context
 * cannot reproduce after compression or eviction.
 */
function parseTranscript(raw: string): ChatMessage[] {
  if (!raw) return []
  let entries: Array<Record<string, unknown>>
  try {
    entries = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(entries)) return []
  return entries.map((entry, index) => {
    const role = String(entry.role ?? '')
    const at = Number(entry.at) || Date.now()
    const content = String(entry.content ?? '')
    if (role === 'tool') {
      return {
        id: String(entry.call_id || `t-${index}`),
        role: 'tool' as const,
        actor: String(entry.tool ?? ''),
        content,
        detail: {
          callId: String(entry.call_id ?? ''),
          summary: String(entry.summary ?? ''),
          ok: entry.ok !== false,
          elapsedMs: Number(entry.elapsed_ms) || 0,
        },
        createdAt: at,
      }
    }
    if (role === 'notice') return { id: `n-${index}`, role: 'notice' as const, content, createdAt: at }
    if (role === 'error') return { id: `e-${index}`, role: 'error' as const, content, createdAt: at }
    if (role === 'assistant') {
      return {
        id: `a-${index}`,
        role: 'assistant' as const,
        content,
        reasoning: String(entry.reasoning ?? '') || undefined,
        createdAt: at,
      }
    }
    return { id: `u-${index}`, role: 'user' as const, content, createdAt: at }
  })
}

/** Parses a `detail_json` payload, tolerating an empty or malformed value. */
function parseDetail(raw: string | undefined): Record<string, unknown> {
  if (!raw) return {}
  try {
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, unknown>) : {}
  } catch {
    return {}
  }
}

/** Maps a thrown connect error into a readable failure result. */
function toErr(e: unknown): Result<never> {
  if (e instanceof Error) return err(e.message)
  return err(String(e))
}

/** Map a UI node type to the daemon kind (`Arithmetic` wraps `Add`). */
function daemonKindOf(kind: string): string {
  return kind === 'Arithmetic' ? 'Add' : kind
}

/** Narrows the daemon's job-state string to the UI union. */
function jobStateOf(raw: string): JobInfo['state'] {
  return raw === 'running' || raw === 'exited' || raw === 'failed' || raw === 'killed'
    ? raw
    : 'failed'
}

/** Narrows the daemon's job-notice kind to the UI union. */
function jobNoticeKindOf(raw: string): JobNotice['kind'] {
  return raw === 'started' || raw === 'output' || raw === 'finished' ? raw : 'output'
}

/** Map a UI category to the daemon node-type string. */
function nodeTypeOf(kind: string): string {
  switch (kind) {
    case 'Start':
    case 'End':
      return 'Event'
    case 'Validator':
    case 'Judge':
    case 'Branch':
      return 'Control'
    case 'Add':
    case 'Subtract':
    case 'Multiply':
    case 'Divide':
      return 'Pure'
    default:
      return 'Function'
  }
}

/** Map a pin kind to the daemon pin-type string. */
function pinTypeOf(kind: PinKind): string {
  switch (kind) {
    case 'exec-in':
      return 'ExecInput'
    case 'exec-out':
      return 'ExecOutput'
    case 'data-in':
      return 'DataInput'
    case 'data-out':
      return 'DataOutput'
  }
}

/** Map a UI pin value type to the daemon data-type string. */
function dataTypeOf(type?: string): string {
  switch (type) {
    case 'number':
      return 'Float'
    case 'int':
      return 'Int'
    case 'bool':
      return 'Bool'
    case 'list':
      return 'List'
    case 'object':
      return 'Json'
    default:
      return 'String'
  }
}

/** Map a daemon data-type string to the UI pin value type. */
function fnTypeOf(dataType: string): string {
  switch (dataType) {
    case 'Bool':
      return 'bool'
    case 'Int':
      return 'int'
    case 'List':
      return 'list'
    case 'Json':
      return 'object'
    case 'Float':
    case 'Int64':
      return 'number'
    default:
      return 'string'
  }
}

/** Coerce an inline editor value to the JSON type its pin declares. */
function coerceValue(type: string | undefined, raw: string): unknown {
  switch (type) {
    case 'number': {
      const n = Number(raw)
      return Number.isNaN(n) ? raw : n
    }
    case 'bool':
      return raw === 'true'
    default:
      return raw
  }
}

/** Serialize a UI node into the daemon `data` object from its pin values. */
function nodeData(node: BlueprintNode): Record<string, unknown> {
  const data: Record<string, unknown> = {}
  const values = node.values ?? {}
  for (const pin of node.inputs) {
    if (pin.kind !== 'data-in') continue
    const raw = values[pin.id]
    if (raw === undefined || raw === '') continue
    data[pin.key ?? pin.name] = coerceValue(pin.type, raw)
  }
  return data
}

/** Convert a domain blueprint into its protobuf form for the daemon. */
function toProtoBlueprint(bp: Blueprint): object {
  const kept = bp.nodes.filter((n) => n.type !== 'FileReference')
  const keptIds = new Set(kept.map((n) => n.id))
  // The daemon resolves the entry against the (FileReference-filtered) node
  // list, so fall back to the first kept node when the canvas entry was dropped.
  const entryNodeId =
    bp.entryNodeId && keptIds.has(bp.entryNodeId) ? bp.entryNodeId : kept[0]?.id ?? ''
  return {
    id: bp.id,
    name: bp.name,
    entryNodeId,
    nodes: kept.map((n) => {
      const kind = daemonKindOf(n.type)
      return {
        id: n.id,
        nodeType: nodeTypeOf(kind),
        kind,
        posX: n.position.x,
        posY: n.position.y,
        pins: [...n.inputs, ...n.outputs].map((p) => ({
          id: p.id,
          key: p.key ?? '',
          name: p.name || (p.kind === 'data-out' ? p.key ?? 'Result' : p.key ?? ''),
          pinType: pinTypeOf(p.kind),
          dataType: dataTypeOf(p.type),
        })),
        dataJson: JSON.stringify(nodeData(n)),
      }
    }),
    edges: bp.edges
      .filter((e) => keptIds.has(e.source) && keptIds.has(e.target))
      .map((e) => ({
        id: e.id,
        sourceNode: e.source,
        sourcePin: e.sourceHandle ?? '',
        targetNode: e.target,
        targetPin: e.targetHandle ?? '',
      })),
  }
}

/** Map a daemon `DataType` display string to the canvas pin type. Daemon
 *  serializes types lowercase (`float`, `list<int>`…); scalar aliases collapse
 *  onto the canvas vocabulary while structural types pass through verbatim. */
function protoTypeOf(dt: string): string {
  switch (dt) {
    case 'float':
      return 'number'
    case 'json':
      return 'json'
    case 'context':
      return 'context'
    case 'any':
      return 'any'
    default:
      return dt // int / bool / string / list<…> / object{…} pass through
  }
}

/** Convert a protobuf blueprint into its domain form. */
function fromProtoBlueprint(pb: {
  id: string
  name: string
  entryNodeId: string
  nodes: Array<{
    id: string
    nodeType: string
    kind: string
    posX: number
    posY: number
    pins: Array<{
      id: string
      key: string
      name: string
      pinType: string
      dataType: string
      choices?: string[]
    }>
    dataJson: string
  }>
  edges: Array<{
    id: string
    sourceNode: string
    sourcePin: string
    targetNode: string
    targetPin: string
  }>
}): Blueprint {
  const nodes: BlueprintNode[] = pb.nodes.map((n) => {
    const inputs: BlueprintPin[] = []
    const outputs: BlueprintPin[] = []
    let data: Record<string, unknown> = {}
    try {
      data = JSON.parse(n.dataJson || '{}')
    } catch {
      data = {}
    }
    const values: Record<string, string> = {}
    for (const p of n.pins) {
      const kind: PinKind =
        p.pinType === 'ExecInput'
          ? 'exec-in'
          : p.pinType === 'ExecOutput'
            ? 'exec-out'
            : p.pinType === 'DataInput'
              ? 'data-in'
              : 'data-out'
      // Default exec outlets carry no on-canvas label (matching hand-drawn
      // nodes); the key keeps the semantic name for DSL round-trips.
      const exec = kind === 'exec-in' || kind === 'exec-out'
      const name = exec && (p.name === 'x-in' || p.name === 'x-out') ? '' : p.name
      // Enum candidates are registry metadata: when the wire omits them (e.g.
      // a DSL-compiled `choice` pin), the preset fills them in by key/label.
      const presetPin = NODE_PRESETS[n.kind]
        ? [...NODE_PRESETS[n.kind].inputs, ...NODE_PRESETS[n.kind].outputs].find(
            (d) => (p.key && d.id === p.key) || (p.name && d.label === p.name),
          )
        : undefined
      const pin: BlueprintPin = {
        id: p.id,
        key: p.key || p.name || undefined,
        name,
        kind,
        type: p.choices?.length || presetPin?.choices?.length ? 'choice' : protoTypeOf(p.dataType),
        choices: p.choices?.length ? p.choices : presetPin?.choices,
      }
      if (kind === 'data-in') {
        const raw = data[p.key || p.name]
        if (raw !== undefined) values[p.id] = String(raw)
        inputs.push(pin)
      } else if (kind === 'exec-in') {
        inputs.push(pin)
      } else {
        outputs.push(pin)
      }
    }
    // Category, colour and signature are registry metadata: the blueprint only
    // carries the kind, so import derives them from the preset table by kind.
    const category: NodeCategory = NODE_PRESETS[n.kind]?.category ?? 'module'
    return {
      id: n.id,
      type: n.kind,
      category,
      title: n.kind,
      position: { x: n.posX, y: n.posY },
      inputs,
      outputs,
      values: Object.keys(values).length ? values : undefined,
    }
  })
  const edges: BlueprintEdge[] = pb.edges.map((e) => ({
    id: e.id,
    source: e.sourceNode,
    sourceHandle: e.sourcePin || undefined,
    target: e.targetNode,
    targetHandle: e.targetPin || undefined,
  }))
  return { id: pb.id, name: pb.name, nodes, edges, entryNodeId: pb.entryNodeId || nodes[0]?.id }
}

/** Map a protobuf execution event into the domain shape. */
function fromProtoEvent(ev: {
  nodeId: string
  kind: string
  message: string
  detailJson: string
}): ExecutionEvent {
  let detail: Record<string, unknown> | undefined
  if (ev.detailJson) {
    try {
      detail = JSON.parse(ev.detailJson)
    } catch {
      detail = undefined
    }
  }
  return { nodeId: ev.nodeId, kind: ev.kind as ExecutionEvent['kind'], message: ev.message, detail }
}

/**
 * A data-access gateway backed by the real daemon over grpc-web.
 *
 * Talks to the Web Server Client (`metteur-web`), which proxies every call to
 * the daemon. Message and domain types are mapped at this boundary so views
 * never depend on generated code.
 */
export class GrpcGateway implements DaemonGateway {
  /** Live connection state, driven by a heartbeat ping; reactive so the UI
   *  flips to offline when the daemon (or the proxy) goes away. */
  readonly connected: Ref<boolean> = ref(true)
  readonly demo = false
  private client: Client<typeof Daemon>
  /** Same origin as the grpc-web transport, used by the SSE chat endpoint. */
  private baseUrl: string

  constructor(baseUrl: string) {
    const transport = createGrpcWebTransport({ baseUrl })
    this.client = createClient(Daemon, transport)
    this.baseUrl = baseUrl
    // Probe the daemon periodically; a network failure marks the app offline.
    setInterval(() => void this.ping(), 5000)
    void this.ping()
  }

  private async ping(): Promise<void> {
    try {
      await this.client.listWorkspaces({})
      this.connected.value = true
    } catch {
      this.connected.value = false
    }
  }

  // Workspaces ----------------------------------------------------------------
  async connect(): Promise<Result<void>> {
    return ok(undefined)
  }

  async openWorkspace(path: string): Promise<Result<WorkspaceInfo>> {
    try {
      const ws = await this.client.openWorkspace({ path })
      return ok({ path: ws.path, locked: ws.locked })
    } catch (e) {
      return toErr(e)
    }
  }

  async closeWorkspace(path: string): Promise<Result<void>> {
    try {
      await this.client.closeWorkspace({ path })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async listWorkspaces(): Promise<Result<WorkspaceInfo[]>> {
    try {
      const list = await this.client.listWorkspaces({})
      return ok(list.workspaces.map((w) => ({ path: w.path, locked: w.locked })))
    } catch (e) {
      return toErr(e)
    }
  }

  // Configuration ----------------------------------------------------------------
  async getConfig(workspacePath = ''): Promise<Result<DaemonConfig>> {
    try {
      const resp = await this.client.getConfig({ workspacePath })
      try {
        return ok(JSON.parse(resp.configJson) as DaemonConfig)
      } catch (e) {
        return err(`daemon returned invalid config: ${String(e)}`)
      }
    } catch (e) {
      return toErr(e)
    }
  }

  async setConfig(config: DaemonConfig, workspacePath = ''): Promise<Result<void>> {
    try {
      await this.client.setConfig({ workspacePath, configJson: JSON.stringify(config) })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  // File explorer ---------------------------------------------------------------
  async listFiles(workspacePath: string, dir: string): Promise<Result<FileTreeNode[]>> {
    try {
      // One level only: the explorer loads children lazily on expansion and
      // caches them, so large workspaces cost one RPC per opened directory
      // instead of a full recursive crawl.
      const list = await this.client.listFiles({ workspacePath, dir })
      const nodes: FileTreeNode[] = list.entries.map((e) => ({
        name: e.name,
        path: e.path,
        kind: e.isDir ? 'dir' : 'file',
      }))
      return ok(nodes)
    } catch (e) {
      return toErr(e)
    }
  }

  async readFile(workspacePath: string, filePath: string): Promise<Result<FileContent>> {
    if (isConfigDoc(filePath)) {
      const scopePath = configScopeOf(filePath) === 'user' ? '' : workspacePath
      const cfg = await this.getConfig(scopePath)
      return cfg.ok ? ok({ content: configToToml(cfg.data), language: 'toml' }) : cfg
    }
    try {
      const file = await this.client.readFile({ workspacePath, path: filePath })
      const language = filePath.endsWith('.blueprint')
        ? 'blueprint'
        : filePath.endsWith('.json')
          ? 'json'
          : 'text'
      return ok({ content: file.content, language })
    } catch (e) {
      return toErr(e)
    }
  }

  async writeFile(workspacePath: string, filePath: string, content: string): Promise<Result<void>> {
    if (isConfigDoc(filePath)) {
      const parsed = tryParseToml(content)
      if (!parsed) return err('Saved content is not valid TOML')
      const scopePath = configScopeOf(filePath) === 'user' ? '' : workspacePath
      return this.setConfig(parsed, scopePath)
    }
    try {
      await this.client.writeFile({ workspacePath, path: filePath, content })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async createDir(workspacePath: string, dirPath: string): Promise<Result<void>> {
    try {
      await this.client.createDir({ workspacePath, path: dirPath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async removeFile(workspacePath: string, filePath: string): Promise<Result<void>> {
    try {
      await this.client.removeFile({ workspacePath, path: filePath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async renameFile(workspacePath: string, from: string, to: string): Promise<Result<void>> {
    try {
      await this.client.renameFile({ workspacePath, from, to })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async statFile(workspacePath: string, path: string): Promise<Result<FileInfo>> {
    try {
      const info = await this.client.statFile({ workspacePath, path })
      return ok({ path: info.path, isDir: info.isDir, len: Number(info.len) })
    } catch (e) {
      return toErr(e)
    }
  }

  async revealInExplorer(workspacePath: string, path: string): Promise<Result<void>> {
    try {
      await this.client.revealInExplorer({ workspacePath, path })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async watchWorkspace(
    workspacePath: string,
    onEvent: (e: WatchEvent) => void,
    signal?: AbortSignal,
  ): Promise<Result<void>> {
    try {
      for await (const ev of this.client.watchWorkspace({ workspacePath }, { signal })) {
        if (ev.kind === 'created' || ev.kind === 'modified' || ev.kind === 'removed') {
          onEvent({ path: ev.path, kind: ev.kind })
        }
      }
      return ok(undefined)
    } catch (e) {
      // Aborting the subscription is not a failure.
      if (signal?.aborted) return ok(undefined)
      return toErr(e)
    }
  }

  // Background commands (jobs) ---------------------------------------------------
  async listJobs(workspacePath: string): Promise<Result<JobInfo[]>> {
    try {
      const res = await this.client.listJobs({ workspacePath })
      return ok(
        res.jobs.map((job) => ({
          id: job.id,
          command: job.command,
          cwd: job.cwd,
          state: jobStateOf(job.state),
          exitCode: job.exitCode,
          runId: job.runId,
          startedAt: Number(job.startedAt),
          finishedAt: Number(job.finishedAt),
          outputBytes: Number(job.outputBytes),
          tail: job.tail,
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  async watchJobs(
    workspacePath: string,
    onEvent: (e: JobNotice) => void,
    signal?: AbortSignal,
  ): Promise<Result<void>> {
    try {
      for await (const event of this.client.watchJobs({ workspacePath }, { signal })) {
        onEvent({
          jobId: event.jobId,
          kind: jobNoticeKindOf(event.kind),
          chunk: event.chunk,
          state: jobStateOf(event.state),
          exitCode: event.exitCode,
          summary: event.summary,
        })
      }
      return ok(undefined)
    } catch (e) {
      // Aborting the subscription is not a failure.
      if (signal?.aborted) return ok(undefined)
      return toErr(e)
    }
  }

  async killJob(
    workspacePath: string,
    jobId: string,
  ): Promise<Result<{ killed: boolean; state: string }>> {
    try {
      const res = await this.client.killJob({ workspacePath, jobId })
      return ok({ killed: res.killed, state: res.state })
    } catch (e) {
      return toErr(e)
    }
  }

  async getFileAtSnapshot(
    workspacePath: string,
    path: string,
    snapshotId?: string,
  ): Promise<Result<{ found: boolean; content: string; snapshotId: string }>> {
    try {
      const res = await this.client.getFileAtSnapshot({
        workspacePath,
        path,
        snapshotId: snapshotId ?? '',
      })
      return ok({ found: res.found, content: res.content, snapshotId: res.snapshotId })
    } catch (e) {
      return toErr(e)
    }
  }

  // ReAct chat --------------------------------------------------------------------
  /**
   * Streams one chat turn over server-sent events.
   *
   * The daemon exposes the same events as a gRPC stream, but SSE is what the
   * browser can follow incrementally over plain HTTP and what an operator can
   * watch with `curl -N`. The callback contract is unchanged, so views do not
   * care which transport is in use.
   */
  async sendChat(
    workspacePath: string,
    content: string,
    history: ChatMessage[],
    onMessage: (m: ChatMessage) => void,
    options?: ChatOptions,
    onSession?: (sessionId: string) => void,
    sessionId?: string,
    onUsage?: (usage: ChatUsage) => void,
    onTodos?: (todos: TodoItem[]) => void,
    signal?: AbortSignal,
    onProgress?: (progress: { name: string; bytes: number }) => void,
    onContext?: (stats: ChatContextStats) => void,
    /** Asks the client to decide on a sandbox approval, mid-turn. */
    onApproval?: (request: { requestId: string; detail: string }) => void,
  ): Promise<Result<void>> {
    let seq = 0
    let turnId: string | null = null
    const historyJson = JSON.stringify(
      history
        .filter((m) => m.role === 'user' || m.role === 'assistant')
        .map((m) => ({ role: m.role, content: m.content })),
    )
    const turn = () => (turnId ??= `a-${Date.now()}-${seq++}`)
    let response: Response
    try {
      response = await fetch(`${this.baseUrl}/api/chat/stream`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          workspace_path: workspacePath,
          message: content,
          history_json: historyJson,
          options_json: JSON.stringify(options ?? {}),
          session_id: sessionId ?? '',
        }),
        signal,
      })
    } catch (e) {
      return toErr(e)
    }
    if (!response.ok) {
      // A failure before the stream starts is a real HTTP error: it says the
      // request never reached the daemon, which is a different problem from a
      // stream that broke halfway.
      return err((await response.text().catch(() => '')) || `chat request failed (${response.status})`)
    }

    try {
      for await (const message of readSse(response, signal)) {
        let event: { kind?: string; content?: string; detail_json?: string }
        try {
          event = JSON.parse(message.data)
        } catch {
          continue
        }
        const detail = parseDetail(event.detail_json)
        switch (event.kind) {
          case 'session': {
            const reported = String(detail.session_id ?? '')
            if (reported) onSession?.(reported)
          // The session event carries the context it starts from, so the meter
          // has a size immediately (and after switching conversations).
          if (detail.context) onContext?.(parseContextStats(detail.context))
            break
          }
          case 'assistant_delta':
            onMessage({
              id: turn(),
              role: 'assistant',
              content: event.content ?? '',
              createdAt: Date.now(),
              pending: true,
            })
            break
          case 'reasoning_delta':
            onMessage({
              id: turn(),
              role: 'assistant',
              content: '',
              reasoning: event.content ?? '',
              reasoningPending: true,
              createdAt: Date.now(),
              pending: true,
            })
            break
          case 'assistant':
            onMessage({
              id: turn(),
              role: 'assistant',
              content: event.content ?? '',
              // The final reasoning settles the collapsible block; without one
              // the streamed thinking (if any) stays.
              reasoning: String(detail.reasoning ?? '') || undefined,
              reasoningPending: false,
              createdAt: Date.now(),
            })
            break
          case 'tool_start':
            onMessage({
              id: `t-${String(detail.call_id ?? seq++)}`,
              role: 'tool',
              actor: String(detail.name ?? ''),
              content: '',
              detail: { callId: detail.call_id, summary: event.content ?? '', running: true },
              createdAt: Date.now(),
              pending: true,
            })
            break
          case 'tool_progress':
            onMessage({
              id: `t-${String(detail.call_id ?? '')}`,
              role: 'tool',
              content: event.content ?? '',
              detail: { callId: detail.call_id, progress: true },
              createdAt: Date.now(),
              pending: true,
            })
            break
          case 'tool':
            onMessage({
              id: `t-${String(detail.call_id ?? seq++)}`,
              role: 'tool',
              actor: String(detail.name ?? ''),
              content: event.content ?? '',
              detail: {
                callId: detail.call_id,
                ok: detail.ok !== false,
                elapsedMs: Number(detail.elapsed_ms) || 0,
              },
              createdAt: Date.now(),
            })
            break
          case 'tool_args':
            // The model is composing a call's arguments; the row appears once
            // they are complete, so report progress as a status instead.
            onProgress?.({
              name: String(detail.name ?? ''),
              bytes: Number(detail.bytes) || 0,
            })
            break
          case 'todos':
            if (Array.isArray(detail.todos)) onTodos?.(detail.todos as TodoItem[])
            break
          case 'approval':
            // A turn that needs a decision must be able to ask for one; the
            // dialog is shared with blueprint runs.
            onApproval?.({
              requestId: String(detail.request_id ?? ''),
              detail: event.content ?? '',
            })
            break
          case 'notice':
            onMessage({
              id: `n-${Date.now()}-${seq++}`,
              role: 'notice',
              content: event.content ?? '',
              createdAt: Date.now(),
            })
            break
          case 'done': {
            if (detail.context) onContext?.(parseContextStats(detail.context))
            const usage = (detail.usage ?? {}) as Record<string, unknown>
            onUsage?.({
              inputTokens: Number(usage.input_tokens) || 0,
              outputTokens: Number(usage.output_tokens) || 0,
              totalTokens: Number(usage.total_tokens) || 0,
              cachedInputTokens: Number(usage.cached_input_tokens) || 0,
              cacheWriteInputTokens: Number(usage.cache_write_input_tokens) || 0,
            })
            break
          }
          case 'error':
            return err(event.content || 'The turn failed.')
          default:
            break
        }
      }
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async sendInterrupt(
    workspacePath: string,
    message: string,
    priority: 'Normal' | 'Urgent' | 'Emergency' = 'Normal',
  ): Promise<Result<void>> {
    try {
      await this.client.sendInterrupt({ workspacePath, message, priority })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async pickDirectory(): Promise<Result<string | null>> {
    try {
      const res = await fetch('/api/pick-directory', { method: 'POST' })
      if (!res.ok) return err(res.statusText || `folder picker failed (${res.status})`)
      const data = (await res.json()) as { path?: string | null }
      return ok(data.path ?? null)
    } catch (e) {
      return toErr(e)
    }
  }

  async abortChat(workspacePath: string): Promise<Result<void>> {
    try {
      await this.client.abortChat({ workspacePath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async listChatSessions(workspacePath: string): Promise<Result<ChatSessionInfo[]>> {
    try {
      const res = await this.client.listChatSessions({ workspacePath })
      return ok(
        res.sessions.map((s) => ({
          sessionId: s.sessionId,
          createdAt: Number(s.createdAt),
          updatedAt: Number(s.updatedAt),
          turns: Number(s.turns),
          title: s.title,
          messageCount: Number(s.messageCount),
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  async getChatSession(workspacePath: string, sessionId?: string): Promise<Result<ChatSessionSnapshot>> {
    try {
      const res = await this.client.getChatSession({ workspacePath, sessionId: sessionId ?? '' })
      let history: ChatMessage[] = []
      try {
        const entries: Array<{ role: string; content: string }> = JSON.parse(res.historyJson)
        history = entries.map((e, i) => ({
          id: `${e.role === 'user' ? 'u' : 'a'}-${i}`,
          role: e.role === 'user' ? 'user' : 'assistant',
          content: e.content,
          createdAt: Number(res.createdAt),
        }))
      } catch {
        history = []
      }
      let todos: TodoItem[] = []
      try {
        todos = JSON.parse(res.todosJson || '[]')
      } catch {
        todos = []
      }
      return ok({
        sessionId: res.sessionId,
        createdAt: Number(res.createdAt),
        history,
        transcript: parseTranscript(res.transcriptJson),
        todos,
      })
    } catch (e) {
      return toErr(e)
    }
  }

  async deleteChatSession(workspacePath: string, sessionId?: string): Promise<Result<void>> {
    try {
      await this.client.deleteChatSession({ workspacePath, sessionId: sessionId ?? '' })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  // Blueprints -----------------------------------------------------------------
  async listNodeKinds(): Promise<Result<string[]>> {
    try {
      const kinds = await this.client.listNodeKinds({})
      return ok(kinds.kinds)
    } catch (e) {
      return toErr(e)
    }
  }

  async listFunctions(workspacePath: string): Promise<Result<FunctionItem[]>> {
    try {
      const list = await this.client.listFunctions({ workspacePath })
      const items: FunctionItem[] = list.functions.map((f) => ({
        id: f.id,
        name: f.name,
        description: f.description,
        source: f.source,
        inputs: (f.inputs ?? []).map((p) => ({ name: p.name, type: fnTypeOf(p.dataType) })),
        outputs: (f.outputs ?? []).map((p) => ({ name: p.name, type: fnTypeOf(p.dataType) })),
      }))
      return ok(items)
    } catch (e) {
      return toErr(e)
    }
  }

  async compileDsl(source: string): Promise<Result<Blueprint>> {
    try {
      const bp = await this.client.compileDsl({ source })
      return ok(fromProtoBlueprint(bp))
    } catch (e) {
      return toErr(e)
    }
  }

  async decompileBlueprint(
    workspacePath: string,
    blueprintOrId: Blueprint | string,
  ): Promise<Result<string>> {
    try {
      const resp =
        typeof blueprintOrId === 'string'
          ? await this.client.decompileBlueprint({ workspacePath, blueprintId: blueprintOrId })
          : await this.client.decompileBlueprint({
              workspacePath,
              blueprintId: '',
              blueprint: toProtoBlueprint(blueprintOrId),
            })
      return ok(resp.source)
    } catch (e) {
      return toErr(e)
    }
  }

  async saveBlueprint(workspacePath: string, blueprint: Blueprint): Promise<Result<void>> {
    try {
      await this.client.saveBlueprint({ workspacePath, blueprint: toProtoBlueprint(blueprint) })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async loadBlueprint(workspacePath: string, blueprintId: string): Promise<Result<Blueprint>> {
    try {
      const bp = await this.client.loadBlueprint({ workspacePath, blueprintId })
      return ok(fromProtoBlueprint(bp))
    } catch (e) {
      return toErr(e)
    }
  }

  // Execution ----------------------------------------------------------------
  async executeBlueprint(
    workspacePath: string,
    blueprintId: string,
    onEvent: (e: ExecutionEvent) => void,
    blueprint?: Blueprint,
  ): Promise<Result<void>> {
    try {
      // Sending the canvas makes Run independent of the stored mirror, which
      // may lag behind (or have failed to update) after an edit.
      const blueprintJson = blueprint ? JSON.stringify(toProtoBlueprint(blueprint)) : ''
      for await (const ev of this.client.executeBlueprint({
        workspacePath,
        blueprintId,
        blueprintJson,
      })) {
        onEvent(fromProtoEvent(ev))
      }
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async continueExecution(
    workspacePath: string,
    runId: string,
    onEvent: (e: ExecutionEvent) => void,
  ): Promise<Result<void>> {
    try {
      for await (const ev of this.client.continueExecution({ workspacePath, runId })) {
        onEvent(fromProtoEvent(ev))
      }
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async listExecutions(workspacePath: string): Promise<Result<ExecutionInfo[]>> {
    try {
      const list = await this.client.listExecutions({ workspacePath })
      return ok(
        list.executions.map((x) => ({
          runId: x.runId,
          blueprintId: x.blueprintId,
          status: x.status,
          startedAt: Number(x.startedAt),
          updatedAt: Number(x.updatedAt),
          executedNodes: x.executedNodes,
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  async getExecutionTree(workspacePath: string, runId: string): Promise<Result<ExecTreeData>> {
    try {
      const res = await this.client.getExecutionTree({ workspacePath, runId })
      return ok({
        nodes: res.nodes.map((n) => ({
          id: n.id,
          kind: n.kind,
          label: n.label,
          parent: n.parent,
          children: [...n.children],
          status: n.status,
          tokens: Number(n.tokens),
          startedAt: Number(n.startedAt),
          finishedAt: Number(n.finishedAt),
        })),
        roots: [...res.roots],
      })
    } catch (e) {
      return toErr(e)
    }
  }

  async cancel(workspacePath: string): Promise<Result<void>> {
    try {
      await this.client.cancelExecution({ workspacePath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async pause(workspacePath: string): Promise<Result<void>> {
    try {
      await this.client.pauseExecution({ workspacePath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async resume(workspacePath: string): Promise<Result<void>> {
    try {
      await this.client.resumeExecution({ workspacePath })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  // Approvals -----------------------------------------------------------------
  async respondApproval(workspacePath: string, requestId: string, allow: boolean): Promise<Result<void>> {
    try {
      await this.client.respondApproval({ workspacePath, requestId, decision: allow ? 'AllowOnce' : 'DenyOnce' })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  // Versioning ------------------------------------------------------------------
  async listSnapshots(workspacePath: string): Promise<Result<SnapshotInfo[]>> {
    try {
      const list = await this.client.listSnapshots({ workspacePath })
      return ok(
        list.snapshots.map((s) => ({
          id: s.id,
          alias: s.alias || undefined,
          createdAt: Number(s.createdAt),
          message: s.description,
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  async createSnapshot(
    workspacePath: string,
    description: string,
    alias?: string,
  ): Promise<Result<SnapshotInfo>> {
    try {
      const s = await this.client.createSnapshot({ workspacePath, description, alias: alias ?? '' })
      return ok({
        id: s.id,
        alias: s.alias || undefined,
        createdAt: Number(s.createdAt),
        message: s.description,
      })
    } catch (e) {
      return toErr(e)
    }
  }

  async rollback(workspacePath: string, snapshotId: string, alias?: string): Promise<Result<void>> {
    try {
      await this.client.rollback({ workspacePath, snapshotId, alias: alias ?? '' })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async listFileHistory(workspacePath: string, path: string): Promise<Result<FileHistoryEntry[]>> {
    try {
      const list = await this.client.getFileHistory({ workspacePath, path })
      return ok(
        list.entries.map((e) => ({
          path,
          snapshotId: e.snapshotId,
          op: e.status === 'Added' ? 'add' : e.status === 'Deleted' ? 'delete' : 'modify',
          at: Number(e.createdAt),
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  // Addons / usage / resources ----------------------------------------------------
  async listAddons(): Promise<Result<AddonInfo[]>> {
    try {
      const list = await this.client.listAddons({})
      return ok(
        list.addons.map((a) => ({
          id: a.id,
          name: a.name,
          version: a.version,
          description: a.description,
          enabled: a.enabled,
          scope: a.scope,
          toolCount: a.toolCount,
          fragmentCount: a.fragmentCount,
        })),
      )
    } catch (e) {
      return toErr(e)
    }
  }

  async setAddonEnabled(
    id: string,
    enabled: boolean,
    workspacePath = '',
  ): Promise<Result<void>> {
    try {
      await this.client.setAddonEnabled({ id, workspacePath, enabled })
      return ok(undefined)
    } catch (e) {
      return toErr(e)
    }
  }

  async listMcpServers(): Promise<Result<McpServerInfo[]>> {
    try {
      const list = await this.client.listMcpServers({})
      return ok(list.servers.map((s) => ({ name: s.name, status: s.status, toolCount: s.toolCount, error: s.error })))
    } catch (e) {
      return toErr(e)
    }
  }

  async getExecutionUsage(workspacePath: string, runId: string): Promise<Result<UsageSummary>> {
    try {
      const u = await this.client.getExecutionUsage({ workspacePath, runId })
      return ok({
        currency: u.currency,
        totalCostMicros: Number(u.totalCostMicros),
        models: u.models.map((m) => ({
          model: m.model,
          calls: Number(m.calls),
          inputTokens: Number(m.inputTokens),
          outputTokens: Number(m.outputTokens),
          reasoningTokens: Number(m.reasoningTokens),
          costMicros: Number(m.costMicros),
          cachedInputTokens: Number(m.cachedInputTokens),
          cacheWriteInputTokens: Number(m.cacheWriteInputTokens),
        })),
      })
    } catch (e) {
      return toErr(e)
    }
  }
}