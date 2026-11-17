import type { EvidenceAccess, EvidenceRequest, EvidenceResponse } from '../src/types/memoryEvidence'
import type { RecallTrace, RecallTraceDetail, RecallTraceSummary } from '../src/types/memoryRecallTrace'
import type { MemoryRetrievalResponse } from '../src/types/memory'
import type { EvaluationCaseResult, EvaluationDatasetSummary, EvaluationPlan, EvaluationReport, EvaluationRunStatus } from '../src/types/memoryEvaluation'

// Browser-only, synthetic transport. Never delegates to a native IPC implementation.
export const CURRENT_TRACE = 'synthetic-trace-current'
const epoch = new Date().toISOString()
const future = () => new Date(Date.now() + 600_000).toISOString()
const clone = <T,>(value: T): T => structuredClone(value)
function fail(message: string): never { throw new Error(`[synthetic IPC] ${message}`) }
const body = (id: string) => `合成来源 ${id}。Unicode 码点测试：星轨🌌、中文、é、é。ORBIT_KEY=synthetic-orbit-7321；这只是可见的测试标记。\n${'继续读取合成记录，核对窗口、指纹与额度。🛰️ Orbit continuation.\n'.repeat(12)}`
const sha256 = async (text: string) => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))), byte => byte.toString(16).padStart(2, '0')).join('')

function trace(requestId: string, old = false, rerun = false): RecallTraceDetail {
  const ids = rerun ? ['synthetic-memory-b', 'synthetic-memory-a', 'synthetic-memory-c'] : ['synthetic-memory-a', 'synthetic-memory-b']
  const value: RecallTrace = {
    schema_version: 1, request_id: requestId, created_at: old ? new Date(Date.now() - 3_600_000).toISOString() : epoch,
    entrypoint: 'synthetic-browser-verification', query: 'ORBIT_KEY 星轨来源',
    arguments: { query: 'ORBIT_KEY 星轨来源', limit: 3, domain_mode: 'manual', domains: ['synthetic-orbit'] },
    routing: { active_domains: ['synthetic-orbit'], mode: 'manual', confidence: null, fallback: null },
    retrieval: { request_id: requestId, trace_status: 'saved', mode: 'hybrid', budget_ms: 12000, injected_chars: 180, semantic_candidates: 3, local_keyword_candidates: 2, selected_count: ids.length, unique_candidates: 3, discarded_candidates: 0, index: { total: 3, cached: 3, pending: 0 }, ranking_version: 'synthetic-v1', timings_ms: { routing: 0, keyword: 2, semantic: 4, selection: 1, total: 7 } },
    candidates: ids.map((memory_id, i) => ({ route: 'semantic', memory_id, input_rank: i + 1, eligible_rank: i + 1, score: 0.9 - i * 0.1, status: 'accepted' })),
    ranking: ids.map((memory_id, i) => ({ memory_id, domains: ['synthetic-orbit'], semantic_rank: i + 1, keyword_rank: i + 1, semantic_score: 0.9 - i * 0.1, keyword_score: 2 - i * 0.1, fused_score: 0.03 - i * 0.001, selected_rank: i + 1, diversity_penalty: 0, selection_score: 0.03 - i * 0.001, outcome: 'selected' })),
    results: ids.map(memory_id => ({ memory_id, fingerprint: (rerun && memory_id.endsWith('-a') ? 'b' : 'a').repeat(64), domains: ['synthetic-orbit'], source_event_key: `synthetic-event-${memory_id}`, source_agent: 'synthetic', session_id: 'synthetic-session', event_time: epoch, updated_at: epoch })), truncated: false,
  }
  return { trace: value, current_memories: ids.map(memory_id => ({ memory_id, state: rerun && memory_id.endsWith('-a') ? 'changed' : 'unchanged', content: `合成记忆 ${memory_id}：ORBIT_KEY 星轨定位。`, domains: ['synthetic-orbit'], source_event_key: `synthetic-event-${memory_id}`, source_agent: 'synthetic', session_id: 'synthetic-session' })) }
}

