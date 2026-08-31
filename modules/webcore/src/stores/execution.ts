import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { gateway } from '@/core'
import type { ApprovalRequest, ContextRegion, ExecStatus, ExecutionEvent, NodeAudit } from '@/core'
import { useWorkspaceStore } from './workspace'

/** App-facing actions derived from an execution event. */
export interface EventLine {
  nodeId: string
  kind: ExecutionEvent['kind']
  message: string
  detail?: Record<string, unknown>
}

/**
 * Execution surface.
 *
 * Subscribes to the gateway stream, keeps the newest events plus the lifecycle
 * status, and surfaces a pending approval as a modal payload for the user to
 * allow/deny. The active run id is captured from `ListExecutions` after a run
 * starts so a suspended run can be resumed.
 */
export const useExecutionStore = defineStore('execution', () => {
  const workspace = useWorkspaceStore()
  const status = ref<ExecStatus>('idle')
  const events = ref<EventLine[]>([])
  const approval = ref<ApprovalRequest | null>(null)
  /** Most recent context region breakdown (from `context` events). */
  const contextUsage = ref<ContextRegion[] | null>(null)
  const contextNode = ref<string | null>(null)
  /** The id of the run belonging to the current execution, if known. */
  const runId = ref<string | null>(null)
  /** Live per-node audit facts, fed by started/finished/node_data/context. */
  const nodeAudits = ref<Map<string, NodeAudit>>(new Map())

  const running = computed(() => status.value === 'running' || status.value === 'paused')
  const lastEvent = computed(() => events.value[events.value.length - 1])
  /** Node whose `started` has no matching `finished` yet. */
  const runningNodeId = computed(() => {
    for (const [id, a] of nodeAudits.value) {
      if (a.startedAt && !a.finishedAt) return id
    }
    return null
  })
  const contextTotal = computed(() =>
    (contextUsage.value ?? []).reduce((sum, r) => sum + r.tokens, 0),
  )

  function ensureAudit(nodeId: string): NodeAudit {
    let a = nodeAudits.value.get(nodeId)
    if (!a) {
      a = { startedAt: 0, finishedAt: 0, outputs: {}, tokens: 0, message: '' }
      nodeAudits.value.set(nodeId, a)
    }
    return a
  }

  function push(ev: EventLine, limit = 500) {
    events.value.push(ev)
    const audit = ev.nodeId ? ensureAudit(ev.nodeId) : null
    if (ev.kind === 'started' && audit) audit.startedAt = Date.now()
    if (ev.kind === 'finished' && audit) audit.finishedAt = Date.now()
    if (ev.kind === 'node_data' && audit && ev.detail && typeof ev.detail === 'object') {
      audit.outputs = { ...audit.outputs, ...(ev.detail.outputs ?? {}) }
    }
    if (ev.kind === 'message' && audit) {
      const summary = ev.detail?.summary
      audit.message = typeof summary === 'string' ? summary : ev.message
    }
    if (ev.kind === 'context' && Array.isArray(ev.detail?.regions)) {
      contextUsage.value = (ev.detail.regions as unknown as ContextRegion[]).map((r) => ({
        region: r.region,
        chars: Number(r.chars) || 0,
        tokens: Number(r.tokens) || 0,
      }))
      contextNode.value = ev.nodeId
      if (audit) {
        audit.tokens = (ev.detail.regions as unknown as ContextRegion[]).reduce(
          (sum, r) => sum + (Number(r.tokens) || 0),
          0,
        )
      }
    }
    if (events.value.length > limit) events.value.splice(0, events.value.length - limit)
  }

  async function run(blueprintId: string) {
    const ws = workspace.active
    if (!ws) return
    events.value = []
    contextUsage.value = null
    contextNode.value = null
    nodeAudits.value = new Map()
    status.value = 'running'
    runId.value = null
    await gateway.executeBlueprint(ws.path, blueprintId, (ev) => {
      push(ev)
      if (ev.kind === 'approval_request') {
        const detail = (ev.detail ?? {}) as Record<string, unknown>
        const requestType = String(detail.request_type ?? 'sandbox')
        const title =
          requestType === 'circuit_tripped'
            ? 'Circuit breaker: allow auto-replan?'
            : requestType === 'replan_proposal'
              ? 'Approve revised plan'
              : 'Approve shell command'
        approval.value = {
          id: String(detail.requestId ?? detail.approvalId ?? ev.message ?? ''),
          title,
          tool: String(detail.tool ?? ''),
          command: ev.message,
          detail: detail.command ? String(detail.command) : ev.message,
          requestType,
        }
      }
    })
    // Capture the run id of the stream just finished for a later resume.
    const runs = await gateway.listExecutions(ws.path)
    if (runs.ok) {
      const active = runs.data.find((r) => r.status === 'Running') ?? runs.data[0]
      if (active) runId.value = active.runId
    }
    if (!['paused', 'cancelled'].includes(status.value)) status.value = 'finished'
  }

  async function respond(allow: boolean) {
    const ws = workspace.active
    if (!approval.value) return
    if (!ws) return
    await gateway.respondApproval(ws.path, approval.value.id, allow)
    push({ nodeId: 'approval', kind: 'message', message: allow ? 'approved' : 'denied' })
    approval.value = null
  }

  async function pause() {
    const ws = workspace.active
    if (!ws) return
    await gateway.pause(ws.path)
    status.value = 'paused'
  }

  async function resume() {
    const ws = workspace.active
    if (!ws || !runId.value) return
    status.value = 'running'
    await gateway.continueExecution(ws.path, runId.value, (ev) => push(ev))
  }

  async function cancel() {
    const ws = workspace.active
    if (!ws) return
    await gateway.cancel(ws.path)
    status.value = 'cancelled'
    approval.value = null
  }

  return {
    status,
    events,
    approval,
    contextUsage,
    contextNode,
    contextTotal,
    runId,
    running,
    lastEvent,
    nodeAudits,
    runningNodeId,
    run,
    respond,
    pause,
    resume,
    cancel,
  }
})