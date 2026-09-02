import type {
  AddonInfo,
  Blueprint,
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

/**
 * Data-access port for every UI surface.
 *
 * The app talks to the daemon through this single interface. `MockGateway`
 * implements it for development/demo; `GrpcGateway` backs it with Connect +
 * grpc-web through the Web Server Client.
 */
export interface DaemonGateway {
  /** Whether a live daemon connection is established. */
  readonly connected: boolean

  // Connection / workspace -----------------------------------------------------
  connect(): Promise<Result<void>>
  openWorkspace(path: string): Promise<Result<WorkspaceInfo>>
  closeWorkspace(path: string): Promise<Result<void>>
  listWorkspaces(): Promise<Result<WorkspaceInfo[]>>

  // Configuration --------------------------------------------------------------
  /**
   * Read one configuration layer: pass an empty string for the global layer,
   * or a workspace root path for that workspace's layer.
   */
  getConfig(workspacePath?: string): Promise<Result<DaemonConfig>>
  /** Persist a configuration layer ('' = global, otherwise workspace-root). */
  setConfig(config: DaemonConfig, workspacePath?: string): Promise<Result<void>>

  // File explorer / editors ---------------------------------------------------
  listFiles(workspacePath: string, dir: string): Promise<Result<FileTreeNode[]>>
  readFile(workspacePath: string, filePath: string): Promise<Result<FileContent>>
  writeFile(workspacePath: string, filePath: string, content: string): Promise<Result<void>>
  createDir(workspacePath: string, dirPath: string): Promise<Result<void>>
  removeFile(workspacePath: string, filePath: string): Promise<Result<void>>
  renameFile(workspacePath: string, from: string, to: string): Promise<Result<void>>
  statFile(workspacePath: string, path: string): Promise<Result<FileInfo>>
  /** Opens the system file manager with the entry selected (best effort). */
  revealInExplorer(workspacePath: string, path: string): Promise<Result<void>>
  /** Subscribes to live file changes; resolves when the stream ends or aborts. */
  watchWorkspace(
    workspacePath: string,
    onEvent: (e: WatchEvent) => void,
    signal?: AbortSignal,
  ): Promise<Result<void>>

  // ReAct chat -----------------------------------------------------------------
  sendChat(
    workspacePath: string,
    content: string,
    history: ChatMessage[],
    onMessage: (m: ChatMessage) => void,
    options?: ChatOptions,
  ): Promise<Result<void>>
  abortChat(workspacePath: string): Promise<Result<void>>

  // Blueprints -----------------------------------------------------------------
  listNodeKinds(): Promise<Result<string[]>>
  listFunctions(workspacePath: string): Promise<Result<FunctionItem[]>>
  compileDsl(source: string): Promise<Result<Blueprint>>
  decompileBlueprint(workspacePath: string, blueprintId: string): Promise<Result<string>>
  saveBlueprint(workspacePath: string, blueprint: Blueprint): Promise<Result<void>>
  loadBlueprint(workspacePath: string, blueprintId: string): Promise<Result<Blueprint>>

  // Execution ------------------------------------------------------------------
  executeBlueprint(
    workspacePath: string,
    blueprintId: string,
    onEvent: (e: ExecutionEvent) => void,
  ): Promise<Result<void>>
  continueExecution(
    workspacePath: string,
    runId: string,
    onEvent: (e: ExecutionEvent) => void,
  ): Promise<Result<void>>
  listExecutions(workspacePath: string): Promise<Result<ExecutionInfo[]>>
  cancel(workspacePath: string): Promise<Result<void>>
  pause(workspacePath: string): Promise<Result<void>>
  resume(workspacePath: string): Promise<Result<void>>

  // Approvals ------------------------------------------------------------------
  respondApproval(workspacePath: string, requestId: string, allow: boolean): Promise<Result<void>>

  // Versioning -----------------------------------------------------------------
  listSnapshots(workspacePath: string): Promise<Result<SnapshotInfo[]>>
  createSnapshot(
    workspacePath: string,
    description: string,
    alias?: string,
  ): Promise<Result<SnapshotInfo>>
  rollback(workspacePath: string, snapshotId: string, alias?: string): Promise<Result<void>>
  listFileHistory(workspacePath: string, path: string): Promise<Result<FileHistoryEntry[]>>

  // Addons / usage / resources --------------------------------------------------
  listAddons(): Promise<Result<AddonInfo[]>>
  setAddonEnabled(id: string, enabled: boolean): Promise<Result<void>>
  listMcpServers(): Promise<Result<McpServerInfo[]>>
  getExecutionUsage(workspacePath: string, runId: string): Promise<Result<UsageSummary>>
}