const traces = new Map([[CURRENT_TRACE, trace(CURRENT_TRACE)], ['synthetic-trace-old', trace('synthetic-trace-old', true)]])
const contexts = new Map<string, { access: EvidenceAccess; ids: string[]; remaining: number }>()
let serial = 0
function newAccess(requestId: string): EvidenceAccess {
  const context_token = (++serial).toString(16).padStart(64, '0')
  const access = { request_id: requestId, status: 'ready', context_token, expires_at: future(), max_chars: 1200 }
  contexts.set(context_token, { access, ids: traces.get(requestId)!.trace.results.map(item => item.memory_id), remaining: 1200 })
  return access
}
export const initialAccess = newAccess(CURRENT_TRACE)
// A capability can be valid even when its auxiliary trace was never saved.
const unsaved = trace('synthetic-trace-unsaved')
traces.set(unsaved.trace.request_id, unsaved)
export const unsavedResponse: MemoryRetrievalResponse = {
  query: unsaved.trace.query, routing: unsaved.trace.routing,
  retrieval: { ...unsaved.trace.retrieval, trace_status: 'unavailable' },
  evidence_access: newAccess(unsaved.trace.request_id),
  memories: unsaved.current_memories.map(memory => ({ id: memory.memory_id, content: memory.content ?? '', domains: memory.domains })),
}
traces.delete(unsaved.trace.request_id)
const cursors = new Map<string, { token: string; ids: string; query: string; offset: number }>()
const dataset: EvaluationDatasetSummary = { dataset_id: 'synthetic-orbit-dataset', name: 'Synthetic ORBIT UI fixture', description: '固定合成 UI 状态；不执行模型、不连接记忆库。', synthetic: true, memory_count: 3, case_count: 3, development_count: 3, holdout_count: 0 }
const backend = { embedding_model: 'synthetic-local-embedding (no model execution)', embedding_location: 'local', routing_model: null, routing_provider: 'synthetic-local', routing_remote: false }
const plans = new Map<string, EvaluationPlan>()
const runs = new Map<string, { plan: EvaluationPlan; status: EvaluationRunStatus; polls: number; started: string; finished: string | null }>()
let latestRun: string | null = null

export const fixture = {
  label: 'synthetic IPC / isolated UI verification',
  counts: {} as Record<string, number>,
  calls: [] as { method: string; args: unknown; state: string }[],
  evidenceDelayMs: 1800,
  cancelledEvidenceRequests: [] as string[],
  resolvedEvidenceRequests: [] as string[],
  latestStatus: null as EvaluationRunStatus | null,
  latestTraceId: CURRENT_TRACE,
  errors: [] as string[],
}

function reportFor(run: typeof runs extends Map<string, infer T> ? T : never): EvaluationReport {
  const { plan, status } = run
  const executed = status.completed
  const cases: EvaluationCaseResult[] = ['success', 'failed', 'not_run'].map((kind, i) => {
    const outcome = i >= executed ? 'not_run' : kind
    const success = outcome === 'success'
    return { case_id: `synthetic-case-${i + 1}`, category: 'synthetic-orbit', split: 'development', query: ['ORBIT_KEY 星轨来源', 'Synthetic failure example', 'Synthetic not-run example'][i], status: outcome, error: outcome === 'failed' ? 'Synthetic retrieval failure; no backend called.' : null, expected_ids: ['synthetic-memory-a'], forbidden_ids: ['synthetic-forbidden'], returned_ids: success ? ['synthetic-memory-a'] : [], expected_domain_ids: ['synthetic-orbit'], active_domains: success ? ['synthetic-orbit'] : [], metrics: { recall_at_k: success ? 1 : null, mrr: success ? 1 : null, ndcg_at_k: success ? 1 : null, duplicate_rate: 0, forbidden_rate: 0, false_positive_on_empty_query: false }, domain_hit: success ? true : null, degraded: false, index_complete: success, retrieval: success ? { budget_ms: 12000, injected_chars: 80, semantic_candidates: 3 } : {}, elapsed_ms: success ? 7 : 0, returned_chars: success ? 80 : 0, evidence_coverage: null, evidence_status_hit: null, evidence_ms: null }
  })
  const successes = cases.filter(item => item.status === 'success').length
  const failures = cases.filter(item => item.status === 'failed').length
  return { schema_version: 1, run_id: status.run_id, dataset_id: dataset.dataset_id, dataset_name: dataset.name, synthetic: true, dataset_fingerprint: 'd'.repeat(64), split: plan.split, mode: plan.mode, status: status.status, planned_count: 3, started_at: run.started, finished_at: run.finished, preparation_ms: 3, backend, cases, summary: { planned_count: 3, executed_count: executed, not_run_count: 3 - executed, successful_count: successes, failed_count: failures, degraded_count: 0, partial_index_count: 0, answerable_count: successes, unanswerable_count: 0, recall_at_k: successes ? 1 : null, mrr: successes ? 1 : null, ndcg_at_k: successes ? 1 : null, duplicate_rate: successes ? 0 : null, forbidden_rate: successes ? 0 : null, false_positive_rate: null, domain_accuracy: successes ? 1 : null, p50_ms: successes ? 7 : null, p95_ms: successes ? 7 : null, mean_returned_chars: successes ? 80 : null, evidence_coverage: null, evidence_status_accuracy: null } }
}

