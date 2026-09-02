import { createClient, type Client } from '@connectrpc/connect'
import { createGrpcWebTransport } from '@connectrpc/connect-web'
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
  DaemonConfig,
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
  UsageSummary,
  WatchEvent,
  WorkspaceInfo,
} from './types'
import { err, ok } from './types'
import { Daemon } from '@/gen/metteur_pb'
import { configScopeOf, configToToml, isConfigDoc, tryParseToml } from '@/lib/toml'

/** Maps a thrown connect error into a readable failure result. */
function toErr(e: unknown): Result<never> {
  if (e instanceof Error) return err(e.message)
  return err(String(e))
}

/** Coerce a daemon node type string to the UI category. */
function categoryOf(kind: string): NodeCategory {
  switch (kind) {
    case 'Start':
    case 'End':
      return 'event'
    case 'Tool':
      return 'action'
    case 'Validator':
    case 'Judge':
    case 'Branch':
    case 'Add':
    case 'Subtract':
    case 'Multiply':
    case 'Divide':
      return 'flow'
    default:
      return 'module'
  }
}

/** Map a UI node type to the daemon kind (`Arithmetic` wraps `Add`). */
function daemonKindOf(kind: string): string {
  return kind === 'Arithmetic' ? 'Add' : kind
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
  return {
    id: bp.id,
    name: bp.name,
    entryNodeId: bp.entryNodeId ?? kept[0]?.id ?? '',
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
    pins: Array<{ id: string; name: string; pinType: string; dataType: string }>
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
      const type = p.dataType === 'Float' ? 'number' : p.dataType === 'Int' ? 'int' : p.dataType === 'Bool' ? 'bool' : p.dataType === 'Json' ? 'object' : 'string'
      const pin: BlueprintPin = { id: p.id, key: p.name || undefined, name: p.name, kind, type }
      if (kind === 'data-in') {
        const raw = data[p.name]
        if (raw !== undefined) values[p.id] = String(raw)
        inputs.push(pin)
      } else {
        outputs.push(pin)
      }
    }
    const category = categoryOf(n.kind)
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
  readonly connected = true
  private client: Client<typeof Daemon>

  constructor(baseUrl: string) {
    const transport = createGrpcWebTransport({ baseUrl })
    this.client = createClient(Daemon, transport)
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

  // ReAct chat --------------------------------------------------------------------
  async sendChat(
    workspacePath: string,
    content: string,
    history: ChatMessage[],
    onMessage: (m: ChatMessage) => void,
    options?: ChatOptions,
  ): Promise<Result<void>> {
    try {
      let seq = 0
      const historyJson = JSON.stringify(
        history
          .filter((m) => m.role === 'user' || m.role === 'assistant')
          .map((m) => ({ role: m.role, content: m.content })),
      )
      const optionsJson = JSON.stringify(options ?? {})
      for await (const ev of this.client.sendChat({
        workspacePath,
        message: content,
        historyJson,
        optionsJson,
      })) {
        if (ev.kind === 'assistant') {
          onMessage({
            id: `a-${Date.now()}-${seq++}`,
            role: 'assistant',
            content: ev.content,
            createdAt: Date.now(),
          })
        } else if (ev.kind === 'tool') {
          let toolName = ''
          if (ev.detailJson) {
            try {
              toolName = String(JSON.parse(ev.detailJson).name ?? '')
            } catch {
              toolName = ''
            }
          }
          onMessage({
            id: `t-${Date.now()}-${seq++}`,
            role: 'tool',
            actor: toolName,
            content: ev.content,
            createdAt: Date.now(),
          })
        } else if (ev.kind === 'error') {
          return err(ev.content)
        }
      }
      return ok(undefined)
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

  async decompileBlueprint(workspacePath: string, blueprintId: string): Promise<Result<string>> {
    try {
      const resp = await this.client.decompileBlueprint({ workspacePath, blueprintId })
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
  ): Promise<Result<void>> {
    try {
      for await (const ev of this.client.executeBlueprint({ workspacePath, blueprintId })) {
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

  async setAddonEnabled(id: string, enabled: boolean): Promise<Result<void>> {
    try {
      await this.client.setAddonEnabled({ id, workspacePath: '', enabled })
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
        })),
      })
    } catch (e) {
      return toErr(e)
    }
  }
}