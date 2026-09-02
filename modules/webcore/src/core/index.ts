import type { DaemonGateway } from './gateway'
import { GrpcGateway } from './grpc-gateway'
import { MockGateway } from './mock-gateway'

/**
 * The app-wide gateway instance.
 *
 * `VITE_MOCK=1` keeps the deterministic demo gateway; otherwise the real
 * grpc-web gateway talks to the Web Server Client (which proxies the daemon).
 */
const mock = import.meta.env.VITE_MOCK === '1'

export const gateway: DaemonGateway = mock
  ? new MockGateway()
  : new GrpcGateway(import.meta.env.VITE_GW_URL ?? '')

export type { DaemonGateway } from './gateway'
export type {
  AddonInfo,
  ApprovalRequest,
  Blueprint,
  BlueprintEdge,
  BlueprintNode,
  BlueprintPin,
  ChatMessage,
  ChatOptions,
  ContextRegion,
  DaemonConfig,
  ExecStatus,
  ExecutionEvent,
  ExecutionInfo,
  FileContent,
  FileHistoryEntry,
  FileInfo,
  FileTreeNode,
  FnPinInfo,
  FunctionItem,
  LlmModelConfig,
  McpServerInfo,
  ModelPricing,
  NodeAudit,
  NodeCategory,
  PinKind,
  Result,
  SnapshotInfo,
  UsageSummary,
  WatchEvent,
  WorkspaceInfo,
} from './types'
export { err, ok } from './types'