async function dispatch(method: string, args: Record<string, unknown>): Promise<unknown> {
  switch (method) {
    case 'memory_recall_trace_list': return [...traces.values()].reverse().map(({ trace: item }): RecallTraceSummary => ({ request_id: item.request_id, created_at: item.created_at, entrypoint: item.entrypoint, query: `${item.request_id === 'synthetic-trace-old' ? '[旧历史]' : '[当前/重跑]'} ${item.query}`, active_domains: item.routing.active_domains ?? [], mode: 'hybrid', selected_count: item.results.length, total_ms: 7, truncated: false })).sort((a, b) => b.created_at.localeCompare(a.created_at)).slice(0, Number(args.limit ?? 30))
    case 'memory_recall_trace_get': return traces.get(String(args.requestId)) ?? null
    case 'memory_retrieval_search': {
      const original = args.arguments as RecallTrace['arguments']
      if (!original || typeof original.query !== 'string') fail('search requires stored arguments')
      const requestId = `synthetic-trace-rerun-${++serial}`
      const next = trace(requestId, false, true)
      next.trace.arguments = clone(original)
      next.trace.query = original.query
      next.trace.created_at = new Date().toISOString()
      next.trace.retrieval.budget_ms = typeof args.budgetMs === 'number' ? args.budgetMs : 12000
      traces.set(requestId, next)
      fixture.latestTraceId = requestId
      const response: MemoryRetrievalResponse = { query: original.query, routing: next.trace.routing, retrieval: next.trace.retrieval, evidence_access: newAccess(requestId), memories: next.current_memories.map(item => ({ id: item.memory_id, content: item.content ?? '', domains: item.domains })) }
      return response
    }
    case 'memory_evidence_read': {
      const input = args.arguments as EvidenceRequest
      const context = contexts.get(input?.context_token)
      if (!context) fail('invalid synthetic context')
      if (Date.parse(context.access.expires_at!) <= Date.now()) fail('synthetic context expired')
      if (!input.memory_ids?.length || input.memory_ids.length > 2 || input.memory_ids.some(id => !context.ids.includes(id))) fail('forbidden synthetic evidence scope')
      const ids = JSON.stringify(input.memory_ids)
      const cursor = input.cursor ? cursors.get(input.cursor) : null
      if (input.cursor && (!cursor || cursor.token !== input.context_token || cursor.ids !== ids || cursor.query !== (input.query ?? ''))) fail('invalid synthetic cursor')
      const offset = cursor?.offset ?? 0
      const limit = Math.min(Number(input.max_chars ?? 4000), context.remaining, 240)
      const perSource = Math.floor(limit / input.memory_ids.length)
      const items = await Promise.all(input.memory_ids.map(async memory_id => {
        const text = body(memory_id)
        const chars = Array.from(text)
        const excerpt = chars.slice(offset, offset + perSource).join('')
        return { memory_id, status: 'ok', sources: [{ source_event_key: `synthetic-event-${memory_id}`, source_agent: 'synthetic', session_id: 'synthetic-session', occurred_at: epoch, source_fingerprint: await sha256(text), status: 'available', excerpt: { text: excerpt, char_start: offset, char_end: offset + Array.from(excerpt).length, total_chars: chars.length, match_method: cursor ? 'continuation' : 'lexical', excerpt_fingerprint: await sha256(excerpt) } }] }
      }))
      const returned = items.reduce((sum, item) => sum + Array.from(item.sources[0].excerpt.text).length, 0)
      context.remaining -= returned
      const more = context.remaining > 0 && items.some(item => item.sources[0].excerpt.char_end < item.sources[0].excerpt.total_chars)
      const next_cursor = more ? `synthetic-cursor-${++serial}` : null
      if (next_cursor) cursors.set(next_cursor, { token: input.context_token, ids, query: input.query ?? '', offset: offset + perSource })
      const response: EvidenceResponse = { request_id: String(args.requestId), context_request_id: context.access.request_id, status: returned ? 'partial' : 'budget_exhausted', expires_at: context.access.expires_at, returned_chars: returned, remaining_chars: context.remaining, partial: more, items, next_cursor, instruction: 'Synthetic UI fixture only; offsets count Unicode code points; SHA-256 fingerprints cover the displayed synthetic source/excerpt.' }
      // Intentionally resolve even after cancel/unmount, to exercise component sequence guards.
      await new Promise(resolve => setTimeout(resolve, fixture.evidenceDelayMs))
      fixture.resolvedEvidenceRequests.push(String(args.requestId))
      return response
    }
    case 'memory_evidence_cancel': fixture.cancelledEvidenceRequests.push(String(args.requestId)); return true
    case 'plugin:dialog|open':
    case 'plugin:dialog|save': return null
    case 'memory_retrieval_eval_datasets': return [dataset]
    case 'memory_retrieval_eval_import': return fail('file import unsupported in isolated fixture; dialog returns null')
    case 'memory_retrieval_eval_export': return fail('file export unsupported in isolated fixture; dialog returns null')
    case 'memory_retrieval_eval_plan': {
      if (args.datasetId !== dataset.dataset_id) fail('unknown synthetic dataset')
      if (!['development', 'holdout', 'all'].includes(String(args.split)) || !['hybrid', 'keyword'].includes(String(args.mode))) fail('invalid plan split/mode')
      const plan: EvaluationPlan = { plan_id: `synthetic-plan-${++serial}`, dataset_id: dataset.dataset_id, dataset_name: dataset.name, synthetic: true, dataset_fingerprint: 'd'.repeat(64), split: args.split as EvaluationPlan['split'], mode: args.mode as EvaluationPlan['mode'], memory_count: 3, case_count: 3, embedding_batches: args.mode === 'keyword' ? 0 : 1, query_calls: args.mode === 'keyword' ? 0 : 3, routing_calls: 0, remote_calls_estimate: 0, budget_ms: Number(args.budgetMs), backend, expires_at: future() }
      plans.set(plan.plan_id, plan)
      return plan
    }
    case 'memory_retrieval_eval_start': {
      if (args.confirmed !== true) fail('confirmed=true required')
      const plan = plans.get(String(args.planId))
      if (!plan || Date.parse(plan.expires_at) <= Date.now()) fail('missing or expired synthetic plan')
      if ([...runs.values()].some(run => ['preparing', 'running', 'cancelling'].includes(run.status.status))) fail('synthetic run already active')
      latestRun = `synthetic-run-${++serial}`
      const status: EvaluationRunStatus = { run_id: latestRun, status: 'preparing', completed: 0, total: 3, prepared: 0, total_memories: 3, error: null }
      runs.set(latestRun, { plan, status, polls: 0, started: new Date().toISOString(), finished: null })
      fixture.latestStatus = clone(status)
      plans.delete(plan.plan_id)
      return status
    }
    case 'memory_retrieval_eval_status': {
      const id = args.runId == null ? latestRun : String(args.runId)
      const run = id ? runs.get(id) : null
      if (!run) return null
      if (run.status.status === 'cancelling') { run.status.status = 'cancelled'; run.finished = new Date().toISOString() }
      else if (['preparing', 'running'].includes(run.status.status)) {
        run.polls++
        run.status.prepared = Math.min(3, run.polls)
        if (run.polls >= 2) { run.status.status = 'running'; run.status.completed = 1 }
        if (run.polls >= 6 && new URLSearchParams(location.search).get('scenario') !== 'cancel') { run.status.status = 'completed'; run.status.completed = 2; run.finished = new Date().toISOString() }
      }
      fixture.latestStatus = clone(run.status)
      return run.status
    }
    case 'memory_retrieval_eval_cancel': {
      const run = runs.get(String(args.runId))
      if (!run) fail('unknown synthetic run')
      if (['preparing', 'running'].includes(run.status.status)) run.status.status = 'cancelling'
      fixture.latestStatus = clone(run.status)
      return run.status
    }
    case 'memory_retrieval_eval_report': {
      const run = runs.get(String(args.runId))
      if (!run) fail('unknown synthetic report')
      return reportFor(run)
    }
    default: return fail(`Unsupported invoke: ${method}`)
  }
}

export function installFixture() {
  const target = window as unknown as { __TAURI_INTERNALS__: { invoke: typeof syntheticInvoke }; __memoryFixture: typeof fixture }
  async function syntheticInvoke<T>(method: string, args: Record<string, unknown> = {}): Promise<T> {
    fixture.counts[method] = (fixture.counts[method] ?? 0) + 1
    const call = { method, args: clone(args), state: 'pending' }
    fixture.calls.push(call)
    try {
      const result = await dispatch(method, args)
      call.state = 'resolved'
      return clone(result) as T
    } catch (error) {
      call.state = 'rejected'
      const message = error instanceof Error ? error.message : String(error)
      fixture.errors.push(message)
      console.error(message)
      throw error
    }
  }
  target.__TAURI_INTERNALS__ = { invoke: syntheticInvoke }
  target.__memoryFixture = fixture
}
